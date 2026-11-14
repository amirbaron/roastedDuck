//! Internal secondary-index hooks for compiled workspace extensions.
//!
//! This is an engine implementation contract, not a public plugin SDK or stable ABI.
//! ART's constraints, deltas and persisted bytes remain in the ART implementation.

use std::any::Any;
use std::collections::BTreeMap;
use std::fmt::Debug;
use std::sync::{Arc, PoisonError, RwLock};

use roastedduck_common::{ByteString, CaseInsensitiveMap, Error, Result, byte_format};
use roastedduck_function::ObjectCache;
use roastedduck_types::{DataChunk, LogicalType, Value};

use super::{Art, IndexAppendMode, IndexConstraintType, IndexKeyExpressions, IndexRemovalType};

/// Validated/bound key expressions and storage metadata for a secondary index.
#[derive(Clone, Debug)]
pub struct SecondaryIndexSpec {
    pub constraint: IndexConstraintType,
    pub name: ByteString,
    pub column_ids: Vec<usize>,
    pub key_types: Vec<LogicalType>,
    pub expressions: IndexKeyExpressions,
    pub options: CaseInsensitiveMap<Value, ByteString>,
    pub persistent: bool,
}

impl SecondaryIndexSpec {
    /// Evaluate keys against the complete physical table-column layout.
    pub fn execute_expressions(&self, table_chunk: &DataChunk) -> Result<DataChunk> {
        match &self.expressions {
            IndexKeyExpressions::Columns(_) => {
                let columns = self
                    .column_ids
                    .iter()
                    .map(|&column| {
                        table_chunk
                            .columns()
                            .get(column)
                            .cloned()
                            .ok_or_else(|| Error::internal("secondary index column out of range"))
                    })
                    .collect::<Result<Vec<_>>>()?;
                Ok(DataChunk::new(columns, table_chunk.len()))
            }
            IndexKeyExpressions::Custom(executor) => executor.execute(table_chunk),
        }
    }
}

/// Non-constraint index lifecycle. Implementations must preserve all-or-nothing
/// batch mutation: returning an error must leave the index unchanged.
pub trait SecondaryIndex: Send + Sync + Debug {
    fn spec(&self) -> &SecondaryIndexSpec;
    fn type_name(&self) -> &str;
    /// Append assigned committed row IDs atomically; never retain borrowed vectors.
    fn append(&mut self, table_chunk: &DataChunk, row_ids: &[i64]) -> Result<()>;
    /// Idempotent compensation after a later index/commit step fails.
    fn revert_append(&mut self, table_chunk: &DataChunk, row_ids: &[i64]) -> Result<usize>;
    /// `MainIndex` retires but retains old-visible rows; `DeletedRowsInUse` releases
    /// them. Revert variants restore the preceding state. Compaction must retain
    /// retired rows until cleanup and repeated compensation must be harmless.
    fn remove(&mut self, table_chunk: &DataChunk, row_ids: &[i64], removal: IndexRemovalType) -> Result<()>;
    /// Empty definition-preserving instance for table row-ID relocation.
    fn empty_like(&self) -> Result<Box<dyn SecondaryIndex>>;
    fn compact(&mut self) -> Result<()>;
    fn reset(&mut self);
    fn in_memory_size(&self) -> usize;
    /// Typed capabilities are used only by compiled engine extensions.
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

/// Storage factory registered in one database instance.
pub trait IndexType: Send + Sync + Debug {
    fn name(&self) -> &str;
    fn create(&self, spec: SecondaryIndexSpec) -> Result<Box<dyn SecondaryIndex>>;
    /// Provider-specific rejection; this registry does not enable persistence.
    fn persistence_error(&self, _settings: Option<&dyn roastedduck_function::SettingsSource>) -> Error {
        Error::not_implemented(byte_format!(
            "Index type {} does not support persistent storage",
            self.name()
        ))
    }
}

/// Database-owned secondary type registry. It has no global state and registers
/// no binary entrypoints. Persisted secondary binding is deliberately absent.
#[derive(Debug, Default)]
pub struct IndexTypeRegistry {
    types: RwLock<BTreeMap<Vec<u8>, Arc<dyn IndexType>>>,
}

const REGISTRY_CACHE_KEY: &str = "roastedduck.internal.index_type_registry.v1";

#[derive(Debug)]
struct AttachedRegistry(Arc<IndexTypeRegistry>);

impl IndexTypeRegistry {
    /// Attach exactly one registry to a database's shared object cache.
    pub fn attach(self: &Arc<Self>, cache: &ObjectCache) -> Result<()> {
        let attached = cache.get_or_insert_with(REGISTRY_CACHE_KEY, || AttachedRegistry(Arc::clone(self)));
        if !Arc::ptr_eq(&attached.0, self) {
            return Err(Error::internal(
                "A different internal index type registry is already attached to this database",
            ));
        }
        Ok(())
    }

    /// Discover this database's registry from its function client's cache.
    pub fn from_cache(cache: &ObjectCache) -> Option<Arc<Self>> {
        cache
            .get::<AttachedRegistry>(REGISTRY_CACHE_KEY)
            .map(|attached| Arc::clone(&attached.0))
    }

    /// Register one case-insensitive type name without replacing a live factory.
    pub fn register(&self, index_type: Arc<dyn IndexType>) -> Result<()> {
        let name = index_type.name().as_bytes().to_ascii_lowercase();
        if name.is_empty() || name == b"art" {
            return Err(Error::invalid_input(
                "secondary index type name must be nonempty and cannot be ART",
            ));
        }
        let mut types = self.types.write().unwrap_or_else(PoisonError::into_inner);
        if types.contains_key(&name) {
            return Err(Error::catalog(byte_format!(
                "Index type {} is already registered",
                index_type.name()
            )));
        }
        types.insert(name, index_type);
        Ok(())
    }

    /// Look up a factory without holding the registry lock while invoking it.
    pub fn create(&self, name: impl AsRef<[u8]>, spec: SecondaryIndexSpec) -> Result<BoundIndex> {
        self.create_with_settings(name, spec, None)
    }

    /// Create with the statement's settings, allowing an exact provider rejection.
    pub fn create_with_settings(
        &self,
        name: impl AsRef<[u8]>,
        spec: SecondaryIndexSpec,
        settings: Option<&dyn roastedduck_function::SettingsSource>,
    ) -> Result<BoundIndex> {
        let name = name.as_ref();
        let factory = self
            .types
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&name.to_ascii_lowercase())
            .cloned()
            .ok_or_else(|| Error::binder(byte_format!("Unknown index type: {name}")))?;
        if spec.persistent {
            return Err(factory.persistence_error(settings));
        }
        let index = factory.create(spec)?;
        if index.spec().constraint != IndexConstraintType::None {
            return Err(Error::binder("secondary indexes cannot enforce constraints"));
        }
        if !index.type_name().eq_ignore_ascii_case(factory.name()) {
            return Err(Error::internal("index factory returned a different index type"));
        }
        Ok(BoundIndex::Secondary(index))
    }

    pub fn contains(&self, name: impl AsRef<[u8]>) -> bool {
        self.types
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .contains_key(&name.as_ref().to_ascii_lowercase())
    }
}

/// Bound-index dispatch keeps ART-specialized capabilities explicit.
#[derive(Debug)]
pub enum BoundIndex {
    Art(Art),
    Secondary(Box<dyn SecondaryIndex>),
}

impl From<Art> for BoundIndex {
    fn from(art: Art) -> Self {
        Self::Art(art)
    }
}

impl BoundIndex {
    pub fn as_art(&self) -> Option<&Art> {
        match self {
            Self::Art(index) => Some(index),
            Self::Secondary(_) => None,
        }
    }
    pub fn as_art_mut(&mut self) -> Option<&mut Art> {
        match self {
            Self::Art(index) => Some(index),
            Self::Secondary(_) => None,
        }
    }
    pub fn as_secondary(&self) -> Option<&dyn SecondaryIndex> {
        match self {
            Self::Art(_) => None,
            Self::Secondary(index) => Some(index.as_ref()),
        }
    }
    pub fn as_secondary_mut(&mut self) -> Option<&mut dyn SecondaryIndex> {
        match self {
            Self::Art(_) => None,
            Self::Secondary(index) => Some(index.as_mut()),
        }
    }
    pub fn name(&self) -> &ByteString {
        match self {
            Self::Art(index) => index.name(),
            Self::Secondary(index) => &index.spec().name,
        }
    }
    pub fn column_ids(&self) -> &[usize] {
        match self {
            Self::Art(index) => index.column_ids(),
            Self::Secondary(index) => &index.spec().column_ids,
        }
    }
    pub fn key_types(&self) -> &[LogicalType] {
        match self {
            Self::Art(index) => index.key_types(),
            Self::Secondary(index) => &index.spec().key_types,
        }
    }
    pub fn expressions(&self) -> &IndexKeyExpressions {
        match self {
            Self::Art(index) => index.expressions(),
            Self::Secondary(index) => &index.spec().expressions,
        }
    }
    pub fn constraint_type(&self) -> IndexConstraintType {
        self.as_art()
            .map_or(IndexConstraintType::None, Art::constraint_type)
    }
    pub fn is_unique(&self) -> bool {
        self.as_art().is_some_and(Art::is_unique)
    }
    pub fn is_primary(&self) -> bool {
        self.as_art().is_some_and(Art::is_primary)
    }
    pub fn is_foreign(&self) -> bool {
        self.as_art().is_some_and(Art::is_foreign)
    }
    pub fn in_memory_size(&self) -> usize {
        match self {
            Self::Art(index) => index.in_memory_size(),
            Self::Secondary(index) => index.in_memory_size(),
        }
    }
    pub fn reset(&mut self) {
        match self {
            Self::Art(index) => index.reset(),
            Self::Secondary(index) => index.reset(),
        }
    }
    pub fn vacuum(&mut self) -> Result<()> {
        match self {
            Self::Art(index) => index.vacuum(),
            Self::Secondary(index) => index.compact(),
        }
    }
    pub fn empty_like(&self) -> Result<Self> {
        match self {
            Self::Art(index) => Ok(Self::Art(index.empty_like()?)),
            Self::Secondary(index) => Ok(Self::Secondary(index.empty_like()?)),
        }
    }
    pub fn append(
        &mut self,
        chunk: &DataChunk,
        rows: &[i64],
        delete_arts: &[&Art],
        mode: IndexAppendMode,
    ) -> Result<()> {
        match self {
            Self::Art(index) => index.append(chunk, rows, delete_arts, mode),
            Self::Secondary(index) => index.append(chunk, rows),
        }
    }
    /// Compensation after append failure or rollback. Secondary indexes must make
    /// repeated compensation harmless, as ART's `try_delete` already does.
    pub fn try_delete(&mut self, chunk: &DataChunk, rows: &[i64]) -> Result<usize> {
        match self {
            Self::Art(index) => index.try_delete(chunk, rows),
            Self::Secondary(index) => index.revert_append(chunk, rows),
        }
    }
}
