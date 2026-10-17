//! Column readers (DuckDB's `ColumnReader` hierarchy): a tree mirroring the column
//! schema whose leaves decode pages of one column chunk and whose inner nodes assemble
//! LIST and STRUCT values from definition and repetition levels.

mod leaf;
mod nested;
pub mod values;

use std::sync::Arc;

use rustdb_common::{Error, Result};
use rustdb_types::{LogicalType, Vector};

pub use leaf::LeafReader;
pub use nested::{ListReader, StructReader};

use crate::file::ParquetFile;
use crate::schema::{ColumnSchema, SchemaKind};
use crate::thrift::{FileMetaData, RowGroup};

/// What every reader of a file shares.
#[derive(Debug, Clone)]
pub struct ReaderContext {
    pub file: Arc<ParquetFile>,
    pub file_name: Arc<str>,
    pub metadata: Arc<FileMetaData>,
    /// How to decrypt the pages of an encrypted file.
    pub decryption: Option<Arc<crate::crypto::FileDecryption>>,
}

/// A node of the reader tree.
#[derive(Debug)]
pub enum ColumnReader {
    Leaf(Box<LeafReader>),
    List(Box<ListReader>),
    Struct(StructReader),
    /// `RowNumberColumnReader`: the row's index in the file.
    RowNumber(RowNumberReader),
    /// `GeometryColumnReader`: WKB values validated and made little-endian
    /// (`ST_GeomFromWKB` over a BLOB column).
    Geometry(Box<GeometryReader>),
    /// `VariantColumnReader`.
    Variant(Box<VariantReader>),
}

/// The reader of a VARIANT column (`VariantColumnReader`): its `metadata`, `value` and
/// optional `typed_value` children.
#[derive(Debug)]
pub struct VariantReader {
    children: Vec<ColumnReader>,
    metadata_index: usize,
    value_index: usize,
    max_define: u8,
}

impl VariantReader {
    fn read(
        &mut self,
        num_values: usize,
        defines: &mut [u8],
        repeats: &mut [u8],
        result: &mut Vector,
    ) -> Result<usize> {
        // The children may leave the levels untouched when all their values are valid.
        defines[..num_values].fill(self.max_define);
        let mut vectors = Vec::with_capacity(self.children.len());
        let mut counts = Vec::with_capacity(self.children.len());
        for child in &mut self.children {
            let mut vector = Vector::flat(
                child.logical_type(),
                rustdb_common::STANDARD_VECTOR_SIZE.max(num_values),
            );
            counts.push(child.read(num_values, defines, repeats, &mut vector)?);
            vectors.push(vector);
        }
        let (metadata_count, value_count) = (counts[self.metadata_index], counts[self.value_index]);
        if metadata_count != value_count {
            return Err(Error::invalid_input(
                "The Variant column did not contain the same amount of values for 'metadata' and 'value'",
            ));
        }
        let typed_value = vectors.get(2);
        if counts.get(2).is_some_and(|&count| count != value_count) {
            return Err(Error::invalid_input(
                "The shredded Variant column did not contain the same amount of values for 'typed_value' and 'value'",
            ));
        }
        crate::variant::read::convert_column(
            &vectors[self.metadata_index],
            &vectors[self.value_index],
            typed_value,
            num_values,
            result,
        )?;
        Ok(value_count)
    }
}

/// The reader of a GEOMETRY column (`ExpressionColumnReader` with `ST_GeomFromWKB`).
#[derive(Debug)]
pub struct GeometryReader {
    blob: LeafReader,
    logical_type: LogicalType,
    /// The BLOB values of the last read.
    scratch: Option<Vector>,
}

impl GeometryReader {
    fn read(
        &mut self,
        num_values: usize,
        defines: &mut [u8],
        repeats: &mut [u8],
        result: &mut Vector,
    ) -> Result<usize> {
        let mut blobs = self.scratch.take().unwrap_or_else(|| {
            Vector::flat(
                self.blob.logical_type().clone(),
                rustdb_common::STANDARD_VECTOR_SIZE,
            )
        });
        let read = self.blob.read(num_values, defines, repeats, &mut blobs)?;
        if result.logical_type() != &self.logical_type || result.kind() != rustdb_types::VectorKind::Flat {
            *result = Vector::flat(self.logical_type.clone(), rustdb_common::STANDARD_VECTOR_SIZE);
        }
        let capacity = result.capacity();
        for row in 0..read {
            if blobs.is_null(row) {
                result.validity_mut().set_invalid(row, capacity);
                continue;
            }
            result.validity_mut().set_valid(row);
            let converted =
                rustdb_types::geometry::from_binary(blobs.string_bytes(row), true)?.unwrap_or_default();
            let stored = result.add_string(&converted);
            result.values_mut::<rustdb_types::StringT>()[row] = stored;
        }
        self.scratch = Some(blobs);
        Ok(read)
    }
}

/// `RowNumberColumnReader`.
#[derive(Debug, Default)]
pub struct RowNumberReader {
    row_group_offset: i64,
}

impl ColumnReader {
    /// `ColumnReader::CreateReader` / `ParquetReader::CreateReaderRecursive`: readers for
    /// `schema`; `projection` lists the (struct) children to read, empty for all.
    pub fn create(
        context: &ReaderContext,
        schema: &ColumnSchema,
        projection: &[ChildProjection],
    ) -> Result<Self> {
        match schema.kind {
            SchemaKind::FileRowNumber => Ok(Self::RowNumber(RowNumberReader::default())),
            SchemaKind::Geometry => {
                let blob = schema
                    .children
                    .first()
                    .ok_or_else(|| Error::internal("GEOMETRY column without its BLOB child"))?;
                Ok(Self::Geometry(Box::new(GeometryReader {
                    blob: LeafReader::new(context, blob)?,
                    logical_type: schema.logical_type.clone(),
                    scratch: None,
                })))
            }
            SchemaKind::Variant => {
                let child = |name: &str| schema.children.iter().position(|child| child.name == name);
                let (Some(metadata_index), Some(value_index)) = (child("metadata"), child("value")) else {
                    return Err(Error::internal(
                        "The Variant column must have 'metadata' and 'value' as the first two columns",
                    ));
                };
                if metadata_index > 1 || value_index > 1 {
                    return Err(Error::internal(
                        "The Variant column must have 'metadata' and 'value' as the first two columns",
                    ));
                }
                let children = schema
                    .children
                    .iter()
                    .map(|child| Self::create(context, child, &[]))
                    .collect::<Result<Vec<_>>>()?;
                Ok(Self::Variant(Box::new(VariantReader {
                    children,
                    metadata_index,
                    value_index,
                    max_define: schema.max_define,
                })))
            }
            SchemaKind::Column => {
                if schema.children.is_empty() {
                    return Ok(Self::Leaf(Box::new(LeafReader::new(context, schema)?)));
                }
                let mut children: Vec<Option<ColumnReader>> = Vec::with_capacity(schema.children.len());
                if projection.is_empty() {
                    for child in &schema.children {
                        children.push(Some(Self::create(context, child, &[])?));
                    }
                } else {
                    children.resize_with(schema.children.len(), || None);
                    for child in projection {
                        let child_schema = schema
                            .children
                            .get(child.index)
                            .ok_or_else(|| Error::internal("projected struct field out of range"))?;
                        children[child.index] = Some(Self::create(context, child_schema, &child.children)?);
                    }
                }
                match schema.logical_type.id() {
                    rustdb_types::LogicalTypeId::List | rustdb_types::LogicalTypeId::Map => {
                        let child = children
                            .into_iter()
                            .next()
                            .flatten()
                            .ok_or_else(|| Error::internal("LIST reader without a child"))?;
                        Ok(Self::List(Box::new(ListReader::new(schema, child))))
                    }
                    rustdb_types::LogicalTypeId::Struct => {
                        Ok(Self::Struct(StructReader::new(schema, children)))
                    }
                    _ => Err(Error::internal(
                        "Unsupported schema type for schema with children",
                    )),
                }
            }
        }
    }

    /// The DuckDB type this reader produces.
    pub fn logical_type(&self) -> LogicalType {
        match self {
            Self::Leaf(leaf) => leaf.logical_type().clone(),
            Self::List(list) => list.logical_type().clone(),
            Self::Struct(node) => node.logical_type().clone(),
            Self::RowNumber(_) => LogicalType::BIGINT,
            Self::Geometry(reader) => reader.logical_type.clone(),
            Self::Variant(_) => LogicalType::variant(),
        }
    }

    /// `InitializeRead`: positions the reader at row group `row_group_index`.
    pub fn initialize_read(
        &mut self,
        row_group_index: usize,
        row_group: &RowGroup,
        context: &ReaderContext,
    ) -> Result<()> {
        match self {
            Self::Leaf(leaf) => leaf.initialize_read(row_group),
            Self::Geometry(reader) => reader.blob.initialize_read(row_group),
            Self::Variant(reader) => reader
                .children
                .iter_mut()
                .try_for_each(|child| child.initialize_read(row_group_index, row_group, context)),
            Self::List(list) => list.initialize_read(row_group_index, row_group, context),
            Self::Struct(node) => node.initialize_read(row_group_index, row_group, context),
            Self::RowNumber(reader) => {
                reader.row_group_offset = context
                    .metadata
                    .row_groups
                    .iter()
                    .take(row_group_index)
                    .map(|group| group.num_rows)
                    .sum();
                Ok(())
            }
        }
    }

    /// Reads `num_values` entries into `result`, writing their levels to
    /// `defines`/`repeats` (`Read`). Returns the number of entries read.
    pub fn read(
        &mut self,
        num_values: usize,
        defines: &mut [u8],
        repeats: &mut [u8],
        result: &mut Vector,
    ) -> Result<usize> {
        match self {
            Self::Leaf(leaf) => leaf.read(num_values, defines, repeats, result),
            Self::Geometry(reader) => reader.read(num_values, defines, repeats, result),
            Self::Variant(reader) => reader.read(num_values, defines, repeats, result),
            Self::List(list) => list.read(num_values, defines, repeats, result),
            Self::Struct(node) => node.read(num_values, defines, repeats, result),
            Self::RowNumber(reader) => {
                let values = result.values_mut::<i64>();
                for value in &mut values[..num_values] {
                    *value = reader.row_group_offset;
                    reader.row_group_offset += 1;
                }
                Ok(num_values)
            }
        }
    }

    /// Skips `num_values` entries, lazily (`Skip`).
    pub fn skip(&mut self, num_values: usize) {
        match self {
            Self::Leaf(leaf) => leaf.skip(num_values),
            Self::Geometry(reader) => reader.blob.skip(num_values),
            Self::Variant(reader) => reader
                .children
                .iter_mut()
                .for_each(|child| child.skip(num_values)),
            Self::List(list) => list.skip(num_values),
            Self::Struct(node) => node.skip(num_values),
            Self::RowNumber(reader) => reader.row_group_offset += num_values as i64,
        }
    }

    /// The entries left in the current row group (`GroupRowsAvailable`).
    pub fn group_rows_available(&self) -> Result<u64> {
        match self {
            Self::Leaf(leaf) => Ok(leaf.group_rows_available()),
            Self::Geometry(reader) => Ok(reader.blob.group_rows_available()),
            Self::Variant(reader) => reader
                .children
                .first()
                .ok_or_else(|| Error::internal("No projected columns in struct?"))?
                .group_rows_available(),
            Self::List(list) => list.group_rows_available(),
            Self::Struct(node) => node.group_rows_available(),
            Self::RowNumber(_) => Ok(u64::MAX),
        }
    }

    /// The compressed bytes this reader reads from the current row group.
    pub fn total_compressed_size(&self) -> u64 {
        match self {
            Self::Leaf(leaf) => leaf.total_compressed_size(),
            Self::Geometry(reader) => reader.blob.total_compressed_size(),
            Self::Variant(reader) => reader
                .children
                .iter()
                .map(ColumnReader::total_compressed_size)
                .sum(),
            Self::List(list) => list.total_compressed_size(),
            Self::Struct(node) => node.total_compressed_size(),
            Self::RowNumber(_) => 0,
        }
    }
}

/// The struct fields to read below a column (DuckDB's nested `ColumnIndex`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChildProjection {
    pub index: usize,
    pub children: Vec<ChildProjection>,
}
