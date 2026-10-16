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
}

/// A node of the reader tree.
#[derive(Debug)]
pub enum ColumnReader {
    Leaf(Box<LeafReader>),
    List(Box<ListReader>),
    Struct(StructReader),
    /// `RowNumberColumnReader`: the row's index in the file.
    RowNumber(RowNumberReader),
}

/// `RowNumberColumnReader`.
#[derive(Debug, Default)]
pub struct RowNumberReader {
    row_group_offset: i64,
}

impl ColumnReader {
    /// `ColumnReader::CreateReader` / `ParquetReader::CreateReaderRecursive`: readers for
    /// `schema`; `projection` lists the (struct) children to read, empty for all.
    pub fn create(context: &ReaderContext, schema: &ColumnSchema, projection: &[ChildProjection]) -> Result<Self> {
        match schema.kind {
            SchemaKind::FileRowNumber => Ok(Self::RowNumber(RowNumberReader::default())),
            SchemaKind::Geometry | SchemaKind::Variant => Err(Error::not_implemented(format!(
                "Reading {} columns from Parquet files",
                schema.logical_type
            ))),
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
                        let child_schema = schema.children.get(child.index).ok_or_else(|| {
                            Error::internal("projected struct field out of range")
                        })?;
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
                    rustdb_types::LogicalTypeId::Struct => Ok(Self::Struct(StructReader::new(schema, children))),
                    _ => Err(Error::internal("Unsupported schema type for schema with children")),
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
        }
    }

    /// `InitializeRead`: positions the reader at row group `row_group_index`.
    pub fn initialize_read(&mut self, row_group_index: usize, row_group: &RowGroup, context: &ReaderContext) -> Result<()> {
        match self {
            Self::Leaf(leaf) => leaf.initialize_read(row_group),
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
    pub fn read(&mut self, num_values: usize, defines: &mut [u8], repeats: &mut [u8], result: &mut Vector) -> Result<usize> {
        match self {
            Self::Leaf(leaf) => leaf.read(num_values, defines, repeats, result),
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
            Self::List(list) => list.skip(num_values),
            Self::Struct(node) => node.skip(num_values),
            Self::RowNumber(reader) => reader.row_group_offset += num_values as i64,
        }
    }

    /// The entries left in the current row group (`GroupRowsAvailable`).
    pub fn group_rows_available(&self) -> Result<u64> {
        match self {
            Self::Leaf(leaf) => Ok(leaf.group_rows_available()),
            Self::List(list) => list.group_rows_available(),
            Self::Struct(node) => node.group_rows_available(),
            Self::RowNumber(_) => Ok(u64::MAX),
        }
    }

    /// The compressed bytes this reader reads from the current row group.
    pub fn total_compressed_size(&self) -> u64 {
        match self {
            Self::Leaf(leaf) => leaf.total_compressed_size(),
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
