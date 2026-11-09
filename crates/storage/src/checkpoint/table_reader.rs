//! Reading a table's data from a checkpoint (DuckDB's `TableDataReader`,
//! `RowGroup` construction from a `RowGroupPointer`, `ColumnData::Deserialize` and
//! `RowVersionManager::Deserialize`).

use std::sync::Arc;

use roastedduck_common::{Error, Result, STANDARD_VECTOR_SIZE};
use roastedduck_types::LogicalType;

use super::pointers::{PersistentColumnData, RowGroupPointer};
use super::stream::StreamDeserializer;
use crate::data_table::TableIoManager;
use crate::metadata::{MetaBlockPointer, MetadataManager, MetadataReader};
use crate::statistics::{BaseStatistics, DistinctStatistics, deserialize_table_statistics};
use crate::table::{
    ChunkVectorInfo, RowGroup, RowGroupCollection, RowGroupMetadata, RowVersionManager, TableMetadataPointer,
    TableStatistics,
};

/// A table's data as stored by a checkpoint.
#[derive(Debug)]
pub struct PersistentTableData {
    pub statistics: Vec<BaseStatistics>,
    /// The distinct statistics of each column, if stored.
    pub distinct: Vec<Option<DistinctStatistics>>,
    pub row_groups: Vec<RowGroupPointer>,
    pub total_rows: u64,
    /// Where the statistics and row group pointers start, and every metadata block they
    /// span (`base_table_pointer` and `read_metadata_pointers`).
    pub metadata_pointer: TableMetadataPointer,
}

/// Reads the statistics and row group pointers written by `FinalizeTable`
/// (`TableDataReader::ReadTableData`).
pub fn read_table_data(
    metadata: &MetadataManager,
    table_pointer: MetaBlockPointer,
    types: &[LogicalType],
    total_rows: u64,
) -> Result<PersistentTableData> {
    let mut reader = MetadataReader::new(metadata, table_pointer, true)?;
    reader.track_read_pointers();
    let (statistics, distinct) = {
        let mut deserializer = StreamDeserializer::new(&mut reader);
        deserializer.begin()?;
        let statistics = deserialize_table_statistics(&mut deserializer, types)?;
        deserializer.end()?;
        statistics
    };
    let count = reader.read_u64()?;
    let mut row_groups = Vec::new();
    for _ in 0..count {
        let mut deserializer = StreamDeserializer::new(&mut reader);
        deserializer.begin()?;
        row_groups.push(RowGroupPointer::deserialize_fields(&mut deserializer)?);
        deserializer.end()?;
    }
    Ok(PersistentTableData {
        statistics,
        distinct,
        row_groups,
        total_rows,
        metadata_pointer: TableMetadataPointer {
            pointer: table_pointer,
            blocks: reader.take_read_pointers(),
        },
    })
}

/// Reads the `PersistentColumnData` of one column (`ColumnData::Deserialize`).
pub(crate) fn read_column(
    metadata: &MetadataManager,
    pointer: MetaBlockPointer,
    ty: &LogicalType,
) -> Result<PersistentColumnData> {
    let mut reader = MetadataReader::new(metadata, pointer, true)?;
    let mut deserializer = StreamDeserializer::new(&mut reader);
    deserializer.begin()?;
    let data = PersistentColumnData::deserialize_fields(&mut deserializer, ty)?;
    deserializer.end()?;
    Ok(data)
}

/// `ValidityMask::Read` of one vector: a set bit per valid row.
fn read_validity_mask(reader: &mut MetadataReader<'_>) -> Result<Vec<u64>> {
    let words = STANDARD_VECTOR_SIZE / 64;
    let flag = reader.read_u8()?;
    if flag == 0 {
        let mut mask = Vec::with_capacity(words);
        for _ in 0..words {
            mask.push(reader.read_u64()?);
        }
        return Ok(mask);
    }
    let is_valid = flag == 1;
    let count = reader.read_u32()?;
    let mut mask = vec![if is_valid { 0 } else { u64::MAX }; words];
    for _ in 0..count {
        let mut bytes = [0u8; 2];
        reader.read_data(&mut bytes)?;
        let row = usize::from(u16::from_le_bytes(bytes));
        if row >= STANDARD_VECTOR_SIZE {
            return Err(Error::serialization("validity mask index out of range"));
        }
        if is_valid {
            mask[row / 64] |= 1 << (row % 64);
        } else {
            mask[row / 64] &= !(1 << (row % 64));
        }
    }
    Ok(mask)
}

/// `RowVersionManager::Deserialize`: the deletes of a row group.
fn read_deletes(metadata: &MetadataManager, pointer: MetaBlockPointer) -> Result<RowVersionManager> {
    const CONSTANT_INFO: u8 = 0;
    const VECTOR_INFO: u8 = 1;
    const EMPTY_INFO: u8 = 2;
    let mut reader = MetadataReader::new(metadata, pointer, true)?;
    reader.track_read_pointers();
    let chunk_count = reader.read_u64()?;
    let mut vectors: Vec<Option<ChunkVectorInfo>> = Vec::new();
    for _ in 0..chunk_count {
        let vector_index =
            usize::try_from(reader.read_u64()?).map_err(|_| Error::serialization("vector index"))?;
        if vector_index * STANDARD_VECTOR_SIZE >= crate::MAX_ROW_GROUP_SIZE as usize {
            return Err(Error::new(
                roastedduck_common::ErrorKind::Io,
                format!(
                    "In DeserializeDeletes, vector_index {vector_index} is out of range for the max row group size \
                     of {}. Corrupted file?",
                    crate::MAX_ROW_GROUP_SIZE
                ),
            ));
        }
        if vectors.len() <= vector_index {
            vectors.resize_with(vector_index + 1, || None);
        }
        vectors[vector_index] = match reader.read_u8()? {
            EMPTY_INFO => None,
            CONSTANT_INFO => {
                let start = reader.read_u64()?;
                Some(ChunkVectorInfo::with_ids(start, 0, 0))
            }
            VECTOR_INFO => {
                let start = reader.read_u64()?;
                Some(ChunkVectorInfo::from_deleted_mask(
                    start,
                    read_validity_mask(&mut reader)?,
                ))
            }
            _ => {
                return Err(Error::serialization(
                    "Could not deserialize Chunk Info Type: unrecognized type",
                ));
            }
        };
    }
    Ok(RowVersionManager::from_vectors(
        vectors,
        reader.take_read_pointers(),
    ))
}

/// Builds the row groups of a table from its checkpoint data; their columns are read when
/// first used.
pub fn load_row_groups(
    io: &Arc<TableIoManager>,
    metadata: &Arc<MetadataManager>,
    types: &[LogicalType],
    data: PersistentTableData,
) -> Result<RowGroupCollection> {
    let collection = RowGroupCollection::new(Arc::clone(io), types.to_vec(), 0);
    let shared_types: Arc<[LogicalType]> = Arc::from(types);
    for mut pointer in data.row_groups {
        if pointer.data_pointers.len() != types.len() {
            return Err(Error::serialization(
                "row group column count does not match the table",
            ));
        }
        let row_group = RowGroup::from_metadata(
            Arc::clone(io),
            Arc::clone(&shared_types),
            Arc::clone(metadata),
            RowGroupMetadata {
                column_pointers: pointer.data_pointers.as_slice().into(),
                has_metadata_blocks: pointer.has_metadata_blocks,
                extra_metadata_blocks: std::mem::take(&mut pointer.extra_metadata_blocks),
                has_per_column_metadata_blocks: pointer.has_per_column_metadata_blocks,
                per_column_metadata_blocks: pointer.per_column_metadata_blocks.clone(),
            },
            pointer.tuple_count,
        );
        if let Some(first) = pointer.deletes_pointers.first() {
            row_group.set_version_info(Arc::new(read_deletes(metadata, *first)?));
        }
        collection.append_existing_row_group(Arc::new(row_group));
    }
    // Types that keep distinct statistics get empty ones when none were stored
    // (`ColumnStatistics::CreateEmptyStats` before `SetDistinct`).
    let distinct = types
        .iter()
        .enumerate()
        .map(|(index, ty)| match data.distinct.get(index).cloned().flatten() {
            Some(distinct) => Some(distinct),
            None => DistinctStatistics::type_is_supported(ty).then(DistinctStatistics::default),
        })
        .collect();
    collection.set_statistics(TableStatistics {
        columns: data.statistics,
        distinct,
    });
    collection.set_metadata_pointer(Some(data.metadata_pointer));
    if !collection.is_empty() {
        collection.set_append_mode(crate::table::RowGroupAppendMode::SuggestNew);
    }
    Ok(collection)
}
