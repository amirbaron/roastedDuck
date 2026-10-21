//! The pointers a checkpoint writes into metadata (DuckDB's `BlockPointer`,
//! `DataPointer`, `PersistentColumnData` and `RowGroupPointer`, with their
//! serialization).

use rustdb_buffer::{BlockId, INVALID_BLOCK};
use rustdb_common::{Error, Result};
use rustdb_serde::{Deserializer, Serializer};
use rustdb_types::{GeometryStorageType, LogicalType, LogicalTypeId, PhysicalType};

use crate::compression::CompressionType;
use crate::metadata::MetaBlockPointer;
use crate::statistics::BaseStatistics;
use crate::table::{PerColumnMetadataBlock, PerColumnMetadataBlocks};

/// A position in a block (DuckDB's `BlockPointer`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockPointer {
    pub block_id: BlockId,
    pub offset: u32,
}

impl BlockPointer {
    pub const fn invalid() -> Self {
        Self {
            block_id: INVALID_BLOCK,
            offset: 0,
        }
    }

    pub const fn is_valid(&self) -> bool {
        self.block_id != INVALID_BLOCK
    }

    /// `BlockPointer::Serialize` (the object's fields).
    pub fn serialize_fields(&self, serializer: &mut dyn Serializer) -> Result<()> {
        serializer.write_property(100, "block_id", &self.block_id)?;
        serializer.write_optional(101, "offset", self.offset == 0, &self.offset)
    }

    /// `BlockPointer::Deserialize`.
    pub fn deserialize_fields(deserializer: &mut dyn Deserializer) -> Result<Self> {
        let block_id = deserializer.read_property(100, "block_id")?;
        let offset = deserializer.read_optional(101, "offset")?.unwrap_or(0);
        Ok(Self { block_id, offset })
    }
}

/// Writes a `MetaBlockPointer` object's fields (`MetaBlockPointer::Serialize`).
pub fn serialize_meta_block_pointer(
    serializer: &mut dyn Serializer,
    pointer: MetaBlockPointer,
) -> Result<()> {
    serializer.write_optional(
        100,
        "block_pointer",
        pointer.block_pointer == 0,
        &pointer.block_pointer,
    )?;
    serializer.write_optional(101, "offset", pointer.offset == 0, &pointer.offset)
}

/// Reads a `MetaBlockPointer` object's fields.
pub fn deserialize_meta_block_pointer(deserializer: &mut dyn Deserializer) -> Result<MetaBlockPointer> {
    let block_pointer = deserializer.read_optional(100, "block_pointer")?.unwrap_or(0);
    let offset = deserializer.read_optional(101, "offset")?.unwrap_or(0);
    Ok(MetaBlockPointer::new(block_pointer, offset))
}

/// Writes a list of `MetaBlockPointer`s as property `field_id`.
pub fn write_meta_pointer_list(
    serializer: &mut dyn Serializer,
    field_id: u16,
    tag: &'static str,
    pointers: &[MetaBlockPointer],
) -> Result<()> {
    serializer.write_list_property(field_id, tag, pointers.len(), |s, i| {
        s.on_object_begin();
        serialize_meta_block_pointer(s, pointers[i])?;
        s.on_object_end();
        Ok(())
    })
}

/// Reads a list of `MetaBlockPointer`s.
pub fn read_meta_pointer_list(deserializer: &mut dyn Deserializer) -> Result<Vec<MetaBlockPointer>> {
    let mut result = Vec::new();
    deserializer.read_list(|d, _| {
        d.on_object_begin()?;
        result.push(deserialize_meta_block_pointer(d)?);
        d.on_object_end()
    })?;
    Ok(result)
}

/// The name of a compression type in `EnumUtil` (JSON serialization).
pub(crate) fn compression_enum_name(compression: CompressionType) -> &'static str {
    match compression {
        CompressionType::Auto => "COMPRESSION_AUTO",
        CompressionType::Uncompressed => "COMPRESSION_UNCOMPRESSED",
        CompressionType::Constant => "COMPRESSION_CONSTANT",
        CompressionType::Rle => "COMPRESSION_RLE",
        CompressionType::Dictionary => "COMPRESSION_DICTIONARY",
        CompressionType::PforDelta => "COMPRESSION_PFOR_DELTA",
        CompressionType::BitPacking => "COMPRESSION_BITPACKING",
        CompressionType::Fsst => "COMPRESSION_FSST",
        CompressionType::Chimp => "COMPRESSION_CHIMP",
        CompressionType::Patas => "COMPRESSION_PATAS",
        CompressionType::Alp => "COMPRESSION_ALP",
        CompressionType::AlpRd => "COMPRESSION_ALPRD",
        CompressionType::Zstd => "COMPRESSION_ZSTD",
        CompressionType::Roaring => "COMPRESSION_ROARING",
        CompressionType::Empty => "COMPRESSION_EMPTY",
        CompressionType::DictFsst => "COMPRESSION_DICT_FSST",
    }
}

/// Where one segment of a column is stored (DuckDB's `DataPointer`).
#[derive(Clone, Debug)]
pub struct DataPointer {
    /// The first row of the segment within the row group (only written for storage
    /// versions that need it).
    pub row_start: u64,
    pub tuple_count: u64,
    pub block_pointer: BlockPointer,
    pub compression: CompressionType,
    pub statistics: BaseStatistics,
    /// The overflow string blocks of an uncompressed string segment (DuckDB's
    /// `SerializedStringSegmentState`).
    pub overflow_blocks: Vec<BlockId>,
}

impl DataPointer {
    /// `DataPointer::Serialize`.
    pub fn serialize_fields(&self, serializer: &mut dyn Serializer) -> Result<()> {
        if !serializer.should_serialize(7) {
            serializer.write_optional(100, "row_start", self.row_start == 0, &self.row_start)?;
        }
        serializer.write_optional(101, "tuple_count", self.tuple_count == 0, &self.tuple_count)?;
        serializer.write_object_property(102, "block_pointer", |s| self.block_pointer.serialize_fields(s))?;
        serializer.on_property_begin(103, "compression_type");
        let compression = self.compression as u8;
        rustdb_serde::write_enum(serializer, compression_enum_name(self.compression), |s| {
            s.write_u8(compression);
        });
        serializer.on_property_end();
        serializer.write_object_property(104, "statistics", |s| self.statistics.serialize_fields(s))?;
        let has_state = !self.overflow_blocks.is_empty();
        serializer.write_optional_with(105, "segment_state", !has_state, |s| {
            // A `unique_ptr<ColumnSegmentState>`: present, then the object.
            s.on_nullable_begin(true);
            s.on_object_begin();
            s.write_list_property(1, "overflow_blocks", self.overflow_blocks.len(), |s, i| {
                s.write_i64(self.overflow_blocks[i]);
                Ok(())
            })?;
            s.on_object_end();
            s.on_nullable_end();
            Ok(())
        })
    }

    /// `DataPointer::Deserialize` for a segment of type `ty`.
    pub fn deserialize_fields(deserializer: &mut dyn Deserializer, ty: &LogicalType) -> Result<Self> {
        let row_start: Option<u64> = deserializer.read_optional(100, "row_start")?;
        let tuple_count = deserializer.read_optional(101, "tuple_count")?.unwrap_or(0);
        let block_pointer =
            deserializer.read_object_property(102, "block_pointer", BlockPointer::deserialize_fields)?;
        deserializer.on_property_begin(103, "compression_type")?;
        let compression_value = deserializer.read_u8()?;
        deserializer.on_property_end()?;
        let compression = CompressionType::from_u8(compression_value)
            .ok_or_else(|| Error::serialization(format!("Unknown compression type {compression_value}")))?;
        let statistics = deserializer
            .read_object_property(104, "statistics", |d| BaseStatistics::deserialize_fields(d, ty))?;
        let mut overflow_blocks = Vec::new();
        if deserializer.on_optional_property_begin(105, "segment_state")? {
            if deserializer.on_nullable_begin()? {
                deserializer.on_object_begin()?;
                // Uncompressed strings and ZSTD keep their extra blocks in a
                // `SerializedStringSegmentState`; no other codec has a segment state.
                if !matches!(compression, CompressionType::Uncompressed | CompressionType::Zstd)
                    || ty.physical_type() != PhysicalType::Varchar
                {
                    return Err(Error::not_implemented(format!(
                        "reading the segment state of {compression} segments"
                    )));
                }
                deserializer.read_list_property(1, "overflow_blocks", |d, _| {
                    overflow_blocks.push(d.read_i64()?);
                    Ok(())
                })?;
                deserializer.on_object_end()?;
            }
            deserializer.on_nullable_end()?;
            deserializer.on_optional_property_end(true)?;
        }
        Ok(Self {
            row_start: row_start.unwrap_or(0),
            tuple_count,
            block_pointer,
            compression,
            statistics,
            overflow_blocks,
        })
    }
}

/// The segments of a column and of its children (DuckDB's `PersistentColumnData`).
#[derive(Clone, Debug)]
pub struct PersistentColumnData {
    /// The type of the stored data: for a shredded GEOMETRY column its vectorized layout.
    pub logical_type: LogicalType,
    pub pointers: Vec<DataPointer>,
    /// Validity first, then the list/array child or the struct fields.
    pub child_columns: Vec<PersistentColumnData>,
    /// The storage type of a GEOMETRY column (DuckDB's `GeometryPersistentColumnData`
    /// `extra_data`); `None` for other columns and for the spatial extension's legacy
    /// GEOMETRY.
    pub geometry_storage: Option<GeometryStorageType>,
    /// The type of the shredded sub-column of a shredded VARIANT column (DuckDB's
    /// `VariantPersistentColumnData` `extra_data`); `None` for other columns.
    pub variant_shredded_type: Option<LogicalType>,
}

/// The `extra_data` of a `PersistentColumnData` (`ExtraPersistentColumnData`).
enum ExtraData {
    Variant(LogicalType),
    Geometry(GeometryStorageType),
}

/// `ExtraPersistentColumnDataType`.
const EXTRA_DATA_VARIANT: u8 = 1;
const EXTRA_DATA_GEOMETRY: u8 = 2;

fn extra_data_type_name(value: u8) -> &'static str {
    match value {
        EXTRA_DATA_VARIANT => "VARIANT",
        EXTRA_DATA_GEOMETRY => "GEOMETRY",
        _ => "INVALID",
    }
}

/// `ExtraPersistentColumnData::Serialize` of a `GeometryPersistentColumnData`.
fn write_geometry_extra_data(serializer: &mut dyn Serializer, storage: GeometryStorageType) -> Result<()> {
    serializer.on_property_begin(100, "type");
    rustdb_serde::write_enum(serializer, extra_data_type_name(EXTRA_DATA_GEOMETRY), |s| {
        s.write_u8(EXTRA_DATA_GEOMETRY);
    });
    serializer.on_property_end();
    serializer.write_optional(101, "storage_type", storage == GeometryStorageType::Wkb, &storage)
}

/// `ExtraPersistentColumnData::Serialize` of a `VariantPersistentColumnData`.
fn write_variant_extra_data(serializer: &mut dyn Serializer, shredded_type: &LogicalType) -> Result<()> {
    serializer.on_property_begin(100, "type");
    rustdb_serde::write_enum(serializer, extra_data_type_name(EXTRA_DATA_VARIANT), |s| {
        s.write_u8(EXTRA_DATA_VARIANT);
    });
    serializer.on_property_end();
    serializer.write_property(101, "storage_type", shredded_type)
}

/// `ExtraPersistentColumnData::Deserialize`.
fn read_extra_data(deserializer: &mut dyn Deserializer) -> Result<ExtraData> {
    let kind = if deserializer.on_optional_property_begin(100, "type")? {
        let kind = rustdb_serde::read_enum(
            deserializer,
            "ExtraPersistentColumnDataType",
            |name| {
                ["INVALID", "VARIANT", "GEOMETRY"]
                    .iter()
                    .position(|n| n.eq_ignore_ascii_case(name))
                    .map(|i| i as u8)
            },
            |d| Ok(Some(d.read_u8()?)),
        )?;
        deserializer.on_optional_property_end(true)?;
        kind
    } else {
        deserializer.on_optional_property_end(false)?;
        0
    };
    match kind {
        EXTRA_DATA_GEOMETRY => Ok(ExtraData::Geometry(
            deserializer
                .read_optional(101, "storage_type")?
                .unwrap_or(GeometryStorageType::Wkb),
        )),
        EXTRA_DATA_VARIANT => Ok(ExtraData::Variant(
            deserializer
                .read_optional(101, "storage_type")?
                .unwrap_or_default(),
        )),
        _ => Err(Error::internal("Unknown PersistentColumnData type")),
    }
}

fn validity_type() -> LogicalType {
    LogicalType::simple(LogicalTypeId::Validity)
}

impl PersistentColumnData {
    pub fn new(logical_type: LogicalType) -> Self {
        Self {
            logical_type,
            pointers: Vec::new(),
            child_columns: Vec::new(),
            geometry_storage: None,
            variant_shredded_type: None,
        }
    }

    fn write_pointers(&self, serializer: &mut dyn Serializer) -> Result<()> {
        serializer.write_optional_with(100, "data_pointers", self.pointers.is_empty(), |s| {
            s.on_list_begin(self.pointers.len());
            for pointer in &self.pointers {
                s.on_object_begin();
                pointer.serialize_fields(s)?;
                s.on_object_end();
            }
            s.on_list_end();
            Ok(())
        })
    }

    fn child(&self, index: usize) -> Result<&Self> {
        self.child_columns
            .get(index)
            .ok_or_else(|| Error::internal("persistent column data without its children"))
    }

    /// `PersistentColumnData::Serialize`.
    pub fn serialize_fields(&self, serializer: &mut dyn Serializer) -> Result<()> {
        // "extra_data" (99) is only written for shredded VARIANT and GEOMETRY columns.
        let has_extra_data = self.geometry_storage.is_some() || self.variant_shredded_type.is_some();
        serializer.on_optional_property_begin(99, "extra_data", has_extra_data);
        if has_extra_data {
            serializer.on_nullable_begin(true);
            serializer.on_object_begin();
            if let Some(shredded_type) = &self.variant_shredded_type {
                write_variant_extra_data(serializer, shredded_type)?;
            } else if let Some(storage) = self.geometry_storage {
                write_geometry_extra_data(serializer, storage)?;
            }
            serializer.on_object_end();
            serializer.on_nullable_end();
        }
        serializer.on_optional_property_end(has_extra_data);
        self.write_pointers(serializer)?;
        if self.logical_type.id() == LogicalTypeId::Validity {
            return Ok(());
        }
        if self.logical_type.id() == LogicalTypeId::Variant {
            // The validity, the unshredded and the (optional) shredded sub-columns.
            serializer.write_object_property(101, "validity", |s| self.child(0)?.serialize_fields(s))?;
            serializer.write_object_property(102, "unshredded", |s| self.child(1)?.serialize_fields(s))?;
            if let Some(shredded) = self.child_columns.get(2) {
                serializer.write_object_property(103, "shredded", |s| shredded.serialize_fields(s))?;
            }
            return Ok(());
        }
        match self.logical_type.physical_type() {
            PhysicalType::Bit => {}
            PhysicalType::List | PhysicalType::Array => {
                serializer.write_object_property(101, "validity", |s| self.child(0)?.serialize_fields(s))?;
                serializer
                    .write_object_property(102, "child_column", |s| self.child(1)?.serialize_fields(s))?;
            }
            PhysicalType::Struct => {
                serializer.write_object_property(101, "validity", |s| self.child(0)?.serialize_fields(s))?;
                let fields = &self.child_columns[1..];
                serializer.write_list_property(102, "sub_columns", fields.len(), |s, i| {
                    s.on_object_begin();
                    fields[i].serialize_fields(s)?;
                    s.on_object_end();
                    Ok(())
                })?;
            }
            _ => {
                serializer.write_object_property(101, "validity", |s| self.child(0)?.serialize_fields(s))?;
            }
        }
        Ok(())
    }

    fn read_pointers(deserializer: &mut dyn Deserializer, ty: &LogicalType) -> Result<Vec<DataPointer>> {
        let mut pointers = Vec::new();
        if deserializer.on_optional_property_begin(100, "data_pointers")? {
            deserializer.read_list(|d, _| {
                d.on_object_begin()?;
                pointers.push(DataPointer::deserialize_fields(d, ty)?);
                d.on_object_end()
            })?;
            deserializer.on_optional_property_end(true)?;
        }
        Ok(pointers)
    }

    /// `PersistentColumnData::Deserialize` for a column of type `ty`.
    pub fn deserialize_fields(deserializer: &mut dyn Deserializer, ty: &LogicalType) -> Result<Self> {
        // `GetPersistentColumnDataType`: the extra data of a GEOMETRY column decides the
        // type of the stored data; that of a VARIANT column the type of its shredded data.
        let mut extra_data = None;
        if deserializer.on_optional_property_begin(99, "extra_data")? {
            if deserializer.on_nullable_begin()? {
                deserializer.on_object_begin()?;
                extra_data = Some(read_extra_data(deserializer)?);
                deserializer.on_object_end()?;
            }
            deserializer.on_nullable_end()?;
            deserializer.on_optional_property_end(true)?;
        } else {
            deserializer.on_optional_property_end(false)?;
        }
        let (geometry_storage, variant_shredded_type) = match extra_data {
            Some(ExtraData::Geometry(storage)) => (Some(storage), None),
            Some(ExtraData::Variant(shredded_type)) => (None, Some(shredded_type)),
            None => (None, None),
        };
        if ty.id() == LogicalTypeId::Variant || variant_shredded_type.is_some() {
            let read_child =
                |d: &mut dyn Deserializer, field: u16, tag: &'static str, child_type: &LogicalType| {
                    d.read_object_property(field, tag, |d| Self::deserialize_fields(d, child_type))
                };
            let mut result = Self::new(LogicalType::variant());
            result.pointers = Self::read_pointers(deserializer, &result.logical_type)?;
            result
                .child_columns
                .push(read_child(deserializer, 101, "validity", &validity_type())?);
            result.child_columns.push(read_child(
                deserializer,
                102,
                "unshredded",
                &rustdb_types::variant::unshredded_type(),
            )?);
            if let Some(shredded_type) = &variant_shredded_type {
                result
                    .child_columns
                    .push(read_child(deserializer, 103, "shredded", shredded_type)?);
            }
            result.variant_shredded_type = variant_shredded_type;
            return Ok(result);
        }
        let stored_type = match geometry_storage {
            Some(storage) => rustdb_types::geometry::vectorized_type(storage)?,
            None => ty.clone(),
        };
        let ty = &stored_type;
        let mut result = Self::new(ty.clone());
        result.geometry_storage = geometry_storage;
        result.pointers = Self::read_pointers(deserializer, ty)?;
        if ty.id() == LogicalTypeId::Validity {
            return Ok(result);
        }
        let read_child =
            |d: &mut dyn Deserializer, field: u16, tag: &'static str, child_type: &LogicalType| {
                d.read_object_property(field, tag, |d| Self::deserialize_fields(d, child_type))
            };
        match ty.physical_type() {
            PhysicalType::Bit => {}
            PhysicalType::List => {
                result
                    .child_columns
                    .push(read_child(deserializer, 101, "validity", &validity_type())?);
                let child_type = ty.child_type().cloned().unwrap_or_default();
                result
                    .child_columns
                    .push(read_child(deserializer, 102, "child_column", &child_type)?);
            }
            PhysicalType::Array => {
                result
                    .child_columns
                    .push(read_child(deserializer, 101, "validity", &validity_type())?);
                let child_type = ty
                    .array_child()
                    .map(|(child, _)| child.clone())
                    .unwrap_or_default();
                result
                    .child_columns
                    .push(read_child(deserializer, 102, "child_column", &child_type)?);
            }
            PhysicalType::Struct => {
                result
                    .child_columns
                    .push(read_child(deserializer, 101, "validity", &validity_type())?);
                let fields: Vec<LogicalType> = ty
                    .struct_children()
                    .unwrap_or_default()
                    .iter()
                    .map(|(_, field)| field.clone())
                    .collect();
                let mut children = Vec::with_capacity(fields.len());
                deserializer.read_list_property(102, "sub_columns", |d, i| {
                    let field = fields
                        .get(i)
                        .ok_or_else(|| Error::serialization("more struct sub-columns than fields"))?;
                    d.on_object_begin()?;
                    children.push(Self::deserialize_fields(d, field)?);
                    d.on_object_end()
                })?;
                result.child_columns.extend(children);
            }
            _ => {
                result
                    .child_columns
                    .push(read_child(deserializer, 101, "validity", &validity_type())?);
            }
        }
        Ok(result)
    }

    /// Every block the column's segments use (data and overflow blocks), with one entry
    /// per reference.
    pub fn collect_blocks(&self, blocks: &mut Vec<BlockId>) {
        for pointer in &self.pointers {
            if pointer.block_pointer.is_valid() {
                blocks.push(pointer.block_pointer.block_id);
            }
            blocks.extend(pointer.overflow_blocks.iter().copied());
        }
        for child in &self.child_columns {
            child.collect_blocks(blocks);
        }
    }
}

/// A row group an optimistic write stored in the database file, as a commit logs it to the
/// WAL (DuckDB's `PersistentRowGroupData`).
#[derive(Clone, Debug)]
pub struct PersistentRowGroupData {
    pub types: Vec<LogicalType>,
    pub column_data: Vec<PersistentColumnData>,
    /// The row id of the row group's first row in the table.
    pub start: u64,
    pub count: u64,
}

impl PersistentRowGroupData {
    /// `PersistentRowGroupData::Serialize`.
    pub fn serialize_fields(&self, serializer: &mut dyn Serializer) -> Result<()> {
        serializer.write_list_property(100, "types", self.types.len(), |s, index| {
            s.write_object(&self.types[index])
        })?;
        serializer.write_list_property(101, "columns", self.column_data.len(), |s, index| {
            s.on_object_begin();
            self.column_data[index].serialize_fields(s)?;
            s.on_object_end();
            Ok(())
        })?;
        serializer.write_property(102, "start", &self.start)?;
        serializer.write_property(103, "count", &self.count)
    }

    /// `PersistentRowGroupData::Deserialize`.
    pub fn deserialize_fields(deserializer: &mut dyn Deserializer) -> Result<Self> {
        let mut types: Vec<LogicalType> = Vec::new();
        deserializer.read_list_property(100, "types", |d, _| {
            types.push(d.read_object()?);
            Ok(())
        })?;
        let mut column_data = Vec::with_capacity(types.len());
        deserializer.read_list_property(101, "columns", |d, index| {
            let ty = types
                .get(index)
                .ok_or_else(|| Error::serialization("more row group columns than types"))?;
            d.on_object_begin()?;
            column_data.push(PersistentColumnData::deserialize_fields(d, ty)?);
            d.on_object_end()
        })?;
        Ok(Self {
            types,
            column_data,
            start: deserializer.read_property(102, "start")?,
            count: deserializer.read_property(103, "count")?,
        })
    }
}

/// The row groups an optimistic write stored, as logged to the WAL by a commit
/// (DuckDB's `PersistentCollectionData`).
#[derive(Clone, Debug, Default)]
pub struct PersistentCollectionData {
    pub row_group_data: Vec<PersistentRowGroupData>,
}

impl PersistentCollectionData {
    /// `PersistentCollectionData::Serialize`.
    pub fn serialize_fields(&self, serializer: &mut dyn Serializer) -> Result<()> {
        serializer.write_list_property(100, "row_groups", self.row_group_data.len(), |s, index| {
            s.on_object_begin();
            self.row_group_data[index].serialize_fields(s)?;
            s.on_object_end();
            Ok(())
        })
    }

    /// `PersistentCollectionData::Deserialize`.
    pub fn deserialize_fields(deserializer: &mut dyn Deserializer) -> Result<Self> {
        let mut row_group_data = Vec::new();
        deserializer.read_list_property(100, "row_groups", |d, _| {
            d.on_object_begin()?;
            row_group_data.push(PersistentRowGroupData::deserialize_fields(d)?);
            d.on_object_end()
        })?;
        Ok(Self { row_group_data })
    }

    /// Every block the row groups use, once per reference (`GetBlockIds`).
    pub fn block_ids(&self) -> Vec<BlockId> {
        let mut blocks = Vec::new();
        for row_group in &self.row_group_data {
            for column in &row_group.column_data {
                column.collect_blocks(&mut blocks);
            }
        }
        blocks
    }
}

/// Where a row group's columns and deletes are stored (DuckDB's `RowGroupPointer`).
#[derive(Clone, Debug, Default)]
pub struct RowGroupPointer {
    pub row_start: u64,
    pub tuple_count: u64,
    /// The metadata pointer of each column's `PersistentColumnData`.
    pub data_pointers: Vec<MetaBlockPointer>,
    pub deletes_pointers: Vec<MetaBlockPointer>,
    /// Whether `extra_metadata_blocks` lists every metadata block of the columns besides
    /// the ones `data_pointers` point into.
    pub has_metadata_blocks: bool,
    /// Those metadata blocks (often none; set for wide columns with a lot of metadata).
    pub extra_metadata_blocks: Vec<u64>,
    /// Whether `per_column_metadata_blocks` is set (it replaces `extra_metadata_blocks`
    /// when columns are written one by one, `force_column_metadata_reuse`).
    pub has_per_column_metadata_blocks: bool,
    pub per_column_metadata_blocks: PerColumnMetadataBlocks,
}

impl RowGroupPointer {
    /// `RowGroup::Serialize`; with `supports_per_column_writes` the per-column metadata
    /// blocks are written too, flattened into the legacy properties for v1.4 and v1.5.
    pub fn serialize_fields(
        &self,
        serializer: &mut dyn Serializer,
        supports_per_column_writes: bool,
    ) -> Result<()> {
        serializer.write_property(100, "row_start", &self.row_start)?;
        serializer.write_property(101, "tuple_count", &self.tuple_count)?;
        write_meta_pointer_list(serializer, 102, "data_pointers", &self.data_pointers)?;
        write_meta_pointer_list(serializer, 103, "delete_pointers", &self.deletes_pointers)?;
        if serializer.should_serialize(6) && !supports_per_column_writes {
            serializer.write_property(104, "has_metadata_blocks", &self.has_metadata_blocks)?;
            write_u64_list(
                serializer,
                105,
                "extra_metadata_blocks",
                &self.extra_metadata_blocks,
            )?;
        }
        if supports_per_column_writes {
            serializer.write_property(104, "has_metadata_blocks", &self.has_per_column_metadata_blocks)?;
            write_u64_list(
                serializer,
                105,
                "extra_metadata_blocks",
                &self.per_column_metadata_blocks.blocks(),
            )?;
            serializer.write_property(
                106,
                "has_per_column_metadata_blocks",
                &self.has_per_column_metadata_blocks,
            )?;
            let packed: Vec<u64> = self
                .per_column_metadata_blocks
                .data
                .iter()
                .map(|entry| entry.packed())
                .collect();
            write_u64_list(serializer, 107, "per_column_metadata_blocks", &packed)?;
        }
        Ok(())
    }

    /// `RowGroup::Deserialize`.
    pub fn deserialize_fields(deserializer: &mut dyn Deserializer) -> Result<Self> {
        let row_start = deserializer.read_property(100, "row_start")?;
        let tuple_count = deserializer.read_property(101, "tuple_count")?;
        deserializer.on_property_begin(102, "data_pointers")?;
        let data_pointers = read_meta_pointer_list(deserializer)?;
        deserializer.on_property_end()?;
        deserializer.on_property_begin(103, "delete_pointers")?;
        let deletes_pointers = read_meta_pointer_list(deserializer)?;
        deserializer.on_property_end()?;
        let mut has_metadata_blocks = deserializer
            .read_optional(104, "has_metadata_blocks")?
            .unwrap_or(false);
        let mut extra_metadata_blocks: Vec<u64> = deserializer
            .read_optional(105, "extra_metadata_blocks")?
            .unwrap_or_default();
        let has_per_column_metadata_blocks = deserializer
            .read_optional(106, "has_per_column_metadata_blocks")?
            .unwrap_or(false);
        let packed: Vec<u64> = deserializer
            .read_optional(107, "per_column_metadata_blocks")?
            .unwrap_or_default();
        let per_column_metadata_blocks = PerColumnMetadataBlocks {
            data: packed.into_iter().map(PerColumnMetadataBlock::unpack).collect(),
        };
        if has_per_column_metadata_blocks {
            // Per-column metadata supersedes the legacy extra metadata blocks.
            has_metadata_blocks = false;
            extra_metadata_blocks.clear();
        }
        Ok(Self {
            row_start,
            tuple_count,
            data_pointers,
            deletes_pointers,
            has_metadata_blocks,
            extra_metadata_blocks,
            has_per_column_metadata_blocks,
            per_column_metadata_blocks,
        })
    }
}

/// `IndexStorageInfo::Deserialize` (the object's fields) of an ART.
pub fn deserialize_index_storage_info(
    deserializer: &mut dyn Deserializer,
) -> Result<crate::index::art::ArtStorageInfo> {
    use crate::index::fixed_size_allocator::{BufferPointer, FixedSizeAllocatorInfo};
    let name: String = deserializer.read_optional(100, "name")?.unwrap_or_default();
    let root: u64 = deserializer.read_optional(101, "root")?.unwrap_or(0);
    let mut allocator_infos = Vec::new();
    if deserializer.on_optional_property_begin(102, "allocator_infos")? {
        let count = deserializer.on_list_begin()?;
        for _ in 0..count {
            deserializer.on_object_begin()?;
            let segment_size: u64 = deserializer.read_optional(100, "segment_size")?.unwrap_or(0);
            let buffer_ids: Vec<u64> = deserializer.read_optional(101, "buffer_ids")?.unwrap_or_default();
            let mut block_pointers = Vec::new();
            if deserializer.on_optional_property_begin(102, "block_pointers")? {
                let pointers = deserializer.on_list_begin()?;
                for _ in 0..pointers {
                    deserializer.on_object_begin()?;
                    let pointer = BlockPointer::deserialize_fields(deserializer)?;
                    block_pointers.push(BufferPointer {
                        block_id: pointer.block_id,
                        offset: pointer.offset,
                    });
                    deserializer.on_object_end()?;
                }
                deserializer.on_list_end()?;
                deserializer.on_optional_property_end(true)?;
            }
            let segment_counts: Vec<u64> = deserializer
                .read_optional(103, "segment_counts")?
                .unwrap_or_default();
            let allocation_sizes: Vec<u64> = deserializer
                .read_optional(104, "allocation_sizes")?
                .unwrap_or_default();
            let buffers_with_free_space: Vec<u64> = deserializer
                .read_optional(105, "buffers_with_free_space")?
                .unwrap_or_default();
            deserializer.on_object_end()?;
            allocator_infos.push(FixedSizeAllocatorInfo {
                segment_size,
                buffer_ids,
                block_pointers,
                segment_counts,
                allocation_sizes,
                buffers_with_free_space,
            });
        }
        deserializer.on_list_end()?;
        deserializer.on_optional_property_end(true)?;
    }
    let options: rustdb_common::CaseInsensitiveMap<rustdb_types::Value> =
        deserializer.read_optional(103, "options")?.unwrap_or_default();
    Ok(crate::index::art::ArtStorageInfo {
        name,
        root,
        allocator_infos,
        options,
    })
}

/// Writes `values` as list property `field` unless it is empty (`WritePropertyWithDefault`).
fn write_u64_list(
    serializer: &mut dyn Serializer,
    field: u16,
    tag: &'static str,
    values: &[u64],
) -> Result<()> {
    if values.is_empty() {
        return Ok(());
    }
    serializer.write_list_property(field, tag, values.len(), |s, index| {
        s.write_u64(values[index]);
        Ok(())
    })
}

/// `IndexStorageInfo::Serialize` (the object's fields) of an ART.
pub fn serialize_index_storage_info(
    serializer: &mut dyn Serializer,
    info: &crate::index::art::ArtStorageInfo,
) -> Result<()> {
    if !info.name.is_empty() {
        serializer.write_property(100, "name", info.name.as_str())?;
    }
    if info.root != 0 {
        serializer.write_property(101, "root", &info.root)?;
    }
    if !info.allocator_infos.is_empty() {
        serializer.write_list_property(102, "allocator_infos", info.allocator_infos.len(), |s, index| {
            let allocator = &info.allocator_infos[index];
            s.on_object_begin();
            if allocator.segment_size != 0 {
                s.write_property(100, "segment_size", &allocator.segment_size)?;
            }
            write_u64_list(s, 101, "buffer_ids", &allocator.buffer_ids)?;
            if !allocator.block_pointers.is_empty() {
                s.write_list_property(102, "block_pointers", allocator.block_pointers.len(), |s, i| {
                    let pointer = allocator.block_pointers[i];
                    s.on_object_begin();
                    BlockPointer {
                        block_id: pointer.block_id,
                        offset: pointer.offset,
                    }
                    .serialize_fields(s)?;
                    s.on_object_end();
                    Ok(())
                })?;
            }
            write_u64_list(s, 103, "segment_counts", &allocator.segment_counts)?;
            write_u64_list(s, 104, "allocation_sizes", &allocator.allocation_sizes)?;
            write_u64_list(
                s,
                105,
                "buffers_with_free_space",
                &allocator.buffers_with_free_space,
            )?;
            s.on_object_end();
            Ok(())
        })?;
    }
    if !info.options.is_empty() {
        serializer.write_property(103, "options", &info.options)?;
    }
    Ok(())
}
