//! Rows a parsed statement carries along (DuckDB's `ColumnDataCollection` as a
//! `ColumnDataRef` holds it, e.g. the rows an appender inserts).

use std::fmt;
use std::sync::Arc;

use rustdb_serde::{DeserializeObject, Deserializer, Result, SerializeObject, Serializer};

use crate::{DataChunk, LogicalType, STANDARD_VECTOR_SIZE, Value, Vector};

/// Rows of fixed column types in chunks, shared by the copies of the statement holding
/// them.
#[derive(Clone, Debug)]
pub struct ColumnDataCollection {
    types: Vec<LogicalType>,
    chunks: Arc<[DataChunk]>,
}

impl ColumnDataCollection {
    /// A collection of `chunks`, whose columns have `types`.
    pub fn new(types: Vec<LogicalType>, chunks: Vec<DataChunk>) -> Self {
        Self {
            types,
            chunks: chunks.into(),
        }
    }

    pub fn types(&self) -> &[LogicalType] {
        &self.types
    }

    pub fn chunks(&self) -> &Arc<[DataChunk]> {
        &self.chunks
    }

    /// The number of rows (`ColumnDataCollection::Count`).
    pub fn count(&self) -> usize {
        self.chunks.iter().map(DataChunk::len).sum()
    }

    /// The values of column `column`, row by row.
    fn column_values(&self, column: usize) -> impl Iterator<Item = Value> + '_ {
        self.chunks.iter().flat_map(move |chunk| {
            let vector = chunk.column(column);
            (0..chunk.len()).map(move |row| vector.get_value(row))
        })
    }
}

impl PartialEq for ColumnDataCollection {
    /// Same types and the same values in the same order (`ColumnDataCollection::ResultEquals`).
    fn eq(&self, other: &Self) -> bool {
        self.types == other.types
            && self.count() == other.count()
            && (0..self.types.len()).all(|column| self.column_values(column).eq(other.column_values(column)))
    }
}

impl fmt::Display for ColumnDataCollection {
    /// `ColumnDataCollection::ToString`: a header, then every chunk as
    /// `DataChunk::ToString` prints it (the scanned vectors are flat).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "ColumnDataCollection - [{} Chunks, {} Rows]",
            self.chunks.len(),
            self.count()
        )?;
        let mut row_count = 0;
        for (chunk_idx, chunk) in self.chunks.iter().enumerate() {
            let size = chunk.len();
            writeln!(f, "Chunk {chunk_idx} - [Rows {row_count} - {}]", row_count + size)?;
            writeln!(f, "Chunk - [{} Columns]", self.types.len())?;
            for (column, ty) in self.types.iter().enumerate() {
                let vector = chunk.column(column);
                let values = (0..size)
                    .map(|row| vector.get_value(row).to_varchar().map_err(|_| fmt::Error))
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                writeln!(f, "- FLAT {ty}: {size} = [ {}]", values.join(", "))?;
            }
            row_count += size;
        }
        Ok(())
    }
}

impl SerializeObject for ColumnDataCollection {
    /// `ColumnDataCollection::Serialize`: the types, then the values column by column.
    fn serialize_fields(&self, serializer: &mut dyn Serializer) -> Result<()> {
        let values: Vec<Vec<Value>> = (0..self.types.len())
            .map(|column| self.column_values(column).collect())
            .collect();
        serializer.write_property(100, "types", &self.types)?;
        serializer.write_property(101, "values", &values)
    }
}

impl DeserializeObject for ColumnDataCollection {
    /// `ColumnDataCollection::Deserialize`: the rows in chunks of `STANDARD_VECTOR_SIZE`.
    fn deserialize_fields(deserializer: &mut dyn Deserializer) -> Result<Self> {
        let types: Vec<LogicalType> = deserializer.read_property(100, "types")?;
        let values: Vec<Vec<Value>> = deserializer.read_property(101, "values")?;
        let rows = values.first().map_or(0, Vec::len);
        let mut chunks = Vec::new();
        for start in (0..rows).step_by(STANDARD_VECTOR_SIZE) {
            let count = (rows - start).min(STANDARD_VECTOR_SIZE);
            let mut columns = Vec::with_capacity(types.len());
            for (ty, column_values) in types.iter().zip(&values) {
                let mut vector = Vector::flat(ty.clone(), count);
                for (row, value) in column_values[start..start + count].iter().enumerate() {
                    vector
                        .set_value(row, value)
                        .map_err(|error| rustdb_serde::Error::internal(error.to_string()))?;
                }
                columns.push(vector);
            }
            chunks.push(DataChunk::new(columns, count));
        }
        Ok(Self::new(types, chunks))
    }
}

rustdb_serde::impl_pointee_serde!(ColumnDataCollection);
