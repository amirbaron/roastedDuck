//! roastedDuck's compatible HNSW storage implementation.
//!
//! SQL validation follows the pinned VSS source (b833341, `hnsw_index_plan.cpp`).
//! The graph itself is independent pure Rust. Persisted C++ HNSW remains unbound.

use std::any::Any;
use std::collections::HashSet;
use std::mem::size_of;

use roastedduck_common::{ByteString, Error, Result, byte_format};
use roastedduck_hnsw::{Config, Hnsw, Metric};
use roastedduck_types::{DataChunk, LogicalType, LogicalTypeId, Value};

use super::{IndexConstraintType, IndexRemovalType, IndexType, SecondaryIndex, SecondaryIndexSpec};

pub use roastedduck_hnsw::{GraphStats, LevelStats, Neighbor, SearchResult, SearchStats};

/// Compiled-in factory; registration is owned by each database's VSS loader.
#[derive(Debug, Default)]
pub struct HnswIndexType;

impl IndexType for HnswIndexType {
    fn name(&self) -> &'static str {
        "HNSW"
    }
    fn create(&self, spec: SecondaryIndexSpec) -> Result<Box<dyn SecondaryIndex>> {
        Ok(Box::new(HnswIndex::new(spec)?))
    }
    fn persistence_error(&self, settings: Option<&dyn roastedduck_function::SettingsSource>) -> Error {
        let enabled = settings
            .and_then(|settings| {
                settings
                    .current_setting("hnsw_enable_experimental_persistence")
                    .ok()
            })
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        if enabled {
            Error::not_implemented("Index type HNSW does not support persistent storage")
        } else {
            Error::binder(
                "HNSW indexes can only be created in in-memory databases, or when the configuration option 'hnsw_enable_experimental_persistence' is set to true.",
            )
        }
    }
}

/// A graph and its table-key binding. Retired rows stay active in the graph until
/// MVCC cleanup; fetch is authoritative for a querying transaction's visibility.
#[derive(Debug)]
pub struct HnswIndex {
    spec: SecondaryIndexSpec,
    graph: Hnsw,
    retired: HashSet<i64>,
}

impl HnswIndex {
    /// Validate options/types before allocating graph storage or changing a table.
    pub fn new(spec: SecondaryIndexSpec) -> Result<Self> {
        if spec.persistent {
            return Err(Error::not_implemented(
                "Index type HNSW does not support persistent storage",
            ));
        }
        let config = Self::parse_config(&spec)?;
        if spec.constraint != IndexConstraintType::None {
            return Err(Error::not_implemented(
                "HNSW indexes do not support unique or primary key constraints",
            ));
        }
        Ok(Self {
            spec,
            graph: Hnsw::new(config)?,
            retired: HashSet::new(),
        })
    }

    fn integer_option(value: &Value, name: &str, minimum: i32) -> Result<usize> {
        if value.logical_type() != &LogicalType::INTEGER {
            return Err(Error::binder(format!("HNSW index '{name}' must be an integer")));
        }
        let value = value
            .as_i32()
            .ok_or_else(|| Error::internal("Calling GetValueInternal on a value that is NULL"))?;
        if value < minimum {
            return Err(Error::binder(format!(
                "HNSW index '{name}' must be at least {minimum}"
            )));
        }
        Ok(value as usize)
    }

    fn parse_config(spec: &SecondaryIndexSpec) -> Result<Config> {
        let mut metric = Metric::L2Squared;
        let mut m = None;
        let mut m0 = None;
        let mut construction = None;
        let mut search = None;
        // Upstream validates options before the number/type of key expressions.
        for (name, value) in spec.options.iter() {
            if name.eq_ignore_ascii_case("metric") {
                if value.logical_type() != &LogicalType::VARCHAR {
                    return Err(Error::binder("HNSW index 'metric' must be a string"));
                }
                // GetValue<string>() uses Value::ToString, including typed NULL.
                let value = value.to_varchar_bytes()?;
                metric = if value.eq_ignore_ascii_case("l2sq") {
                    Metric::L2Squared
                } else if value.eq_ignore_ascii_case("cosine") {
                    Metric::Cosine
                } else if value.eq_ignore_ascii_case("ip") {
                    Metric::InnerProduct
                } else {
                    // Confirmed by the official 1.5.6 oracle in
                    // vss-oracle-probes-1/invalid_metric.stderr.
                    return Err(Error::binder(
                        "HNSW index 'metric' must be one of: 'ip', 'cosine', 'l2sq'",
                    ));
                };
            } else if name.eq_ignore_ascii_case("ef_construction") {
                construction = Some(Self::integer_option(value, "ef_construction", 1)?);
            } else if name.eq_ignore_ascii_case("ef_search") {
                search = Some(Self::integer_option(value, "ef_search", 1)?);
            } else if name.eq_ignore_ascii_case("M") {
                m = Some(Self::integer_option(value, "M", 2)?);
            } else if name.eq_ignore_ascii_case("M0") {
                m0 = Some(Self::integer_option(value, "M0", 2)?);
            } else {
                return Err(Error::binder(byte_format!(
                    "Unknown option for HNSW index: '{name}'"
                )));
            }
        }
        if spec.key_types.len() != 1 {
            return Err(Error::binder(
                "HNSW indexes can only be created over a single column of keys.",
            ));
        }
        let Some((child, dimensions)) = spec.key_types[0].array_child() else {
            return Err(Error::binder("HNSW index keys must be of type FLOAT[N]"));
        };
        if child.id() != LogicalTypeId::Float {
            return Err(Error::binder("HNSW index key type must be one of: 'FLOAT[N]'"));
        }
        let mut config = Config::new(dimensions as usize, metric);
        if let Some(m) = m {
            config.m = m;
            config.m0 = m.checked_mul(2).ok_or_else(|| {
                Error::invalid_input("HNSW configuration exceeds addressable allocation size")
            })?;
        }
        if let Some(m0) = m0 {
            config.m0 = m0;
        }
        if let Some(ef) = construction {
            config.ef_construction = ef;
        }
        if let Some(ef) = search {
            config.ef_search = ef;
        }
        config.validate()?;
        Ok(config)
    }

    pub fn spec(&self) -> &SecondaryIndexSpec {
        &self.spec
    }
    pub fn name(&self) -> &ByteString {
        &self.spec.name
    }
    pub fn dimensions(&self) -> usize {
        self.graph.config().dimensions
    }
    pub fn metric(&self) -> &'static str {
        match self.graph.config().metric {
            Metric::L2Squared => "l2sq",
            Metric::Cosine => "cosine",
            Metric::InnerProduct => "ip",
        }
    }
    pub fn config(&self) -> &Config {
        self.graph.config()
    }
    pub fn contains(&self, label: i64) -> bool {
        self.graph.contains(label)
    }
    pub fn is_retired(&self, label: i64) -> bool {
        self.retired.contains(&label)
    }
    /// Includes retired rows still available to an older transaction.
    pub fn searchable_count(&self) -> usize {
        self.graph.len()
    }
    pub fn active_count(&self) -> usize {
        self.graph.len() - self.retired.len()
    }
    pub fn retired_count(&self) -> usize {
        self.retired.len()
    }

    /// Graph candidates only. The caller fetches through `DataTable`'s MVCC path,
    /// applies residual predicates, and evaluates SQL scores with core functions.
    /// Cosine navigation follows the calibrated FLOAT kernel clamp; the caller
    /// still evaluates SQL scores with the engine's existing metric functions.
    pub fn search(&self, query: &[f32], limit: usize, ef_override: Option<usize>) -> Result<SearchResult> {
        self.graph.search(query, limit, ef_override)
    }

    /// Actual graph allocation counts include old-visible retired nodes.
    pub fn stats(&self) -> GraphStats {
        self.graph.stats()
    }

    /// Fully removed labels are compacted; retired old-visible labels remain live
    /// in the graph and are therefore retained automatically by its rebuild.
    pub fn compact(&mut self) -> Result<()> {
        if self.graph.stats().deleted == 0 {
            return Ok(());
        }
        self.graph.compact()
    }

    fn keys(&self, table_chunk: &DataChunk, row_ids: &[i64]) -> Result<Vec<(i64, Vec<f32>)>> {
        if table_chunk.len() != row_ids.len() {
            return Err(Error::internal(
                "HNSW index row identifier count does not match input",
            ));
        }
        let keys = self.spec.execute_expressions(table_chunk)?;
        if keys.column_count() != 1
            || keys.len() != row_ids.len()
            || keys.column(0).logical_type() != &self.spec.key_types[0]
        {
            return Err(Error::internal(
                "HNSW key executor returned an incompatible chunk",
            ));
        }
        let arrays = keys.column(0).flattened(keys.len());
        let child = arrays
            .array_child()
            .ok_or_else(|| Error::internal("HNSW array child is missing"))?;
        let child = child.unified::<f32>();
        let dimensions = self.dimensions();
        let mut result = Vec::with_capacity(keys.len());
        for (row, &label) in row_ids.iter().enumerate() {
            if arrays.is_null(row) {
                continue;
            }
            let offset = row * dimensions;
            // The pinned Construct/build task checks the ARRAY validity, but does
            // not inspect child validity: it passes raw FLOAT payloads to its graph.
            // Preserve that mechanism, rather than adding a new NULL-element ban.
            let mut vector = Vec::with_capacity(dimensions);
            for coordinate in offset..offset + dimensions {
                let slot = child.slot(coordinate);
                vector.push(*child.values.get(slot).ok_or_else(|| {
                    Error::internal("HNSW array payload is shorter than its declared shape")
                })?);
            }
            result.push((label, vector));
        }
        Ok(result)
    }
}

impl SecondaryIndex for HnswIndex {
    fn spec(&self) -> &SecondaryIndexSpec {
        &self.spec
    }
    fn type_name(&self) -> &'static str {
        "HNSW"
    }
    fn append(&mut self, table_chunk: &DataChunk, row_ids: &[i64]) -> Result<()> {
        let keys = self.keys(table_chunk, row_ids)?;
        self.graph.insert_batch(&keys)
    }
    fn revert_append(&mut self, _table_chunk: &DataChunk, row_ids: &[i64]) -> Result<usize> {
        for row in row_ids {
            self.retired.remove(row);
        }
        Ok(self.graph.remove_batch(row_ids))
    }
    fn remove(&mut self, table_chunk: &DataChunk, row_ids: &[i64], removal: IndexRemovalType) -> Result<()> {
        match removal {
            IndexRemovalType::MainIndex => {
                for &row in row_ids {
                    if self.graph.contains(row) {
                        self.retired.insert(row);
                    }
                }
            }
            IndexRemovalType::MainIndexOnly => {
                for row in row_ids {
                    self.retired.remove(row);
                }
                self.graph.remove_batch(row_ids);
            }
            IndexRemovalType::RevertMainIndex => {
                for row in row_ids {
                    self.retired.remove(row);
                }
            }
            IndexRemovalType::RevertMainIndexOnly => {
                let mut keys = self.keys(table_chunk, row_ids)?;
                keys.retain(|(label, _)| !self.graph.contains(*label));
                self.graph.insert_batch(&keys)?;
            }
            IndexRemovalType::DeletedRowsInUse => {
                for &row in row_ids {
                    if self.retired.remove(&row) {
                        self.graph.remove(row);
                    }
                }
            }
        }
        Ok(())
    }
    fn empty_like(&self) -> Result<Box<dyn SecondaryIndex>> {
        Ok(Box::new(Self::new(self.spec.clone())?))
    }
    fn compact(&mut self) -> Result<()> {
        Self::compact(self)
    }
    fn reset(&mut self) {
        self.graph.clear();
        self.retired.clear();
    }
    fn in_memory_size(&self) -> usize {
        self.graph.stats().approx_memory_usage + self.retired.capacity() * (size_of::<i64>() + 1)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}
