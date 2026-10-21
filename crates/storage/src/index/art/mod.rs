//! The adaptive radix tree (DuckDB's `ART`, `src/execution/index/art`).
//!
//! Keys map to row ids. A key with one row id ends in an inlined leaf; a key with several
//! row ids (non-unique indexes, or transiently a unique index during commit) ends in a
//! *gate*: the root of a nested ART over the row ids' 8-byte keys, whose last byte lives
//! in `Node7Leaf`/`Node15Leaf`/`Node256Leaf` nodes. All nodes are stored in DuckDB's byte
//! layout (see [`node`]) in fixed-size allocators, so the index serializes as is.

mod iterator;
mod key;
mod leaf;
pub mod node;
mod nodes;
mod operator;
mod prefix;
mod storage;
#[cfg(test)]
mod tests;
mod vacuum;

use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;

use rustdb_buffer::BufferManager;
use rustdb_common::{Error, ErrorKind, Result};
use rustdb_types::{DataChunk, LogicalType, Value};

pub use iterator::{ArtIterator, RowIdSetOutput, ScanOutput, ScanResult};
pub use key::{
    ArtKey, ROW_ID_SIZE, create_key, decode_row_id, generate_keys, is_supported_key_type, row_id_key,
};
pub use node::{GateStatus, NType, Node, Slot};
pub use storage::{ArtStorageInfo, deprecated_index_block_ids};

use self::node::{ALLOCATOR_COUNT, read_node, write_node};
use super::conflict_manager::ConflictManager;
use super::fixed_size_allocator::{FixedSizeAllocator, FixedSizeAllocatorInfo};
use super::{IndexAppendMode, IndexConstraintType, VerifyExistenceType};
use crate::table_filter::TableFilter;
use rustdb_types::compare::CompareOp;

/// `Prefix::METADATA_SIZE`: the count byte and the child pointer.
pub const PREFIX_METADATA_SIZE: usize = 9;
/// `Prefix::DEPRECATED_COUNT`.
pub const DEPRECATED_PREFIX_COUNT: u8 = 15;

/// Evaluates index key expressions that are not plain column references (DuckDB binds
/// these with an `ExpressionExecutor`; the binder provides an implementation).
pub trait IndexKeyExecutor: Send + Sync + fmt::Debug {
    /// The key columns of the rows of `table_chunk`, which holds every table column.
    fn execute(&self, table_chunk: &DataChunk) -> Result<DataChunk>;
    /// The display names of the key expressions (`Expression::GetName`).
    fn names(&self) -> &[String];

    /// The predicates an index scan answers for the pushed-down `filter` on the one column
    /// the key expression reads, if it compares the key expression with a constant
    /// (`ART::TryInitializeScan` on the filter's `ToExpression`).
    fn scan_predicates(&self, filter: &TableFilter) -> Option<Vec<IndexScanPredicate>> {
        let _ = filter;
        None
    }
}

/// How an index computes its key columns from a table chunk.
#[derive(Clone, Debug)]
pub enum IndexKeyExpressions {
    /// Plain column references: key column `i` is table column `column_ids[i]`, displayed
    /// as `names[i]`.
    Columns(Vec<String>),
    /// Arbitrary expressions.
    Custom(Arc<dyn IndexKeyExecutor>),
}

/// The outcome of inserting one key (`ARTConflictType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConflictType {
    NoConflict,
    Constraint,
}

/// The storage version from which GEOMETRY keys use their new representation.
const GEOMETRY_KEY_STORAGE_VERSION: u64 = 7;

/// An adaptive radix tree index.
#[derive(Debug)]
pub struct Art {
    name: String,
    constraint: IndexConstraintType,
    column_ids: Vec<usize>,
    key_types: Vec<LogicalType>,
    expressions: IndexKeyExpressions,
    pub(crate) tree: Node,
    pub(crate) allocators: Vec<FixedSizeAllocator>,
    pub(crate) prefix_count: u8,
    storage_version: Option<u64>,
    buffer_manager: BufferManager,
    block_size: usize,
}

impl Art {
    /// An empty ART over the key columns `column_ids` (physical table columns) with key
    /// types `key_types`.
    pub fn new(
        name: impl Into<String>,
        constraint: IndexConstraintType,
        column_ids: Vec<usize>,
        key_types: Vec<LogicalType>,
        expressions: IndexKeyExpressions,
        buffer_manager: BufferManager,
        block_size: usize,
    ) -> Result<Self> {
        for ty in &key_types {
            if !is_supported_key_type(ty) {
                return Err(key::invalid_key_type(ty));
            }
        }
        let compound_size: usize = key_types.iter().map(|t| t.physical_type().size()).sum();
        let prefix_count = Self::default_prefix_count(compound_size);
        let mut art = Self {
            name: name.into(),
            constraint,
            column_ids,
            key_types,
            expressions,
            tree: Node::default(),
            allocators: Vec::with_capacity(ALLOCATOR_COUNT),
            prefix_count,
            storage_version: None,
            buffer_manager,
            block_size,
        };
        art.init_allocators()?;
        Ok(art)
    }

    /// `ART::SetPrefixCount` for a new index: the aligned compound key size minus one,
    /// capped so that a prefix segment stays below 256 bytes.
    fn default_prefix_count(compound_size: usize) -> u8 {
        let max_aligned = ((255 - PREFIX_METADATA_SIZE) / 8 * 8 - 1) as u8;
        let key_aligned = compound_size.div_ceil(8) * 8;
        let key_aligned = key_aligned.saturating_sub(1);
        if key_aligned > usize::from(max_aligned) {
            max_aligned
        } else {
            key_aligned as u8
        }
    }

    fn init_allocators(&mut self) -> Result<()> {
        self.allocators.clear();
        let prefix_size = usize::from(self.prefix_count) + PREFIX_METADATA_SIZE;
        let sizes = [
            prefix_size,
            NType::Leaf.segment_size(),
            NType::Node4.segment_size(),
            NType::Node16.segment_size(),
            NType::Node48.segment_size(),
            NType::Node256.segment_size(),
            NType::Node7Leaf.segment_size(),
            NType::Node15Leaf.segment_size(),
            NType::Node256Leaf.segment_size(),
        ];
        for size in sizes {
            self.allocators.push(FixedSizeAllocator::new(
                size,
                self.block_size,
                self.buffer_manager.clone(),
            )?);
        }
        Ok(())
    }

    /// An empty ART with the same definition (`CreateDeltaIndex`); the index of deleted
    /// rows still in use allows duplicates.
    pub fn delta(&self, allow_duplicates: bool) -> Result<Self> {
        let constraint = if allow_duplicates {
            IndexConstraintType::None
        } else {
            self.constraint
        };
        let mut art = Self::new(
            self.name.clone(),
            constraint,
            self.column_ids.clone(),
            self.key_types.clone(),
            self.expressions.clone(),
            self.buffer_manager.clone(),
            self.block_size,
        )?;
        art.storage_version = self.storage_version;
        Ok(art)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn constraint_type(&self) -> IndexConstraintType {
        self.constraint
    }

    /// Whether the index enforces uniqueness (PRIMARY KEY or UNIQUE).
    pub fn is_unique(&self) -> bool {
        matches!(
            self.constraint,
            IndexConstraintType::Unique | IndexConstraintType::Primary
        )
    }

    pub fn is_primary(&self) -> bool {
        self.constraint == IndexConstraintType::Primary
    }

    pub fn is_foreign(&self) -> bool {
        self.constraint == IndexConstraintType::Foreign
    }

    /// The physical table columns the keys are computed from.
    pub fn column_ids(&self) -> &[usize] {
        &self.column_ids
    }

    pub fn key_types(&self) -> &[LogicalType] {
        &self.key_types
    }

    pub fn expressions(&self) -> &IndexKeyExpressions {
        &self.expressions
    }

    pub fn prefix_count(&self) -> u8 {
        self.prefix_count
    }

    pub fn storage_version(&self) -> Option<u64> {
        self.storage_version
    }

    pub fn set_storage_version(&mut self, version: Option<u64>) {
        self.storage_version = version;
    }

    /// Whether the index holds no keys.
    pub fn is_empty(&self) -> bool {
        !self.tree.has_metadata()
    }

    /// The root node.
    pub fn root(&self) -> Node {
        self.tree
    }

    /// Bytes of memory held by the index (`GetInMemorySize`).
    pub fn in_memory_size(&self) -> usize {
        self.allocators
            .iter()
            .map(FixedSizeAllocator::in_memory_size)
            .sum()
    }

    /// Drops every key (`ResetStorage`).
    pub fn reset(&mut self) {
        for allocator in &mut self.allocators {
            allocator.reset();
        }
        self.tree.clear();
    }

    /// The serialization information of the allocators.
    pub fn allocator_infos(&self, count: usize) -> Vec<FixedSizeAllocatorInfo> {
        self.allocators
            .iter()
            .take(count)
            .map(FixedSizeAllocator::info)
            .collect()
    }

    // ----------------------------------------------------------------------------------
    // Segment and slot access
    // ----------------------------------------------------------------------------------

    fn allocator_of(node: Node) -> Result<usize> {
        node.ntype()?
            .allocator_index()
            .ok_or_else(|| Error::internal("an inlined leaf has no segment"))
    }

    /// The bytes of the segment of `node`.
    pub(crate) fn seg(&self, node: Node) -> Result<&[u8]> {
        let index = Self::allocator_of(node)?;
        self.allocators[index].segment(node)
    }

    /// The bytes of the segment of `node`, for modification.
    pub(crate) fn seg_mut(&mut self, node: Node) -> Result<&mut [u8]> {
        let index = Self::allocator_of(node)?;
        self.allocators[index].segment_mut(node)
    }

    /// The node stored at `slot`.
    pub(crate) fn get(&self, slot: Slot) -> Result<Node> {
        match slot {
            Slot::Root => Ok(self.tree),
            Slot::Dummy => Ok(Node::default()),
            Slot::In { owner, offset } => Ok(read_node(self.seg(owner)?, usize::from(offset))),
        }
    }

    /// Stores `node` at `slot`.
    pub(crate) fn set(&mut self, slot: Slot, node: Node) -> Result<()> {
        match slot {
            Slot::Root => {
                self.tree = node;
                Ok(())
            }
            Slot::Dummy => Err(Error::internal("write to an empty ART node placeholder")),
            Slot::In { owner, offset } => {
                write_node(self.seg_mut(owner)?, usize::from(offset), node);
                Ok(())
            }
        }
    }

    /// Sets the gate status of the node at `slot`.
    pub(crate) fn set_gate(&mut self, slot: Slot, status: GateStatus) -> Result<()> {
        let mut node = self.get(slot)?;
        node.set_gate_status(status);
        self.set(slot, node)
    }

    /// Allocates a zeroed segment for a node of type `ty`.
    pub(crate) fn alloc(&mut self, ty: NType) -> Result<Node> {
        let index = ty
            .allocator_index()
            .ok_or_else(|| Error::internal("an inlined leaf has no segment"))?;
        let mut node = self.allocators[index].new_segment()?;
        node.set_type(ty);
        Ok(node)
    }

    /// Frees the segment of `node` (`Node::FreeNode` without clearing the reference).
    pub(crate) fn free_node(&mut self, node: Node) -> Result<()> {
        let index = Self::allocator_of(node)?;
        self.allocators[index].free(node)
    }

    // ----------------------------------------------------------------------------------
    // Keys
    // ----------------------------------------------------------------------------------

    /// The key columns of `table_chunk` (`ExecuteExpressions`).
    pub fn execute_expressions(&self, table_chunk: &DataChunk) -> Result<DataChunk> {
        match &self.expressions {
            IndexKeyExpressions::Columns(_) => {
                let columns = self
                    .column_ids
                    .iter()
                    .map(|&c| {
                        table_chunk
                            .columns()
                            .get(c)
                            .cloned()
                            .ok_or_else(|| Error::internal("index column missing from the table chunk"))
                    })
                    .collect::<Result<Vec<_>>>()?;
                Ok(DataChunk::new(columns, table_chunk.len()))
            }
            IndexKeyExpressions::Custom(executor) => executor.execute(table_chunk),
        }
    }

    /// The display names of the key expressions.
    pub fn expression_names(&self) -> Vec<String> {
        match &self.expressions {
            IndexKeyExpressions::Columns(names) => names.clone(),
            IndexKeyExpressions::Custom(executor) => executor.names().to_vec(),
        }
    }

    /// Whether keys of GEOMETRY columns must be converted (`KeyInputNeedConversion`); the
    /// pre-v1.5 GEOMETRY representation is not supported by this port.
    fn check_key_input(&self) -> Result<()> {
        let old = self
            .storage_version
            .is_none_or(|v| v < GEOMETRY_KEY_STORAGE_VERSION);
        if old
            && self
                .key_types
                .iter()
                .any(|t| t.id() == rustdb_types::LogicalTypeId::Geometry)
        {
            return Err(Error::not_implemented(
                "indexes on GEOMETRY columns of databases written before v1.5",
            ));
        }
        Ok(())
    }

    // ----------------------------------------------------------------------------------
    // Insert and delete
    // ----------------------------------------------------------------------------------

    /// Appends the rows of `table_chunk` with row ids `row_ids` (`ART::Append`).
    pub fn append(
        &mut self,
        table_chunk: &DataChunk,
        row_ids: &[i64],
        delete_arts: &[&Self],
        mode: IndexAppendMode,
    ) -> Result<()> {
        let key_chunk = self.execute_expressions(table_chunk)?;
        self.insert(&key_chunk, row_ids, delete_arts, mode)
    }

    /// Inserts the keys of `key_chunk` (`ART::Insert`); on a constraint violation every
    /// key inserted by this call is removed again.
    pub fn insert(
        &mut self,
        key_chunk: &DataChunk,
        row_ids: &[i64],
        delete_arts: &[&Self],
        mode: IndexAppendMode,
    ) -> Result<()> {
        self.check_key_input()?;
        let keys = generate_keys(key_chunk)?;
        self.insert_keys(&keys, row_ids, delete_arts, mode, Some(key_chunk))
    }

    /// `ART::InsertKeys`.
    pub(crate) fn insert_keys(
        &mut self,
        keys: &[Option<ArtKey>],
        row_ids: &[i64],
        delete_arts: &[&Self],
        mode: IndexAppendMode,
        key_chunk: Option<&DataChunk>,
    ) -> Result<()> {
        let mut conflict = None;
        for (i, key) in keys.iter().enumerate() {
            let Some(key) = key else { continue };
            let row_id = row_id_key(row_ids[i]);
            if self.insert_key(Slot::Root, key, 0, row_id, GateStatus::NotSet, delete_arts, mode)?
                == ConflictType::Constraint
            {
                conflict = Some(i);
                break;
            }
        }
        let Some(conflict) = conflict else {
            return Ok(());
        };
        for (i, key) in keys.iter().enumerate().take(conflict) {
            if let Some(key) = key {
                self.delete_key(key, row_id_key(row_ids[i]))?;
            }
        }
        let message = match key_chunk {
            Some(chunk) => append_row_error(chunk, conflict),
            None => "???".to_owned(),
        };
        Err(Error::new(
            ErrorKind::Constraint,
            format!("PRIMARY KEY or UNIQUE constraint violation: duplicate key \"{message}\""),
        ))
    }

    /// Removes the rows of `table_chunk` and returns how many were found
    /// (`ART::TryDelete`).
    pub fn try_delete(&mut self, table_chunk: &DataChunk, row_ids: &[i64]) -> Result<usize> {
        let key_chunk = self.execute_expressions(table_chunk)?;
        self.check_key_input()?;
        let keys = generate_keys(&key_chunk)?;
        let mut deleted = 0;
        for (i, key) in keys.iter().enumerate() {
            let removed = match key {
                Some(key) => self.delete_key(key, row_id_key(row_ids[i]))?,
                None => true,
            };
            if removed {
                deleted += 1;
            }
        }
        Ok(deleted)
    }

    /// Removes the rows of `table_chunk`; every row must be present (`BoundIndex::Delete`).
    pub fn delete(&mut self, table_chunk: &DataChunk, row_ids: &[i64]) -> Result<()> {
        let deleted = self.try_delete(table_chunk, row_ids)?;
        if deleted != table_chunk.len() {
            return Err(Error::invalid_input(format!(
                "Failed to delete all rows from index. Only deleted {deleted} out of {} rows.",
                table_chunk.len()
            )));
        }
        Ok(())
    }

    /// Inserts one key at `slot` (`ARTOperator::Insert` from the root).
    #[allow(clippy::too_many_arguments, reason = "mirrors ARTOperator::Insert")]
    pub(crate) fn insert_key(
        &mut self,
        slot: Slot,
        key: &[u8],
        depth: usize,
        row_id: [u8; ROW_ID_SIZE],
        status: GateStatus,
        delete_arts: &[&Self],
        mode: IndexAppendMode,
    ) -> Result<ConflictType> {
        operator::insert(self, slot, key, depth, row_id, status, delete_arts, mode)
    }

    /// Removes one key/row id pair (`ARTOperator::Delete`).
    pub(crate) fn delete_key(&mut self, key: &[u8], row_id: [u8; ROW_ID_SIZE]) -> Result<bool> {
        operator::delete(self, key, row_id)
    }

    /// The leaf of `key`, if present (`ARTOperator::Lookup`).
    pub fn lookup(&self, key: &[u8]) -> Result<Option<Node>> {
        operator::lookup(self, self.tree, key, 0)
    }

    /// Whether `row_id` is stored under `key`.
    pub fn contains(&self, key: &[u8], row_id: i64) -> Result<bool> {
        match self.lookup(key)? {
            Some(leaf) => operator::lookup_in_leaf(self, leaf, row_id_key(row_id)),
            None => Ok(false),
        }
    }

    // ----------------------------------------------------------------------------------
    // Constraint checking
    // ----------------------------------------------------------------------------------

    /// Verifies that the rows of `table_chunk` can be appended (`ART::VerifyAppend`).
    pub fn verify_append(
        &self,
        table_chunk: &DataChunk,
        delete_arts: &[&Self],
        manager: Option<&mut ConflictManager>,
    ) -> Result<()> {
        if let Some(manager) = manager {
            return self.verify_constraint(table_chunk, delete_arts, manager);
        }
        let mut manager = ConflictManager::new(VerifyExistenceType::Append, table_chunk.len());
        self.verify_constraint(table_chunk, delete_arts, &mut manager)
    }

    /// Looks up every key of `table_chunk` and records hits in `manager`; throws on the
    /// first conflict the manager does not tolerate (`ART::VerifyConstraint`).
    pub fn verify_constraint(
        &self,
        table_chunk: &DataChunk,
        delete_arts: &[&Self],
        manager: &mut ConflictManager,
    ) -> Result<()> {
        let key_chunk = self.execute_expressions(table_chunk)?;
        self.check_key_input()?;
        let keys = generate_keys(&key_chunk)?;
        let mut conflict = None;
        for (i, key) in keys.iter().enumerate() {
            if conflict.is_some() {
                break;
            }
            let Some(key) = key else {
                if manager.add_null(i) {
                    conflict = Some(i);
                }
                continue;
            };
            let Some(leaf) = self.lookup(key)? else {
                continue;
            };
            self.verify_leaf(leaf, key, delete_arts, manager, &mut conflict, i)?;
        }
        manager.finish_lookup();
        let Some(conflict) = conflict else {
            return Ok(());
        };
        let key_name = self.error_key_name(&key_chunk, conflict);
        Err(Error::new(
            ErrorKind::Constraint,
            self.constraint_error_message(manager.verify_type(), &key_name),
        ))
    }

    /// `ART::VerifyLeaf`.
    fn verify_leaf(
        &self,
        leaf: Node,
        key: &[u8],
        delete_arts: &[&Self],
        manager: &mut ConflictManager,
        conflict: &mut Option<usize>,
        i: usize,
    ) -> Result<()> {
        let mut deleted_row_ids = Vec::new();
        for delete_art in delete_arts {
            let Some(deleted) = delete_art.lookup(key)? else {
                continue;
            };
            if !deleted.is(NType::LeafInlined) {
                return Err(Error::internal("Non-inlined leaf?"));
            }
            deleted_row_ids.push(deleted.row_id());
        }
        if leaf.is(NType::LeafInlined) {
            let row_id = leaf.row_id();
            if deleted_row_ids.contains(&row_id) {
                return Ok(());
            }
            if manager.add_hit(i, row_id) {
                *conflict = Some(i);
            }
            return Ok(());
        }
        if self.is_foreign() {
            // Foreign keys do not care which row matched.
            if manager.add_hit(i, crate::MAX_ROW_ID as i64) {
                *conflict = Some(i);
            }
            return Ok(());
        }
        let mut row_ids = BTreeSet::new();
        let mut it = ArtIterator::new(self);
        it.find_minimum(leaf)?;
        let mut output = RowIdSetOutput::new(&mut row_ids, 2);
        let result = it.scan(&[], &mut output, false)?;
        if result != ScanResult::Completed || row_ids.len() != 2 {
            return Err(Error::internal(
                "VerifyLeaf expects exactly two row IDs to be scanned",
            ));
        }
        if row_ids.iter().any(|r| deleted_row_ids.contains(r)) {
            return Ok(());
        }
        let mut iter = row_ids.into_iter();
        if let Some(first) = iter.next()
            && manager.add_hit(i, first)
        {
            *conflict = Some(i);
        }
        if let Some(second) = iter.next() {
            manager.add_second_hit(i, second);
        }
        Ok(())
    }

    /// `"<name>: <value>, ..."` for row `row` of the key chunk (`GenerateErrorKeyName`).
    fn error_key_name(&self, key_chunk: &DataChunk, row: usize) -> String {
        let names = self.expression_names();
        key_chunk
            .columns()
            .iter()
            .enumerate()
            .map(|(k, column)| {
                let name = names.get(k).map_or("", String::as_str);
                format!("{name}: {}", value_to_string(&column.get_value(row)))
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// `ART::GenerateConstraintErrorMessage`.
    fn constraint_error_message(&self, verify_type: VerifyExistenceType, key_name: &str) -> String {
        match verify_type {
            VerifyExistenceType::Append => {
                let kind = if self.is_primary() {
                    "primary key"
                } else {
                    "unique"
                };
                format!("Duplicate key \"{key_name}\" violates {kind} constraint.")
            }
            VerifyExistenceType::AppendFk => format!(
                "Violates foreign key constraint because key \"{key_name}\" does not exist in the referenced table"
            ),
            VerifyExistenceType::DeleteFk => format!(
                "Violates foreign key constraint because key \"{key_name}\" is still referenced by a foreign key in a different table. If this is an unexpected constraint violation, please refer to our foreign key limitations in the documentation"
            ),
        }
    }

    /// The constraint violation message for row `failed_index` of `table_chunk`
    /// (`GetConstraintViolationMessage`).
    pub fn constraint_violation_message(
        &self,
        verify_type: VerifyExistenceType,
        failed_index: usize,
        table_chunk: &DataChunk,
    ) -> Result<String> {
        let key_chunk = self.execute_expressions(table_chunk)?;
        let key_name = self.error_key_name(&key_chunk, failed_index);
        Ok(self.constraint_error_message(verify_type, &key_name))
    }

    // ----------------------------------------------------------------------------------
    // Scans
    // ----------------------------------------------------------------------------------

    /// Collects the row ids of every key (`FullScan`); false if more than `max_count`.
    pub fn full_scan(&self, max_count: usize, row_ids: &mut BTreeSet<i64>) -> Result<bool> {
        if !self.tree.has_metadata() {
            return Ok(true);
        }
        let mut it = ArtIterator::new(self);
        it.find_minimum(self.tree)?;
        let mut output = RowIdSetOutput::new(row_ids, max_count);
        Ok(it.scan(&[], &mut output, false)? == ScanResult::Completed)
    }

    /// `SearchEqual`.
    pub fn search_equal(&self, key: &[u8], max_count: usize, row_ids: &mut BTreeSet<i64>) -> Result<bool> {
        let Some(leaf) = self.lookup(key)? else {
            return Ok(true);
        };
        let mut it = ArtIterator::new(self);
        it.find_minimum(leaf)?;
        let mut output = RowIdSetOutput::new(row_ids, max_count);
        Ok(it.scan(&[], &mut output, false)? == ScanResult::Completed)
    }

    /// `SearchGreater`: keys `> key` (or `>=` with `equal`).
    pub fn search_greater(
        &self,
        key: &[u8],
        equal: bool,
        max_count: usize,
        row_ids: &mut BTreeSet<i64>,
    ) -> Result<bool> {
        if !self.tree.has_metadata() {
            return Ok(true);
        }
        let mut it = ArtIterator::new(self);
        if !it.lower_bound(self.tree, key, equal)? {
            return Ok(true);
        }
        let mut output = RowIdSetOutput::new(row_ids, max_count);
        Ok(it.scan(&[], &mut output, false)? == ScanResult::Completed)
    }

    /// `SearchLess`: keys `< upper` (or `<=` with `equal`).
    pub fn search_less(
        &self,
        upper: &[u8],
        equal: bool,
        max_count: usize,
        row_ids: &mut BTreeSet<i64>,
    ) -> Result<bool> {
        if !self.tree.has_metadata() {
            return Ok(true);
        }
        let mut it = ArtIterator::new(self);
        it.find_minimum(self.tree)?;
        if it.current_key_greater_than(upper, equal) {
            return Ok(true);
        }
        let mut output = RowIdSetOutput::new(row_ids, max_count);
        Ok(it.scan(upper, &mut output, equal)? == ScanResult::Completed)
    }

    /// `SearchCloseRange`.
    pub fn search_close_range(
        &self,
        lower: &[u8],
        upper: &[u8],
        left_equal: bool,
        right_equal: bool,
        max_count: usize,
        row_ids: &mut BTreeSet<i64>,
    ) -> Result<bool> {
        if !self.tree.has_metadata() {
            return Ok(true);
        }
        let mut it = ArtIterator::new(self);
        if !it.lower_bound(self.tree, lower, left_equal)? {
            return Ok(true);
        }
        let mut output = RowIdSetOutput::new(row_ids, max_count);
        Ok(it.scan(upper, &mut output, right_equal)? == ScanResult::Completed)
    }

    /// Runs an index scan for `predicate` (`ART::Scan`); false if more than `max_count` row
    /// ids match, in which case the caller falls back to a table scan.
    pub fn scan(
        &self,
        predicate: &IndexScanPredicate,
        max_count: usize,
        row_ids: &mut BTreeSet<i64>,
    ) -> Result<bool> {
        match predicate {
            IndexScanPredicate::Full => self.full_scan(max_count, row_ids),
            IndexScanPredicate::Single { op, value } => {
                let key = create_key(value)?;
                match op {
                    CompareOp::Equal => self.search_equal(&key, max_count, row_ids),
                    CompareOp::GreaterThanOrEqual => self.search_greater(&key, true, max_count, row_ids),
                    CompareOp::GreaterThan => self.search_greater(&key, false, max_count, row_ids),
                    CompareOp::LessThanOrEqual => self.search_less(&key, true, max_count, row_ids),
                    CompareOp::LessThan => self.search_less(&key, false, max_count, row_ids),
                    CompareOp::NotEqual => Err(Error::internal("Index scan type not implemented")),
                }
            }
            IndexScanPredicate::Range {
                low,
                low_inclusive,
                high,
                high_inclusive,
            } => {
                let lower = create_key(low)?;
                let upper = create_key(high)?;
                self.search_close_range(
                    &lower,
                    &upper,
                    *low_inclusive,
                    *high_inclusive,
                    max_count,
                    row_ids,
                )
            }
        }
    }
}

/// The predicate of an index scan (`ARTIndexScanState`).
#[derive(Clone, Debug, PartialEq)]
pub enum IndexScanPredicate {
    Full,
    /// One comparison `key <op> value`.
    Single {
        op: CompareOp,
        value: Value,
    },
    /// `low <(=) key <(=) high`.
    Range {
        low: Value,
        low_inclusive: bool,
        high: Value,
        high_inclusive: bool,
    },
}

impl IndexScanPredicate {
    /// The predicates an index answers for a pushed-down filter on the key column
    /// (`TryScanIndex`): one equality lookup per IN value that satisfies the other
    /// comparisons, or a single top-level comparison. `None` if the index cannot be used.
    pub fn from_filter(filter: &TableFilter) -> Option<Vec<Self>> {
        let mut comparisons = Vec::new();
        let mut in_values: Vec<Value> = Vec::new();
        if extract_comparisons(filter, &mut comparisons, &mut in_values) && !in_values.is_empty() {
            let mut unique: Vec<Value> = Vec::new();
            for value in in_values {
                if unique.contains(&value) {
                    continue;
                }
                let qualifies = comparisons.iter().all(|(op, constant)| {
                    value
                        .compare_not_distinct(constant)
                        .is_some_and(|ordering| ordering_matches(*op, ordering))
                });
                if qualifies {
                    unique.push(value);
                }
            }
            if unique.is_empty() {
                return None;
            }
            return Some(
                unique
                    .into_iter()
                    .map(|value| Self::Single {
                        op: CompareOp::Equal,
                        value,
                    })
                    .collect(),
            );
        }
        match filter {
            TableFilter::ConstantComparison { op, constant } if !constant.is_null() => match op {
                CompareOp::Equal
                | CompareOp::GreaterThan
                | CompareOp::GreaterThanOrEqual
                | CompareOp::LessThan
                | CompareOp::LessThanOrEqual => Some(vec![Self::Single {
                    op: *op,
                    value: constant.clone(),
                }]),
                CompareOp::NotEqual => None,
            },
            _ => None,
        }
    }
}

/// `ExtractComparisonsAndInFilters`: constant comparisons and IN lists under ANDs.
fn extract_comparisons(
    filter: &TableFilter,
    comparisons: &mut Vec<(CompareOp, Value)>,
    in_values: &mut Vec<Value>,
) -> bool {
    match filter {
        TableFilter::ConstantComparison { op, constant } => {
            comparisons.push((*op, constant.clone()));
            true
        }
        TableFilter::In(values) => {
            in_values.extend(values.iter().cloned());
            true
        }
        TableFilter::And(children) => children
            .iter()
            .all(|child| extract_comparisons(child, comparisons, in_values)),
        TableFilter::Optional(child) => child
            .as_ref()
            .is_none_or(|child| extract_comparisons(child, comparisons, in_values)),
        // A `SelectivityOptionalFilter` is an `OptionalFilter`.
        TableFilter::SelectivityOptional(optional) => {
            extract_comparisons(&optional.child, comparisons, in_values)
        }
        // Bloom filters cannot find comparisons or IN filters, but do not prevent them.
        TableFilter::Expression(expression) => expression.is_bloom_filter(),
        _ => false,
    }
}

/// Whether `left <op> right` holds for `left.cmp(right) == ordering`.
fn ordering_matches(op: CompareOp, ordering: std::cmp::Ordering) -> bool {
    use std::cmp::Ordering::{Equal, Greater, Less};
    match op {
        CompareOp::Equal => ordering == Equal,
        CompareOp::NotEqual => ordering != Equal,
        CompareOp::LessThan => ordering == Less,
        CompareOp::LessThanOrEqual => ordering != Greater,
        CompareOp::GreaterThan => ordering == Greater,
        CompareOp::GreaterThanOrEqual => ordering != Less,
    }
}

/// DuckDB's `Value::ToString` for error messages.
fn value_to_string(value: &Value) -> String {
    if value.is_null() {
        return "NULL".to_owned();
    }
    value.to_varchar().unwrap_or_default()
}

/// The key values of row `row`, joined by `", "` (`BoundIndex::AppendRowError`).
fn append_row_error(key_chunk: &DataChunk, row: usize) -> String {
    key_chunk
        .columns()
        .iter()
        .map(|column| value_to_string(&column.get_value(row)))
        .collect::<Vec<_>>()
        .join(", ")
}
