//! Writing a table's data at a checkpoint (DuckDB's `SingleFileTableDataWriter`,
//! `RowGroup::WriteToDisk/Checkpoint`, `ColumnCheckpointState`, `PartialBlockManager` and
//! `WriteOverflowStringsToDisk`).
//!
//! Every column of every row group is checkpointed by the
//! [column checkpointer](super::column_checkpointer), which picks a codec per column and
//! writes its segments here: segments whose statistics show a single value become
//! constant segments without a block; small segments share blocks.

use std::collections::BTreeMap;
use std::sync::Arc;

use rustdb_buffer::{BlockId, MAXIMUM_BLOCK};
use rustdb_common::{Error, Result};
use rustdb_serde::binary::BinarySerializer;
use rustdb_serde::{SerializationOptions, Serializer};

use super::column_checkpointer::{CheckpointContext, checkpoint_column, merge_persistent_statistics};
use super::pointers::{BlockPointer, RowGroupPointer};
use crate::checkpoint::pointers::PersistentColumnData;
use crate::compression::CompressionInfo;
use crate::compression::uncompressed::{
    BIG_STRING_MARKER_SIZE, DICTIONARY_HEADER_SIZE, read_overflow_string_bytes,
};
use crate::data_table::DataTable;
use crate::metadata::{MetaBlockPointer, MetadataManager, MetadataWriter};
use crate::single_file_block_manager::SingleFileBlockManager;
use crate::statistics::{BaseStatistics, serialize_table_statistics};
use crate::table::RowGroup;
use crate::table::column_segment::{ColumnSegment, Layout};

/// The writer of data blocks of a checkpoint (DuckDB's `PartialBlockManager` for full
/// checkpoints): segments smaller than 80% of a block share blocks.
#[derive(Debug)]
pub struct BlockWriter {
    manager: Arc<SingleFileBlockManager>,
    /// The block being filled: its id, contents and used size.
    partial: Option<(BlockId, Vec<u8>, usize)>,
    /// Every data block this checkpoint references, with the number of references.
    pub used_blocks: BTreeMap<BlockId, u32>,
}

impl BlockWriter {
    pub fn new(manager: Arc<SingleFileBlockManager>) -> Self {
        Self {
            manager,
            partial: None,
            used_blocks: BTreeMap::new(),
        }
    }

    pub(crate) fn block_size(&self) -> usize {
        rustdb_buffer::BlockManager::block_size(self.manager.as_ref())
    }

    /// What the codecs of this checkpoint need to know: the block size, the storage version
    /// written and the compression settings.
    pub(crate) fn compression_info(&self) -> CompressionInfo {
        CompressionInfo {
            block_size: self.block_size(),
            storage_version: self.manager.storage_version(),
            config: self.manager.compression_config(),
        }
    }

    /// A new block id for checkpoint data.
    pub(crate) fn new_block(&mut self) -> Result<BlockId> {
        let id = self.manager.get_free_block_id_for_checkpoint()?;
        self.used_blocks.insert(id, 1);
        Ok(id)
    }

    /// Writes a block, zero-padding it to the block size.
    pub(crate) fn write_block(&self, block_id: BlockId, mut data: Vec<u8>) -> Result<()> {
        data.resize(self.block_size(), 0);
        self.manager.write_block(block_id, &data)
    }

    /// Stores the bytes of a segment and returns where (`GetBlockAllocation`).
    pub fn write_segment(&mut self, data: &[u8]) -> Result<BlockPointer> {
        let block_size = self.block_size();
        if data.len() > block_size {
            return Err(Error::internal(
                "segment size exceeds block size in ColumnCheckpointState::FlushSegmentInternal",
            ));
        }
        if data.len() > block_size / 5 * 4 {
            let block_id = self.new_block()?;
            self.write_block(block_id, data.to_vec())?;
            return Ok(BlockPointer { block_id, offset: 0 });
        }
        if let Some((block_id, buffer, used)) = &mut self.partial {
            let start = used.next_multiple_of(8);
            if start + data.len() <= block_size {
                buffer.resize(start, 0);
                buffer.extend_from_slice(data);
                *used = start + data.len();
                *self.used_blocks.entry(*block_id).or_insert(0) += 1;
                return Ok(BlockPointer {
                    block_id: *block_id,
                    offset: u32::try_from(start).map_err(|_| Error::internal("block offset overflow"))?,
                });
            }
        }
        self.flush()?;
        let block_id = self.new_block()?;
        self.partial = Some((block_id, data.to_vec(), data.len()));
        Ok(BlockPointer { block_id, offset: 0 })
    }

    /// Writes the partially filled block (`FlushPartialBlocks`).
    pub fn flush(&mut self) -> Result<()> {
        if let Some((block_id, buffer, _)) = self.partial.take() {
            self.write_block(block_id, buffer)?;
        }
        Ok(())
    }
}

/// Writes big strings to overflow blocks (`WriteOverflowStringsToDisk`): `[length][bytes]`,
/// a full block ending in the id of the block the string continues in.
struct OverflowWriter {
    block: Option<(BlockId, Vec<u8>)>,
    offset: usize,
    /// The blocks written, in order (the segment state).
    blocks: Vec<BlockId>,
}

impl OverflowWriter {
    fn new() -> Self {
        Self {
            block: None,
            offset: 0,
            blocks: Vec::new(),
        }
    }

    fn allocate_new_block(&mut self, writer: &mut BlockWriter) -> Result<()> {
        let new_block = writer.new_block()?;
        let string_space = writer.block_size() - 8;
        if let Some((_, buffer)) = &mut self.block {
            buffer.resize(string_space, 0);
            buffer.extend_from_slice(&new_block.to_le_bytes());
            self.flush(writer)?;
        }
        self.block = Some((new_block, Vec::with_capacity(writer.block_size())));
        self.offset = 0;
        self.blocks.push(new_block);
        Ok(())
    }

    fn write_string(&mut self, writer: &mut BlockWriter, bytes: &[u8]) -> Result<(BlockId, i32)> {
        let string_space = writer.block_size() - 8;
        if self.block.is_none() || self.offset + 8 >= string_space {
            self.allocate_new_block(writer)?;
        }
        let (block_id, _) = self
            .block
            .as_ref()
            .ok_or_else(|| Error::internal("no overflow block"))?;
        let location = (
            *block_id,
            i32::try_from(self.offset).map_err(|_| Error::internal("offset overflow"))?,
        );
        self.append(&(bytes.len() as u32).to_le_bytes());
        let mut remaining = bytes;
        while !remaining.is_empty() {
            let to_write = remaining.len().min(string_space - self.offset);
            if to_write > 0 {
                self.append(&remaining[..to_write]);
                remaining = &remaining[to_write..];
            }
            if !remaining.is_empty() {
                self.allocate_new_block(writer)?;
            }
        }
        Ok(location)
    }

    fn append(&mut self, bytes: &[u8]) {
        if let Some((_, buffer)) = &mut self.block {
            buffer.resize(self.offset, 0);
            buffer.extend_from_slice(bytes);
            self.offset += bytes.len();
        }
    }

    fn flush(&mut self, writer: &BlockWriter) -> Result<()> {
        if let Some((block_id, buffer)) = self.block.take()
            && self.offset > 0
        {
            writer.write_block(block_id, buffer)?;
        }
        self.offset = 0;
        Ok(())
    }
}

/// The bytes of a transient segment to store on disk (`FinalizeAppend`) and its overflow
/// blocks: fixed-size values, validity masks per vector, or a string dictionary moved
/// next to its offsets when the segment is less than 80% full. Big strings are moved to
/// on-disk overflow blocks and their markers rewritten.
pub(crate) fn segment_bytes(
    segment: &ColumnSegment,
    writer: &mut BlockWriter,
) -> Result<(Vec<u8>, Vec<BlockId>)> {
    let count = segment.count() as usize;
    let buffer = segment.pin()?;
    match segment.layout() {
        Layout::Fixed(width) => {
            // SAFETY: the segment belongs to the checkpoint and is not shared.
            let bytes = unsafe { buffer.bytes(0, count * width)? };
            Ok((bytes.to_vec(), Vec::new()))
        }
        Layout::Validity => {
            let size = count.div_ceil(rustdb_common::STANDARD_VECTOR_SIZE)
                * (rustdb_common::STANDARD_VECTOR_SIZE / 8);
            // SAFETY: as above.
            let bytes = unsafe { buffer.bytes(0, size)? };
            Ok((bytes.to_vec(), Vec::new()))
        }
        Layout::Strings => {
            // SAFETY: as above.
            let (dictionary_size, dictionary_end) =
                unsafe { (buffer.read_u32(0)? as usize, buffer.read_u32(4)? as usize) };
            // SAFETY: as above.
            let offsets: Vec<i32> = unsafe { buffer.bytes(DICTIONARY_HEADER_SIZE, count * 4)? }
                .as_chunks::<4>()
                .0
                .iter()
                .map(|chunk| i32::from_le_bytes(*chunk))
                .collect();
            // SAFETY: as above.
            let mut dictionary =
                unsafe { buffer.bytes(dictionary_end - dictionary_size, dictionary_size)? }.to_vec();
            let mut overflow = OverflowWriter::new();
            let mut previous = 0i32;
            for &offset in &offsets {
                let is_marker = offset < 0 && offset.unsigned_abs() != previous.unsigned_abs();
                previous = offset;
                if !is_marker {
                    continue;
                }
                let position = dictionary_size - offset.unsigned_abs() as usize;
                let marker = &dictionary[position..position + BIG_STRING_MARKER_SIZE];
                let block_id = i64::from_le_bytes(marker[..8].try_into().unwrap_or_default());
                let block_offset = i32::from_le_bytes(marker[8..].try_into().unwrap_or_default());
                if block_id < MAXIMUM_BLOCK {
                    continue;
                }
                let string = read_overflow_string_bytes(segment, block_id, block_offset)?;
                let (new_block, new_offset) = overflow.write_string(writer, &string)?;
                dictionary[position..position + 8].copy_from_slice(&new_block.to_le_bytes());
                dictionary[position + 8..position + BIG_STRING_MARKER_SIZE]
                    .copy_from_slice(&new_offset.to_le_bytes());
            }
            overflow.flush(writer)?;
            let offset_size = DICTIONARY_HEADER_SIZE + count * 4;
            let total_size = offset_size + dictionary_size;
            let block_size = writer.block_size();
            let mut result;
            if total_size >= block_size / 5 * 4 {
                // Full enough: keep the dictionary at the end of the segment.
                // SAFETY: as above.
                result = unsafe { buffer.bytes(0, dictionary_end)? }.to_vec();
                result[dictionary_end - dictionary_size..].copy_from_slice(&dictionary);
            } else {
                result = Vec::with_capacity(total_size);
                result.extend_from_slice(&(dictionary_size as u32).to_le_bytes());
                result.extend_from_slice(&(total_size as u32).to_le_bytes());
                for offset in &offsets {
                    result.extend_from_slice(&offset.to_le_bytes());
                }
                result.extend_from_slice(&dictionary);
            }
            Ok((result, overflow.blocks))
        }
    }
}

/// Serializes `write` as one unit into a metadata writer (a `BinarySerializer` over the
/// stream, with `Begin`/`End`).
pub fn write_unit(
    writer: &mut MetadataWriter<'_>,
    options: &SerializationOptions,
    write: impl FnOnce(&mut dyn Serializer) -> Result<()>,
) -> Result<()> {
    let mut serializer = BinarySerializer::new(options.clone());
    serializer.begin();
    write(&mut serializer)?;
    serializer.end();
    writer.write_data(serializer.bytes())
}

/// The result of writing a table's data: what the catalog entry stores, and the written
/// columns of each row group, to replace the in-memory data once the checkpoint is
/// complete.
#[derive(Debug)]
pub struct TableDataPointer {
    pub table_pointer: MetaBlockPointer,
    pub total_rows: u64,
    pub persisted: Vec<PersistedRowGroup>,
}

/// The checkpointed columns of one row group.
#[derive(Debug)]
pub struct PersistedRowGroup {
    pub row_group: Arc<RowGroup>,
    pub count: u64,
    pub columns: Vec<PersistentColumnData>,
}

/// Replaces the in-memory columns of the checkpointed row groups by the persistent
/// segments just written (`RowGroup::Checkpoint` swapping in the `ColumnCheckpointState`
/// results), freeing the transient data. The caller guarantees that no transaction
/// writes to the table and none needs an older version of its data; row groups that
/// changed since they were written are kept.
pub fn replace_with_checkpointed(table: &DataTable, persisted: Vec<PersistedRowGroup>) -> Result<()> {
    if persisted.is_empty() {
        return Ok(());
    }
    // Appends to the main table wait (they hold the append lock while they write).
    let _append = table.append_lock();
    let collection = table.row_groups();
    let row_groups = collection.row_groups();
    let mut nodes = row_groups.tree().nodes();
    for written in persisted {
        let Some(node) = nodes
            .iter_mut()
            .find(|node| Arc::ptr_eq(&node.node, &written.row_group))
        else {
            continue;
        };
        if node.node.count() != written.count || node.node.column_count() != written.columns.len() {
            continue;
        }
        // The written columns are built again when first used.
        let types = written
            .columns
            .iter()
            .map(|data| data.logical_type.clone())
            .collect();
        node.node = Arc::new(node.node.with_persistent(
            Arc::clone(collection.io()),
            types,
            written.columns.clone(),
        ));
    }
    row_groups.tree().replace(nodes);
    Ok(())
}

/// Writes a table's committed data (`DataTable::Checkpoint` and `FinalizeTable`).
pub fn write_table_data(
    table: &DataTable,
    metadata: &MetadataManager,
    table_writer: &mut MetadataWriter<'_>,
    blocks: &mut BlockWriter,
    options: &SerializationOptions,
    checkpoint_id: u64,
) -> Result<TableDataPointer> {
    let collection = table.row_groups();
    let types = collection.types().to_vec();
    // DuckDB's `TableStatistics::InitializeEmpty(other)`: empty statistics, the distinct
    // statistics kept, and their count as the distinct count.
    let distinct = collection.distinct_statistics();
    let mut table_stats: Vec<BaseStatistics> = types
        .iter()
        .enumerate()
        .map(|(index, ty)| {
            let mut stats = BaseStatistics::create_empty(ty.clone());
            if let Some(Some(distinct)) = distinct.get(index) {
                stats.set_distinct_count(distinct.count());
            }
            stats
        })
        .collect();
    let mut row_group_pointers = Vec::new();
    let mut persisted = Vec::new();
    let row_groups = collection.row_groups();
    let base = row_groups.base_row_id();
    for node in row_groups.tree().nodes() {
        let row_group = &node.node;
        let count = row_group.count();
        if count == 0 {
            continue;
        }
        let row_group_start = node.row_start - base;
        let mut pointer = RowGroupPointer {
            row_start: row_group_start,
            tuple_count: count,
            ..RowGroupPointer::default()
        };
        let mut written_columns = Vec::with_capacity(row_group.column_count());
        for (index, column) in row_group.columns()?.iter().enumerate() {
            let mut context = CheckpointContext {
                info: blocks.compression_info(),
                writer: &mut *blocks,
                io: collection.io(),
            };
            let persistent = checkpoint_column(column, count, row_group_start, &mut context)?;
            if let Some(stats) = table_stats.get_mut(index) {
                merge_persistent_statistics(&persistent, stats);
            }
            pointer.data_pointers.push(table_writer.meta_block_pointer()?);
            write_unit(table_writer, options, |s| persistent.serialize_fields(s))?;
            written_columns.push(persistent);
        }
        persisted.push(PersistedRowGroup {
            row_group: Arc::clone(row_group),
            count,
            columns: written_columns,
        });
        if let Some(version_info) = row_group.version_info() {
            let deletes = version_info.checkpoint_deletes(checkpoint_id);
            if !deletes.is_empty() {
                let mut writer = MetadataWriter::new(metadata);
                writer.track_written_pointers();
                writer.write_u64(deletes.len() as u64)?;
                for (vector_index, deleted) in deletes {
                    writer.write_u64(vector_index as u64)?;
                    write_chunk_deletes(&mut writer, vector_index, deleted.as_deref())?;
                }
                writer.flush()?;
                pointer.deletes_pointers = writer.take_written_pointers();
            }
        }
        row_group_pointers.push(pointer);
    }
    blocks.flush()?;
    // `FinalizeTable`.
    let table_pointer = table_writer.meta_block_pointer()?;
    write_unit(table_writer, options, |s| {
        serialize_table_statistics(s, &table_stats, &distinct)
    })?;
    table_writer.write_u64(row_group_pointers.len() as u64)?;
    let mut total_rows = 0;
    for pointer in &row_group_pointers {
        total_rows = total_rows.max(pointer.row_start + pointer.tuple_count);
        write_unit(table_writer, options, |s| pointer.serialize_fields(s))?;
    }
    Ok(TableDataPointer {
        table_pointer,
        total_rows,
        persisted,
    })
}

/// `ChunkVectorInfo::Write`: the deletes of one vector; `deleted` is `None` when the
/// whole vector is deleted, otherwise a mask with a set bit per deleted row.
fn write_chunk_deletes(
    writer: &mut MetadataWriter<'_>,
    vector_index: usize,
    deleted: Option<&[u64]>,
) -> Result<()> {
    const CONSTANT_INFO: u8 = 0;
    const VECTOR_INFO: u8 = 1;
    let start = (vector_index * rustdb_common::STANDARD_VECTOR_SIZE) as u64;
    let Some(mask) = deleted else {
        writer.write_u8(CONSTANT_INFO)?;
        return writer.write_u64(start);
    };
    writer.write_u8(VECTOR_INFO)?;
    writer.write_u64(start)?;
    // `ValidityMask::Write` with valid == deleted.
    let count = rustdb_common::STANDARD_VECTOR_SIZE;
    let valid_values = mask.iter().map(|word| word.count_ones() as usize).sum::<usize>();
    let invalid_values = count - valid_values;
    let bitmask_bytes = count / 8;
    let valid_size = 2 * valid_values + 4;
    let invalid_size = 2 * invalid_values + 4;
    if valid_size < bitmask_bytes || invalid_size < bitmask_bytes {
        let serialize_valid = valid_size < invalid_size;
        writer.write_u8(if serialize_valid { 1 } else { 2 })?;
        writer.write_u32(valid_values.min(invalid_values) as u32)?;
        for row in 0..count {
            let is_valid = mask[row / 64] >> (row % 64) & 1 == 1;
            if is_valid == serialize_valid {
                writer.write_data(&(row as u16).to_le_bytes())?;
            }
        }
    } else {
        writer.write_u8(0)?;
        for word in mask {
            writer.write_u64(*word)?;
        }
    }
    Ok(())
}
