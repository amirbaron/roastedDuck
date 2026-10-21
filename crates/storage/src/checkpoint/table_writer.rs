//! Writing a table's data at a checkpoint (DuckDB's `SingleFileTableDataWriter`,
//! `RowGroup::WriteToDisk/Checkpoint`, `ColumnCheckpointState`, `PartialBlockManager` and
//! `WriteOverflowStringsToDisk`).
//!
//! Every column of every changed row group is checkpointed by the
//! [column checkpointer](super::column_checkpointer), which keeps unchanged persistent
//! segments and otherwise picks a codec per column and writes its segments here: segments
//! whose statistics show a single value become constant segments without a block; small
//! segments share blocks. Row groups without changes keep their metadata on disk
//! (`experimental_metadata_reuse`), and so do tables whose row groups all do.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use rustdb_buffer::{BlockId, MAXIMUM_BLOCK};
use rustdb_common::{Error, Result};
use rustdb_serde::binary::BinarySerializer;
use rustdb_serde::{SerializationOptions, Serializer};
use rustdb_types::LogicalType;

use super::column_checkpointer::{CheckpointContext, checkpoint_column, merge_persistent_statistics};
use super::pointers::{BlockPointer, DataPointer, PersistentColumnData, RowGroupPointer};
use super::stream::StreamDeserializer;
use crate::compression::uncompressed::{
    BIG_STRING_MARKER_SIZE, DICTIONARY_HEADER_SIZE, read_overflow_string_bytes,
};
use crate::compression::{CompressionInfo, CompressionType};
use crate::data_table::DataTable;
use crate::metadata::{MetaBlockPointer, MetadataManager, MetadataReader, MetadataWriter};
use crate::single_file_block_manager::SingleFileBlockManager;
use crate::statistics::{BaseStatistics, serialize_table_statistics};
use crate::table::column_segment::{ColumnSegment, Layout};
use crate::table::{
    PerColumnMetadataBlocks, RowGroup, RowGroupCollection, RowGroupMetadata, TableMetadataPointer,
};

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

    /// Counts the blocks of segments this checkpoint keeps from the previous one.
    pub(crate) fn add_reused_pointers(&mut self, pointers: &[DataPointer]) {
        for pointer in pointers {
            if pointer.block_pointer.is_valid() {
                *self
                    .used_blocks
                    .entry(pointer.block_pointer.block_id)
                    .or_insert(0) += 1;
            }
            for &block in &pointer.overflow_blocks {
                *self.used_blocks.entry(block).or_insert(0) += 1;
            }
        }
    }

    /// Counts every block of a column this checkpoint keeps from the previous one.
    pub(crate) fn add_reused_column(&mut self, data: &PersistentColumnData) {
        self.add_reused_pointers(&data.pointers);
        for child in &data.child_columns {
            self.add_reused_column(child);
        }
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

/// The checkpoint settings that decide how a table's metadata is written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TableWriteSettings {
    /// `experimental_metadata_reuse`: unchanged row groups and tables keep their
    /// metadata.
    pub metadata_reuse: bool,
    /// `force_column_metadata_reuse`: with storage version v1.4.0 or later, unchanged
    /// columns of changed row groups keep their metadata.
    pub force_column_metadata_reuse: bool,
    /// The serialization version written.
    pub storage_version: u64,
    /// Whether the checkpoint is a full one (`CheckpointType::FULL_CHECKPOINT`): no
    /// transaction needs older data, so deletes can be vacuumed and row ids change.
    pub full_checkpoint: bool,
    /// `max_vacuum_tasks`: the vacuum tasks a table checkpoint schedules at most.
    pub max_vacuum_tasks: u64,
    /// The start time of the oldest active transaction (`LowestActiveStart`), for the
    /// compression of version information.
    pub lowest_active_start: u64,
    /// `geometry_minimum_shredding_size` (`GeometryMinimumShreddingSize`).
    pub geometry_minimum_shredding_size: i64,
}

impl Default for TableWriteSettings {
    fn default() -> Self {
        Self {
            metadata_reuse: true,
            force_column_metadata_reuse: false,
            storage_version: crate::storage_info::DEFAULT_SERIALIZATION_VERSION,
            full_checkpoint: false,
            max_vacuum_tasks: 100,
            lowest_active_start: crate::transaction::TRANSACTION_ID_START,
            geometry_minimum_shredding_size: 30_000,
        }
    }
}

impl TableWriteSettings {
    /// `RowGroupCollection::SupportsPerColumnWrites`.
    pub fn supports_per_column_writes(&self) -> bool {
        self.storage_version >= 6 && self.force_column_metadata_reuse
    }

    /// `TableDataWriter::RequireLegacyStartRow`: storage versions before v1.4.4 store
    /// the start row of every segment.
    pub fn require_legacy_start_row(&self) -> bool {
        self.storage_version < 6
    }
}

/// How a checkpoint writes a row group (DuckDB's `RowGroupWriteAction`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RowGroupWriteAction {
    /// Nothing changed: the metadata on disk is kept.
    ReuseExistingRowGroupMetadata,
    /// The unchanged columns keep their metadata, the others are written.
    PartiallyReuseColumnMetadata,
    FullyCheckpointRowGroup,
}

/// What writing a row group's data produced (DuckDB's `RowGroupWriteData`).
#[derive(Debug)]
struct RowGroupWriteData {
    action: RowGroupWriteAction,
    /// Per column, its written segments, or `None` when its metadata is reused.
    columns: Vec<Option<PersistentColumnData>>,
    /// The extra metadata blocks of the reused columns (partial reuse).
    reused_extras: PerColumnMetadataBlocks,
}

/// The result of writing a table's data: what the catalog entry stores, and what the
/// in-memory table takes over once the checkpoint is complete.
#[derive(Debug)]
pub struct TableDataPointer {
    pub table_pointer: MetaBlockPointer,
    pub total_rows: u64,
    /// The written row groups.
    pub persisted: Vec<PersistedRowGroup>,
    /// Where the table data was written, or `None` when the table was unchanged and its
    /// metadata reused (`WriteUnchangedTable`).
    pub metadata_pointer: Option<TableMetadataPointer>,
    /// The statistics written (`RowGroupCollection::SetStats` after the checkpoint).
    pub statistics: Option<Vec<BaseStatistics>>,
    /// When row groups were vacuumed: the row groups the table consists of now, with
    /// their first row (relative to the collection's base row id), and its row count.
    pub vacuumed: Option<VacuumedLayout>,
}

/// The row groups a table consists of after a vacuum, each with its first row (relative to
/// the collection's base row id), and the table's row count.
pub type VacuumedLayout = (Vec<(Arc<RowGroup>, u64)>, u64);

/// The checkpointed columns of one row group.
#[derive(Debug)]
pub struct PersistedRowGroup {
    pub row_group: Arc<RowGroup>,
    pub count: u64,
    pub columns: Vec<PersistentColumnData>,
    /// Where the columns' metadata now is.
    pub metadata: RowGroupMetadata,
}

/// Hands the table what a completed checkpoint wrote (`RowGroupCollection::Checkpoint`
/// installing the new row groups and `FinalizeCheckpoint`).
///
/// With `replace_in_memory`, the in-memory columns of the written row groups are replaced
/// by the persistent segments, freeing the transient data, and the table takes the
/// written statistics; the caller guarantees that no transaction writes to the table and
/// none needs an older version of its data. Otherwise the row groups keep their data and
/// only learn where their metadata now is. Row groups that changed since they were
/// written forget their metadata, so it is written again.
pub fn finish_table_checkpoint(
    table: &DataTable,
    data: TableDataPointer,
    replace_in_memory: bool,
) -> Result<()> {
    // `DataTable::Checkpoint`: appends start a new row group, which a later vacuum can
    // merge, instead of changing a checkpointed one.
    table
        .row_groups()
        .set_append_mode(crate::table::RowGroupAppendMode::SuggestNew);
    let Some(metadata_pointer) = data.metadata_pointer else {
        // The table was unchanged.
        return Ok(());
    };
    // Appends to the main table wait (they hold the append lock while they write).
    let _append = table.append_lock();
    let collection = table.row_groups();
    collection.set_metadata_pointer(Some(metadata_pointer));
    if replace_in_memory && let Some(columns) = data.statistics {
        let mut stats = collection.statistics();
        stats.columns = columns;
        collection.set_statistics(stats);
    }
    if let Some((layout, total_rows)) = data.vacuumed {
        // Row groups were merged or dropped: the table is made of the checkpointed row
        // groups now (only full checkpoints vacuum, so `replace_in_memory` holds).
        let mut written: Vec<PersistedRowGroup> = data.persisted;
        let row_groups = collection.row_groups();
        let base = row_groups.base_row_id();
        let mut nodes = Vec::with_capacity(layout.len());
        for (row_group, row_start) in layout {
            let node = match written.iter().position(|w| Arc::ptr_eq(&w.row_group, &row_group)) {
                Some(position) => {
                    let w = written.swap_remove(position);
                    let types = column_types(collection, &w.columns);
                    Arc::new(row_group.with_persistent(
                        Arc::clone(collection.io()),
                        types,
                        w.columns,
                        w.metadata,
                    ))
                }
                None => row_group,
            };
            nodes.push(crate::table::SegmentNode {
                row_start: base + row_start,
                node,
            });
        }
        row_groups.tree().replace(nodes);
        collection.set_total_rows(total_rows);
        return Ok(());
    }
    if data.persisted.is_empty() {
        return Ok(());
    }
    let row_groups = collection.row_groups();
    let mut nodes = row_groups.tree().nodes();
    for written in data.persisted {
        let Some(node) = nodes
            .iter_mut()
            .find(|node| Arc::ptr_eq(&node.node, &written.row_group))
        else {
            continue;
        };
        if node.node.count() != written.count || node.node.column_count() != written.columns.len() {
            node.node.set_metadata(RowGroupMetadata::default());
            continue;
        }
        if !replace_in_memory {
            node.node.set_metadata(written.metadata);
            continue;
        }
        // The written columns are built again when first used.
        let types = column_types(collection, &written.columns);
        node.node = Arc::new(node.node.with_persistent(
            Arc::clone(collection.io()),
            types,
            written.columns,
            written.metadata,
        ));
    }
    row_groups.tree().replace(nodes);
    Ok(())
}

/// `RowGroup::ComputePerColumnMetadataBlocks`: per column, the metadata blocks its
/// `PersistentColumnData` spans besides its start block, found by reading it again.
fn compute_per_column_metadata_blocks(
    metadata: &MetadataManager,
    column_pointers: &[MetaBlockPointer],
    types: &[LogicalType],
) -> Result<PerColumnMetadataBlocks> {
    let mut result = PerColumnMetadataBlocks::default();
    for (index, (start, ty)) in column_pointers.iter().zip(types).enumerate() {
        let mut reader = MetadataReader::new(metadata, *start, false)?;
        reader.track_read_pointers();
        {
            let mut deserializer = StreamDeserializer::new(&mut reader);
            deserializer.begin()?;
            PersistentColumnData::deserialize_fields(&mut deserializer, ty)?;
            deserializer.end()?;
        }
        let extra: Vec<u64> = reader
            .take_read_pointers()
            .into_iter()
            .filter(|pointer| pointer.block_pointer != start.block_pointer)
            .map(|pointer| pointer.block_pointer)
            .collect();
        result.add_column(index, &extra);
    }
    Ok(result)
}

/// The segments of column `index` of a row group whose column is not rewritten: its
/// stored data when it was not built, otherwise its persistent segments.
fn reused_column_data(row_group: &RowGroup, index: usize) -> Result<PersistentColumnData> {
    if let Some(data) = row_group.unloaded_column_data(index)? {
        return Ok(data);
    }
    row_group.column(index)?.persistent_data(0)
}

/// What every row group of a table checkpoint shares (DuckDB's `RowGroupWriter` and the
/// `TableDataWriter`).
#[derive(Clone, Copy)]
struct TableWriteContext<'a> {
    collection: &'a RowGroupCollection,
    metadata: &'a MetadataManager,
    settings: TableWriteSettings,
    /// `RowGroupWriter::compression_types`: the declared compression of each column.
    compression_types: &'a [CompressionType],
    /// `schema.table`, for the log.
    table_name: &'a str,
}

/// `RowGroup::WriteToDisk`: decides how the row group is written and writes the data of
/// the columns that are written again.
fn write_row_group_data(
    table: &TableWriteContext<'_>,
    row_group: &RowGroup,
    row_group_start: u64,
    blocks: &mut BlockWriter,
    row_ids_changed: bool,
) -> Result<RowGroupWriteData> {
    let TableWriteContext {
        collection,
        metadata,
        settings,
        compression_types,
        table_name,
    } = *table;
    let count = row_group.count();
    let mut stored = row_group.metadata();
    // `CanReuseMetadata`: storage versions that store segment start rows cannot keep
    // metadata once row ids moved.
    let can_reuse_metadata = settings.metadata_reuse
        && !stored.column_pointers.is_empty()
        && !(settings.require_legacy_start_row() && row_ids_changed);
    let supports_per_column_writes = settings.supports_per_column_writes();
    let reuse = |reused_extras| RowGroupWriteData {
        action: RowGroupWriteAction::ReuseExistingRowGroupMetadata,
        columns: Vec::new(),
        reused_extras,
    };
    if can_reuse_metadata && !row_group.has_changes() {
        if supports_per_column_writes {
            if !stored.has_per_column_metadata_blocks {
                stored.per_column_metadata_blocks = compute_per_column_metadata_blocks(
                    metadata,
                    &stored.column_pointers,
                    collection.types(),
                )?;
                stored.has_per_column_metadata_blocks = true;
                row_group.set_metadata(stored);
            }
            return Ok(reuse(PerColumnMetadataBlocks::default()));
        }
        if stored.has_metadata_blocks {
            return Ok(reuse(PerColumnMetadataBlocks::default()));
        }
        if !stored.has_per_column_metadata_blocks {
            let blocks =
                compute_per_column_metadata_blocks(metadata, &stored.column_pointers, collection.types())?;
            stored.extra_metadata_blocks = blocks.blocks();
            stored.has_metadata_blocks = true;
            row_group.set_metadata(stored);
            return Ok(reuse(PerColumnMetadataBlocks::default()));
        }
        // The per-column metadata read from disk cannot be written anymore: the
        // metadata is written in one go.
    }
    let partial_reuse =
        can_reuse_metadata && stored.has_per_column_metadata_blocks && supports_per_column_writes;
    let action = if partial_reuse {
        RowGroupWriteAction::PartiallyReuseColumnMetadata
    } else {
        RowGroupWriteAction::FullyCheckpointRowGroup
    };
    let mut columns = Vec::with_capacity(row_group.column_count());
    let mut reused_columns = Vec::new();
    for index in 0..row_group.column_count() {
        let reused = partial_reuse
            && (!row_group.column_is_loaded(index) || !row_group.column(index)?.has_any_changes());
        if reused {
            reused_columns.push(index);
            columns.push(None);
            continue;
        }
        let column = row_group.column(index)?;
        let mut context = CheckpointContext {
            info: blocks.compression_info(),
            writer: &mut *blocks,
            io: collection.io(),
            column_compression: compression_types
                .get(index)
                .copied()
                .unwrap_or(CompressionType::Auto),
            table_name,
            storage_version: settings.storage_version,
            geometry_minimum_shredding_size: settings.geometry_minimum_shredding_size,
        };
        columns.push(Some(checkpoint_column(
            column,
            count,
            row_group_start,
            &mut context,
        )?));
    }
    let mut reused_extras = PerColumnMetadataBlocks::default();
    if partial_reuse {
        let extras = stored
            .per_column_metadata_blocks
            .blocks_for_columns(&reused_columns);
        for (index, blocks) in reused_columns.iter().zip(extras) {
            reused_extras.add_column(*index, &blocks);
        }
    }
    Ok(RowGroupWriteData {
        action,
        columns,
        reused_extras,
    })
}

/// Merges the statistics of the columns of a row group whose data is not written again
/// into `stats`: built columns contribute their statistics, the others are recorded in
/// `incomplete` (`SetHasUnloadedColumn`), their statistics are the table's.
/// The types of the columns of a written row group: the table's, as the stored data of a
/// GEOMETRY column has the type of its storage layout.
fn column_types(
    collection: &RowGroupCollection,
    columns: &[PersistentColumnData],
) -> Arc<[rustdb_types::LogicalType]> {
    let types = collection.types();
    if types.len() == columns.len() {
        return types.iter().cloned().collect();
    }
    columns.iter().map(|data| data.logical_type.clone()).collect()
}

fn merge_kept_column_statistics(
    row_group: &RowGroup,
    index: usize,
    stats: &mut BaseStatistics,
    incomplete: &mut BTreeSet<usize>,
) -> Result<()> {
    if row_group.column_is_loaded(index) {
        row_group.column(index)?.merge_into_statistics(stats)
    } else {
        incomplete.insert(index);
        Ok(())
    }
}

/// Writes a table's committed data (`DataTable::Checkpoint`, `RowGroupCollection::Checkpoint`
/// and `SingleFileTableDataWriter::FinalizeTable`): first the data of every row group,
/// then, unless the whole table is unchanged and keeps its metadata, the metadata of the
/// row groups, the table statistics and the row group pointers.
pub fn write_table_data(
    table: &DataTable,
    metadata: &MetadataManager,
    table_writer: &mut MetadataWriter<'_>,
    blocks: &mut BlockWriter,
    options: &SerializationOptions,
    checkpoint_id: u64,
    settings: TableWriteSettings,
) -> Result<TableDataPointer> {
    let collection = table.row_groups();
    let types = collection.types().to_vec();
    // `RowGroupWriter::compression_types`: the declared compression of each column.
    let compression_types: Vec<CompressionType> =
        table.columns().iter().map(|column| column.compression).collect();
    let table_name = format!("{}.{}", table.info().schema_name(), table.info().table_name());
    let row_groups = collection.row_groups();
    let nodes = row_groups.tree().nodes();
    let segment_count = nodes.len();
    // `CollectionCheckpointState`: the row group at each position, `None` once dropped.
    let mut segments: Vec<Option<Arc<RowGroup>>> =
        nodes.iter().map(|node| Some(Arc::clone(&node.node))).collect();
    let mut vacuum = initialize_vacuum_state(table, &nodes, &mut segments, settings);
    let mut dropped_any = segments.iter().any(Option::is_none);
    // The data of every row group (the checkpoint tasks), with the first row each has
    // once the row groups before it were vacuumed.
    let mut written: Vec<(Arc<RowGroup>, u64, RowGroupWriteData)> = Vec::with_capacity(segment_count);
    let mut new_total_rows = 0u64;
    let mut total_vacuum_tasks = 0u64;
    let context = TableWriteContext {
        collection,
        metadata,
        settings,
        compression_types: &compression_types,
        table_name: &table_name,
    };
    let write = |row_group: &Arc<RowGroup>,
                 row_start: u64,
                 row_ids_changed: bool,
                 blocks: &mut BlockWriter|
     -> Result<RowGroupWriteData> {
        write_row_group_data(&context, row_group, row_start, blocks, row_ids_changed)
    };
    for segment_idx in 0..segment_count {
        let schedule = total_vacuum_tasks < settings.max_vacuum_tasks;
        match schedule_vacuum(
            &mut vacuum,
            &segments,
            segment_idx,
            schedule,
            collection.row_group_size(),
        ) {
            VacuumDecision::Merge {
                merge_count,
                target_count,
                merge_rows,
            } => {
                total_vacuum_tasks += 1;
                vacuum.row_ids_changed = true;
                dropped_any = true;
                // `VacuumTask`: the merged row groups become `target_count` new ones at
                // the position of the first.
                let targets = vacuum_row_groups(
                    collection,
                    &vacuum,
                    &mut segments,
                    segment_idx,
                    merge_count,
                    target_count,
                    merge_rows,
                )?;
                for (offset, target) in targets.into_iter().enumerate() {
                    segments[segment_idx + offset] = Some(Arc::clone(&target));
                    let data = write(&target, new_total_rows, true, blocks)?;
                    let count = target.count();
                    written.push((target, new_total_rows, data));
                    new_total_rows += count;
                }
                continue;
            }
            VacuumDecision::Skip => {
                total_vacuum_tasks += 1;
                vacuum.row_ids_changed = true;
                continue;
            }
            VacuumDecision::Keep => {}
        }
        let Some(row_group) = segments[segment_idx].clone() else {
            continue;
        };
        // The row group is kept as it is: try to compress its version information.
        if let Some(version_info) = row_group.version_info() {
            version_info.compress_version_ids(settings.lowest_active_start);
        }
        let data = write(&row_group, new_total_rows, vacuum.row_ids_changed, blocks)?;
        let count = row_group.count();
        written.push((row_group, new_total_rows, data));
        new_total_rows += count;
    }
    let supports_per_column_writes = settings.supports_per_column_writes();
    // An unchanged table that exists on disk keeps its metadata.
    if settings.metadata_reuse
        && let Some(existing) = collection.metadata_pointer()
        && !dropped_any
        && written
            .iter()
            .all(|(_, _, data)| data.action == RowGroupWriteAction::ReuseExistingRowGroupMetadata)
    {
        for (row_group, _, _) in &written {
            let stored = row_group.metadata();
            metadata.clear_modified_blocks(&stored.column_pointers)?;
            metadata.clear_modified_blocks(&stored.extra_metadata_block_pointers())?;
            if let Some(version_info) = row_group.version_info() {
                metadata.clear_modified_blocks(&version_info.storage_pointers())?;
            }
            for index in 0..row_group.column_count() {
                blocks.add_reused_column(&reused_column_data(row_group, index)?);
            }
        }
        // `WriteUnchangedTable` and `FinalizeTable`: the blocks of the statistics and
        // row group pointers stay in use.
        metadata.clear_modified_blocks(&existing.blocks)?;
        return Ok(TableDataPointer {
            table_pointer: existing.pointer,
            total_rows: collection.total_rows(),
            persisted: Vec::new(),
            metadata_pointer: None,
            statistics: None,
            vacuumed: None,
        });
    }
    // `TableStatistics::InitializeEmpty(other)`: empty statistics, the distinct
    // statistics kept, and their count as the distinct count.
    let previous = collection.statistics();
    let mut table_stats: Vec<BaseStatistics> = types
        .iter()
        .enumerate()
        .map(|(index, ty)| {
            let mut stats = BaseStatistics::create_empty(ty.clone());
            if let Some(Some(distinct)) = previous.distinct.get(index) {
                stats.set_distinct_count(distinct.count());
            }
            stats
        })
        .collect();
    let mut incomplete = BTreeSet::new();
    let mut row_group_pointers = Vec::with_capacity(written.len());
    let mut persisted = Vec::new();
    let layout: Vec<(Arc<RowGroup>, u64)> = written
        .iter()
        .map(|(row_group, row_start, _)| (Arc::clone(row_group), *row_start))
        .collect();
    for (row_group, row_start, data) in written {
        let row_group = &row_group;
        let count = row_group.count();
        let stored = row_group.metadata();
        // `RowGroup::Checkpoint`.
        let mut pointer = RowGroupPointer {
            row_start,
            tuple_count: count,
            ..RowGroupPointer::default()
        };
        if data.action == RowGroupWriteAction::ReuseExistingRowGroupMetadata {
            pointer.data_pointers = stored.column_pointers.to_vec();
            pointer.has_metadata_blocks = stored.has_metadata_blocks;
            pointer
                .extra_metadata_blocks
                .clone_from(&stored.extra_metadata_blocks);
            pointer.has_per_column_metadata_blocks = stored.has_per_column_metadata_blocks;
            pointer
                .per_column_metadata_blocks
                .clone_from(&stored.per_column_metadata_blocks);
            if let Some(version_info) = row_group.version_info() {
                pointer.deletes_pointers = version_info.checkpoint(metadata, checkpoint_id)?;
            }
            metadata.clear_modified_blocks(&stored.column_pointers)?;
            metadata.clear_modified_blocks(&stored.extra_metadata_block_pointers())?;
            for (index, stats) in table_stats.iter_mut().enumerate() {
                merge_kept_column_statistics(row_group, index, stats, &mut incomplete)?;
                blocks.add_reused_column(&reused_column_data(row_group, index)?);
            }
            row_group_pointers.push(pointer);
            continue;
        }
        let mut reused_column_blocks = Vec::new();
        let mut written_column_blocks = PerColumnMetadataBlocks::default();
        let mut columns = Vec::with_capacity(data.columns.len());
        for (index, column) in data.columns.into_iter().enumerate() {
            let Some(column) = column else {
                // The column keeps its metadata.
                merge_kept_column_statistics(row_group, index, &mut table_stats[index], &mut incomplete)?;
                let start = stored.column_pointers[index];
                pointer.data_pointers.push(start);
                reused_column_blocks.push(start);
                let reused = reused_column_data(row_group, index)?;
                blocks.add_reused_column(&reused);
                columns.push(reused);
                continue;
            };
            merge_persistent_statistics(&column, &mut table_stats[index]);
            table_writer.track_written_pointers();
            let start = table_writer.meta_block_pointer()?;
            pointer.data_pointers.push(start);
            write_unit(table_writer, options, |s| column.serialize_fields(s))?;
            let extra: Vec<u64> = table_writer
                .take_written_pointers()
                .into_iter()
                .filter(|written| written.block_pointer != start.block_pointer)
                .map(|written| written.block_pointer)
                .collect();
            written_column_blocks.add_column(index, &extra);
            columns.push(column);
        }
        if supports_per_column_writes {
            pointer.has_per_column_metadata_blocks = true;
            if data.action == RowGroupWriteAction::PartiallyReuseColumnMetadata {
                pointer.per_column_metadata_blocks =
                    PerColumnMetadataBlocks::merge(&data.reused_extras, &written_column_blocks);
                // The blocks of the reused columns stay in use.
                reused_column_blocks.extend(
                    data.reused_extras
                        .blocks()
                        .into_iter()
                        .map(|block| MetaBlockPointer::new(block, 0)),
                );
            } else {
                pointer.per_column_metadata_blocks = written_column_blocks;
            }
        } else {
            pointer.has_metadata_blocks = true;
            pointer.extra_metadata_blocks = written_column_blocks.blocks();
        }
        if let Some(version_info) = row_group.version_info() {
            pointer.deletes_pointers = version_info.checkpoint(metadata, checkpoint_id)?;
        }
        metadata.clear_modified_blocks(&reused_column_blocks)?;
        persisted.push(PersistedRowGroup {
            row_group: Arc::clone(row_group),
            count,
            columns,
            metadata: RowGroupMetadata {
                column_pointers: pointer.data_pointers.as_slice().into(),
                has_metadata_blocks: pointer.has_metadata_blocks,
                extra_metadata_blocks: pointer.extra_metadata_blocks.clone(),
                has_per_column_metadata_blocks: pointer.has_per_column_metadata_blocks,
                per_column_metadata_blocks: pointer.per_column_metadata_blocks.clone(),
            },
        });
        row_group_pointers.push(pointer);
    }
    // The statistics of columns that were not built are the previous ones.
    for index in incomplete {
        if let (Some(stats), Some(previous)) = (table_stats.get_mut(index), previous.columns.get(index)) {
            stats.merge(previous);
        }
    }
    blocks.flush()?;
    // `FinalizeTable`.
    let table_pointer = table_writer.meta_block_pointer()?;
    table_writer.track_written_pointers();
    write_unit(table_writer, options, |s| {
        serialize_table_statistics(s, &table_stats, &previous.distinct)
    })?;
    table_writer.write_u64(row_group_pointers.len() as u64)?;
    let mut total_rows = 0;
    for pointer in &row_group_pointers {
        total_rows = total_rows.max(pointer.row_start + pointer.tuple_count);
        write_unit(table_writer, options, |s| {
            pointer.serialize_fields(s, supports_per_column_writes)
        })?;
    }
    let written_pointers = table_writer.take_written_pointers();
    Ok(TableDataPointer {
        table_pointer,
        total_rows,
        persisted,
        metadata_pointer: Some(TableMetadataPointer {
            pointer: table_pointer,
            blocks: written_pointers,
        }),
        statistics: Some(table_stats),
        vacuumed: (settings.full_checkpoint && dropped_any).then_some((layout, new_total_rows)),
    })
}

/// Checkpoints a table of an in-memory database (DuckDB's `InMemoryCheckpointer::WriteTable`
/// with `InMemoryTableDataWriter`): nothing is written, but a full checkpoint vacuums
/// deleted rows, dropping and merging row groups, and the version information of the
/// kept row groups is compressed.
pub fn checkpoint_in_memory_table(table: &DataTable, settings: TableWriteSettings) -> Result<()> {
    let _append = table.append_lock();
    let collection = table.row_groups();
    let row_groups = collection.row_groups();
    let nodes = row_groups.tree().nodes();
    let segment_count = nodes.len();
    let mut segments: Vec<Option<Arc<RowGroup>>> =
        nodes.iter().map(|node| Some(Arc::clone(&node.node))).collect();
    let mut vacuum = initialize_vacuum_state(table, &nodes, &mut segments, settings);
    let mut changed = segments.iter().any(Option::is_none);
    let mut layout: Vec<(Arc<RowGroup>, u64)> = Vec::with_capacity(segment_count);
    let mut new_total_rows = 0u64;
    let mut total_vacuum_tasks = 0u64;
    for segment_idx in 0..segment_count {
        let schedule = total_vacuum_tasks < settings.max_vacuum_tasks;
        match schedule_vacuum(
            &mut vacuum,
            &segments,
            segment_idx,
            schedule,
            collection.row_group_size(),
        ) {
            VacuumDecision::Merge {
                merge_count,
                target_count,
                merge_rows,
            } => {
                total_vacuum_tasks += 1;
                changed = true;
                let targets = vacuum_row_groups(
                    collection,
                    &vacuum,
                    &mut segments,
                    segment_idx,
                    merge_count,
                    target_count,
                    merge_rows,
                )?;
                for target in targets {
                    let count = target.count();
                    layout.push((target, new_total_rows));
                    new_total_rows += count;
                }
                continue;
            }
            VacuumDecision::Skip => {
                total_vacuum_tasks += 1;
                continue;
            }
            VacuumDecision::Keep => {}
        }
        let Some(row_group) = segments[segment_idx].clone() else {
            continue;
        };
        if let Some(version_info) = row_group.version_info() {
            version_info.compress_version_ids(settings.lowest_active_start);
        }
        let count = row_group.count();
        layout.push((row_group, new_total_rows));
        new_total_rows += count;
    }
    // `DataTable::Checkpoint`: appends start a new row group, which a later vacuum can
    // merge.
    collection.set_append_mode(crate::table::RowGroupAppendMode::SuggestNew);
    if !changed {
        return Ok(());
    }
    // `RowGroupCollection::Checkpoint`: the table consists of the kept and merged row
    // groups now.
    let base = row_groups.base_row_id();
    let nodes = layout
        .into_iter()
        .map(|(node, row_start)| crate::table::SegmentNode {
            row_start: base + row_start,
            node,
        })
        .collect();
    row_groups.tree().replace(nodes);
    collection.set_total_rows(new_total_rows);
    Ok(())
}

/// What the vacuum knows about a table checkpoint (DuckDB's `VacuumState`).
#[derive(Debug, Default)]
struct VacuumState {
    can_vacuum_deletes: bool,
    can_change_row_ids: bool,
    /// Whether dropping or merging row groups moved row ids.
    row_ids_changed: bool,
    next_vacuum_idx: usize,
    /// Per row group, its committed row count, `None` when it cannot be vacuumed.
    row_group_counts: Vec<Option<u64>>,
}

/// What `ScheduleVacuumTasks` decided for a row group.
#[derive(Debug, PartialEq, Eq)]
enum VacuumDecision {
    /// Not vacuumed: checkpoint the row group (unless it was dropped).
    Keep,
    /// The row group is merged by an earlier merge.
    Skip,
    /// Merge `merge_count` row groups holding `merge_rows` rows into `target_count`.
    Merge {
        merge_count: usize,
        target_count: usize,
        merge_rows: u64,
    },
}

/// `RowGroupCollection::InitializeVacuumState`: the committed row count of every row
/// group; row groups without committed rows are dropped (`segments` set to `None`).
fn initialize_vacuum_state(
    table: &DataTable,
    nodes: &[crate::table::SegmentNode<RowGroup>],
    segments: &mut [Option<Arc<RowGroup>>],
    settings: TableWriteSettings,
) -> VacuumState {
    let mut state = VacuumState {
        // Deletes can only be vacuumed by a full checkpoint.
        can_vacuum_deletes: settings.full_checkpoint,
        ..VacuumState::default()
    };
    if !state.can_vacuum_deletes {
        // Row groups without rows (an append rolled back) are left out; this moves no
        // row id.
        for (segment, node) in segments.iter_mut().zip(nodes) {
            if node.node.count() == 0 {
                *segment = None;
            }
        }
        return state;
    }
    // With indexes, row ids cannot move (`vacuum_rebuild_indexes` is not supported).
    state.can_change_row_ids = !table.has_indexes();
    let mut committed_counts: Vec<Option<u64>> = Vec::with_capacity(nodes.len());
    let mut dropped_any = false;
    for (index, node) in nodes.iter().enumerate() {
        let row_group = &node.node;
        let count = row_group.committed_row_count();
        if !state.can_change_row_ids {
            committed_counts.push(Some(count));
            if row_group.count() != count {
                // Vacuuming this row group would alter row ids.
                state.row_group_counts.push(None);
                continue;
            }
        }
        if count == 0 {
            // An empty row group is dropped entirely.
            segments[index] = None;
            dropped_any = true;
            state.row_group_counts.push(Some(0));
            continue;
        }
        if dropped_any {
            // The row ids of the row groups after a dropped one move forward.
            state.row_ids_changed = true;
        }
        state.row_group_counts.push(Some(count));
    }
    if !state.can_change_row_ids {
        // Trailing row groups without committed rows can still go: no row id moves.
        for index in (0..state.row_group_counts.len()).rev() {
            if committed_counts.get(index).copied().flatten() != Some(0) {
                break;
            }
            segments[index] = None;
        }
    }
    state
}

/// `RowGroupCollection::ScheduleVacuumTasks`: whether the row groups from `segment_idx`
/// on are merged into fewer ones (batches of up to three, preferring the fewest targets).
fn schedule_vacuum(
    state: &mut VacuumState,
    segments: &[Option<Arc<RowGroup>>],
    segment_idx: usize,
    schedule_vacuum: bool,
    row_group_size: u64,
) -> VacuumDecision {
    const MAX_MERGE_COUNT: usize = 3;
    if !state.can_vacuum_deletes {
        return VacuumDecision::Keep;
    }
    if segment_idx < state.next_vacuum_idx {
        // Merged by a previous merge.
        return VacuumDecision::Skip;
    }
    if state.row_group_counts[segment_idx] == Some(0) {
        // Already dropped.
        return VacuumDecision::Keep;
    }
    if !schedule_vacuum {
        return VacuumDecision::Keep;
    }
    let segment_count = segments.len();
    let mut merge_rows = 0;
    let mut merge_count = 0;
    let mut next_idx = 0;
    let mut target_count = 1;
    let mut perform_merge = false;
    while target_count <= MAX_MERGE_COUNT {
        let total_target_size = target_count as u64 * row_group_size;
        merge_count = 0;
        merge_rows = 0;
        next_idx = segment_idx;
        while next_idx < segment_count {
            let Some(next_row_count) = state.row_group_counts[next_idx] else {
                break;
            };
            if next_row_count == 0 {
                next_idx += 1;
                continue;
            }
            if merge_rows + next_row_count > total_target_size {
                break;
            }
            merge_rows += next_row_count;
            merge_count += 1;
            next_idx += 1;
        }
        if next_idx == segment_count {
            // Merging at the end needs to reach twice the current size (or a full row
            // group), so that repeated small appends do not rewrite the same rows again
            // and again.
            let current = state.row_group_counts[segment_idx].unwrap_or(0);
            let minimum_target = (current * 2).min(row_group_size) * target_count as u64;
            if merge_rows >= crate::VECTOR_SIZE && merge_rows < minimum_target {
                next_idx = segment_idx + 1;
                target_count += 1;
                continue;
            }
        }
        if target_count < merge_count {
            perform_merge = true;
            break;
        }
        target_count += 1;
    }
    if !perform_merge {
        return VacuumDecision::Keep;
    }
    state.next_vacuum_idx = next_idx;
    VacuumDecision::Merge {
        merge_count,
        target_count,
        merge_rows,
    }
}

/// `VacuumTask::ExecuteTask`: the committed rows of the `merge_count` row groups from
/// `segment_idx` on, appended to `target_count` new row groups; the merged ones are
/// dropped.
fn vacuum_row_groups(
    collection: &RowGroupCollection,
    state: &VacuumState,
    segments: &mut [Option<Arc<RowGroup>>],
    segment_idx: usize,
    merge_count: usize,
    target_count: usize,
    merge_rows: u64,
) -> Result<Vec<Arc<RowGroup>>> {
    use crate::table::{
        CollectionScanState, ColumnDataType, RowGroupAppendState, ScanFilterInfo, StorageIndex, TableScanType,
    };
    let _timer = rustdb_common::profiling::start_timer(rustdb_common::profiling::MetricType::CumulativeVacuumTime);
    let row_group_size = collection.row_group_size();
    let types = collection.types();
    let io = collection.io();
    let mut targets: Vec<RowGroup> = Vec::with_capacity(target_count);
    let mut append_counts = vec![0u64; target_count];
    for _ in 0..target_count {
        targets.push(RowGroup::new_empty(io, types, ColumnDataType::MainTable));
    }
    let column_ids: Vec<StorageIndex> = (0..types.len()).map(StorageIndex::from).collect();
    let options = RowGroup::scan_options(
        TableScanType::CommittedRows,
        crate::transaction::TransactionData::new(0, crate::transaction::TRANSACTION_ID_START),
    );
    let mut current = 0;
    let mut append: RowGroupAppendState = targets[current].initialize_append()?;
    let mut merged = 0;
    let mut index = segment_idx;
    while merged < merge_count && index < state.row_group_counts.len() {
        if state.row_group_counts[index] == Some(0) {
            index += 1;
            continue;
        }
        merged += 1;
        let source = segments[index]
            .take()
            .ok_or_else(|| Error::internal("vacuum of a dropped row group"))?;
        let mut filters = ScanFilterInfo::default();
        let mut scan = CollectionScanState {
            max_row: u64::MAX,
            column_scans: RowGroup::column_scan_states(types, &column_ids),
            ..CollectionScanState::default()
        };
        if source.initialize_scan(&mut scan, &column_ids, &mut filters, 0, 0)? {
            let mut chunk = rustdb_types::DataChunk::with_types(types, rustdb_common::STANDARD_VECTOR_SIZE);
            loop {
                chunk.reset();
                source.scan(options, &mut scan, &column_ids, &mut filters, 0, &mut chunk)?;
                if chunk.is_empty() {
                    break;
                }
                let columns: Vec<rustdb_types::Vector> = chunk
                    .columns()
                    .iter()
                    .map(|vector| vector.flattened(chunk.len()))
                    .collect();
                let mut remaining = chunk.len();
                let mut offset = 0;
                while remaining > 0 {
                    let append_count =
                        (remaining as u64).min(row_group_size - append_counts[current]) as usize;
                    let part = if offset == 0 {
                        rustdb_types::DataChunk::new(columns.clone(), remaining)
                    } else {
                        let rows: Vec<u32> = (offset as u32..(offset + remaining) as u32).collect();
                        let sel = rustdb_types::SelectionVector::new(&rows[..]);
                        rustdb_types::DataChunk::new(
                            rustdb_types::Vector::slice_all(&columns, &sel),
                            remaining,
                        )
                    };
                    targets[current].append(&mut append, &part, append_count)?;
                    append_counts[current] += append_count as u64;
                    remaining -= append_count;
                    offset += append_count;
                    let full = append_counts[current] == row_group_size;
                    let last = current + 1 >= targets.len();
                    if remaining > 0 || (full && !last) {
                        current += 1;
                        if current >= targets.len() {
                            return Err(Error::internal("vacuum ran out of target row groups"));
                        }
                        append = targets[current].initialize_append()?;
                    }
                }
            }
        }
        index += 1;
    }
    let total: u64 = append_counts.iter().sum();
    if total != merge_rows {
        return Err(Error::internal(format!(
            "Mismatch in row group count {merge_rows} vs verify count {total} in RowGroupCollection::Checkpoint"
        )));
    }
    Ok(targets
        .into_iter()
        .zip(append_counts)
        .map(|(target, count)| {
            target.set_vacuumed_count(count);
            Arc::new(target)
        })
        .collect())
}
