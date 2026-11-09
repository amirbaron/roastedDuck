//! What rendering needs of the connection (the parts of DuckDB's `ClientContext` the
//! renderers use): casting result vectors to text and noticing interrupts.

use roastedduck::{Connection, LogicalType, Result, Vector};

/// The connection a result renders with.
pub trait RenderContext {
    /// The first `count` rows of `vector` cast to VARCHAR, `None` for NULL
    /// (`VectorOperations::Cast(context, ..., VARCHAR)`).
    fn cast_to_varchar(&self, vector: &Vector, count: usize) -> Result<Vec<Option<String>>>;

    /// Whether the query was interrupted (`ClientContext::IsInterrupted`).
    fn is_interrupted(&self) -> bool;
}

impl RenderContext for Connection {
    fn cast_to_varchar(&self, vector: &Vector, count: usize) -> Result<Vec<Option<String>>> {
        let strings = self.cast_vector(vector, &LogicalType::VARCHAR, count)?;
        Ok(string_column(&strings, count))
    }

    fn is_interrupted(&self) -> bool {
        self.client_handle().is_interrupted()
    }
}

/// The values of a VARCHAR vector, `None` for NULL.
pub fn string_column(vector: &Vector, count: usize) -> Vec<Option<String>> {
    (0..count)
        .map(|row| {
            (!vector.is_null(row)).then(|| String::from_utf8_lossy(vector.string_bytes(row)).into_owned())
        })
        .collect()
}
