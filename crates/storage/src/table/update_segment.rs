//! In-place updates with MVCC (DuckDB's `UpdateSegment` and `UpdateInfo`).
//!
//! Updates never modify the column segments. Per vector, a *base* node holds the newest
//! value of every tuple that was ever updated, and a chain of transaction nodes (newest
//! first) holds the values each updating transaction overwrote. A reader starts from the
//! segment data, applies the base node, and then restores the old values of every
//! transaction node it must not see — newest to oldest, so the oldest relevant value wins.

use std::any::Any;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

use rustdb_common::{Error, ErrorKind, Result, STANDARD_VECTOR_SIZE};
use rustdb_types::{
    DataChunk, Interval, LogicalType, LogicalTypeId, NativeType, PhysicalType, SelectionVector, StringHeap,
    StringT, ValidityMask, Vector,
};

use crate::VECTOR_SIZE;
use crate::statistics::BaseStatistics;
use crate::transaction::TransactionData;

/// The version of a transaction's update node: the transaction id until it commits, then
/// the commit id (DuckDB's `UpdateInfo::version_number`).
#[derive(Debug)]
pub struct UpdateVersion {
    version: AtomicU64,
}

impl UpdateVersion {
    fn new(version: u64) -> Arc<Self> {
        Arc::new(Self {
            version: AtomicU64::new(version),
        })
    }

    pub fn get(&self) -> u64 {
        self.version.load(Ordering::Acquire)
    }

    pub fn set(&self, version: u64) {
        self.version.store(version, Ordering::Release);
    }

    /// Whether a transaction must use the old values stored with this version: the update
    /// committed after it started or is not committed yet, and is not its own.
    fn applies_to(&self, transaction: TransactionData) -> bool {
        let version = self.get();
        version > transaction.start_time && version != transaction.transaction_id
    }
}

/// Typed value arrays of update nodes.
#[derive(Clone, Debug)]
enum Values {
    /// Validity updates: `true` = valid.
    Validity(Vec<bool>),
    Bool(Vec<bool>),
    I8(Vec<i8>),
    I16(Vec<i16>),
    I32(Vec<i32>),
    I64(Vec<i64>),
    I128(Vec<i128>),
    U8(Vec<u8>),
    U16(Vec<u16>),
    U32(Vec<u32>),
    U64(Vec<u64>),
    U128(Vec<u128>),
    F32(Vec<f32>),
    F64(Vec<f64>),
    Interval(Vec<Interval>),
    /// Strings; long ones point into the segment's heap.
    Str(Vec<StringT>),
}

/// A value type that updates can store.
trait UpdateType: NativeType {
    fn values(values: &Values) -> Option<&Vec<Self>>;
    fn wrap(values: Vec<Self>) -> Values;
    /// C++ `==` on the stored representation (`TemplatedGetEffectiveUpdates`).
    fn same(self, other: Self, update: &Vector, base: &Vector) -> bool;
    /// Copies data that may point into foreign memory into the update heap.
    fn own(self, _source: &Vector, _heap: &mut StringHeap) -> Self {
        self
    }
    /// Accounts for a new value in the update statistics.
    fn update_stats(_stats: &mut BaseStatistics, _value: Self, _source: &Vector) -> Result<()> {
        Ok(())
    }
}

macro_rules! numeric_update_type {
    ($($ty:ty => $variant:ident),* $(,)?) => {$(
        impl UpdateType for $ty {
            fn values(values: &Values) -> Option<&Vec<Self>> {
                match values {
                    Values::$variant(values) => Some(values),
                    _ => None,
                }
            }
            fn wrap(values: Vec<Self>) -> Values {
                Values::$variant(values)
            }
            #[allow(clippy::float_cmp, reason = "C++ operator== semantics: -0.0 == 0.0, NaN != NaN")]
            fn same(self, other: Self, _: &Vector, _: &Vector) -> bool {
                self == other
            }
            fn update_stats(stats: &mut BaseStatistics, value: Self, _: &Vector) -> Result<()> {
                if let Some(numeric) = stats.numeric_mut() {
                    numeric.update([value]);
                }
                Ok(())
            }
        }
    )*};
}

numeric_update_type! {
    bool => Bool, i8 => I8, i16 => I16, i32 => I32, i64 => I64, i128 => I128,
    u8 => U8, u16 => U16, u32 => U32, u64 => U64, u128 => U128, f32 => F32, f64 => F64,
}

impl UpdateType for Interval {
    fn values(values: &Values) -> Option<&Vec<Self>> {
        match values {
            Values::Interval(values) => Some(values),
            _ => None,
        }
    }
    fn wrap(values: Vec<Self>) -> Values {
        Values::Interval(values)
    }
    fn same(self, other: Self, _: &Vector, _: &Vector) -> bool {
        self == other
    }
}

impl UpdateType for StringT {
    fn values(values: &Values) -> Option<&Vec<Self>> {
        match values {
            Values::Str(values) => Some(values),
            _ => None,
        }
    }
    fn wrap(values: Vec<Self>) -> Values {
        Values::Str(values)
    }
    fn same(self, other: Self, update: &Vector, base: &Vector) -> bool {
        self.len() == other.len() && update.string_value(&self) == base.string_value(&other)
    }
    fn own(self, source: &Vector, heap: &mut StringHeap) -> Self {
        if self.is_inlined() {
            self
        } else {
            heap.add(source.string_value(&self))
        }
    }
    fn update_stats(stats: &mut BaseStatistics, value: Self, source: &Vector) -> Result<()> {
        let check_unicode = stats.logical_type().id() == LogicalTypeId::Varchar;
        if let Some(string) = stats.string_mut() {
            string.update(source.string_value(&value), check_unicode)?;
        }
        Ok(())
    }
}

/// Values of updated tuples of one vector, sorted by tuple.
#[derive(Clone, Debug)]
struct UpdateNode {
    tuples: Vec<u16>,
    values: Values,
}

/// A transaction's node: the values it overwrote.
#[derive(Debug)]
struct TransactionNode {
    version: Arc<UpdateVersion>,
    node: UpdateNode,
}

#[derive(Debug)]
struct VectorUpdates {
    base: UpdateNode,
    /// Newest first.
    chain: Vec<TransactionNode>,
}

/// Owner of the string bytes of updated values; scanned vectors keep it alive.
#[derive(Debug, Default)]
struct UpdateHeap(Mutex<StringHeap>);

impl UpdateHeap {
    fn lock(&self) -> MutexGuard<'_, StringHeap> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The updates of one column of a row group.
#[derive(Debug)]
pub struct UpdateSegment {
    /// The column type (`VALIDITY` for validity columns).
    ty: LogicalType,
    vectors: RwLock<Vec<Option<VectorUpdates>>>,
    stats: Mutex<BaseStatistics>,
    heap: Arc<UpdateHeap>,
}

/// Dispatches `$body` with `$t` bound to the native type of physical type `$physical`.
macro_rules! dispatch {
    ($physical:expr, $t:ident => $body:expr, $other:expr) => {
        match $physical {
            PhysicalType::Bool => {
                type $t = bool;
                $body
            }
            PhysicalType::Int8 => {
                type $t = i8;
                $body
            }
            PhysicalType::Int16 => {
                type $t = i16;
                $body
            }
            PhysicalType::Int32 => {
                type $t = i32;
                $body
            }
            PhysicalType::Int64 => {
                type $t = i64;
                $body
            }
            PhysicalType::Int128 => {
                type $t = i128;
                $body
            }
            PhysicalType::UInt8 => {
                type $t = u8;
                $body
            }
            PhysicalType::UInt16 => {
                type $t = u16;
                $body
            }
            PhysicalType::UInt32 => {
                type $t = u32;
                $body
            }
            PhysicalType::UInt64 => {
                type $t = u64;
                $body
            }
            PhysicalType::UInt128 => {
                type $t = u128;
                $body
            }
            PhysicalType::Float => {
                type $t = f32;
                $body
            }
            PhysicalType::Double => {
                type $t = f64;
                $body
            }
            PhysicalType::Interval => {
                type $t = Interval;
                $body
            }
            PhysicalType::Varchar => {
                type $t = StringT;
                $body
            }
            _ => $other,
        }
    };
}

fn unsupported(physical: PhysicalType) -> Error {
    Error::not_implemented(format!(
        "Unimplemented type for update segment: {}",
        physical.name()
    ))
}

fn conflict_on_update() -> Error {
    Error::new(ErrorKind::Transaction, "Conflict on update!")
}

/// Applies `node` to the flat vector `result` at rows `tuples` (`MergeUpdateInfo`).
fn merge_into<T: UpdateType>(node: &UpdateNode, result: &mut Vector) {
    let Some(values) = T::values(&node.values) else {
        return;
    };
    let data = result.values_mut::<T>();
    for (&tuple, &value) in node.tuples.iter().zip(values) {
        data[usize::from(tuple)] = value;
    }
}

fn merge_validity(node: &UpdateNode, result: &mut Vector) {
    let Values::Validity(values) = &node.values else {
        return;
    };
    let capacity = result.capacity();
    let mask = result.validity_mut();
    for (&tuple, &valid) in node.tuples.iter().zip(values) {
        mask.set(usize::from(tuple), valid, capacity);
    }
}

fn merge_node(physical: PhysicalType, node: &UpdateNode, result: &mut Vector) {
    if physical == PhysicalType::Bit {
        merge_validity(node, result);
        return;
    }
    dispatch!(physical, T => merge_into::<T>(node, result), ());
}

/// Like [`merge_node`] for tuples `start..end` of the vector, written at
/// `result_offset + tuple - start` (`MergeUpdateInfoRange`).
fn merge_node_range(
    physical: PhysicalType,
    node: &UpdateNode,
    start: usize,
    end: usize,
    result_offset: usize,
    result: &mut Vector,
) {
    let in_range = node
        .tuples
        .iter()
        .enumerate()
        .filter(|(_, tuple)| (start..end).contains(&usize::from(**tuple)));
    if physical == PhysicalType::Bit {
        let Values::Validity(values) = &node.values else {
            return;
        };
        let capacity = result.capacity();
        for (index, &tuple) in in_range {
            let row = result_offset + usize::from(tuple) - start;
            result.validity_mut().set(row, values[index], capacity);
        }
        return;
    }
    dispatch!(
        physical,
        T => {
            let Some(values) = T::values(&node.values) else { return };
            let data = result.values_mut::<T>();
            for (index, &tuple) in in_range {
                data[result_offset + usize::from(tuple) - start] = values[index];
            }
        },
        ()
    );
}

impl UpdateSegment {
    /// Updates of a column of type `ty` (use the `VALIDITY` type for validity columns).
    pub fn new(ty: LogicalType) -> Self {
        let stats = BaseStatistics::create_empty(ty.clone());
        Self {
            ty,
            vectors: RwLock::default(),
            stats: Mutex::new(stats),
            heap: Arc::default(),
        }
    }

    fn physical(&self) -> PhysicalType {
        self.ty.physical_type()
    }

    /// The size of one updated value (`UpdateSegment::type_size`, `GetTypeIdSize`; a
    /// validity value is a `bool`).
    pub(crate) fn type_size(&self) -> usize {
        match self.physical() {
            PhysicalType::Bit => 1,
            physical => physical.size(),
        }
    }

    fn read(&self) -> RwLockReadGuard<'_, Vec<Option<VectorUpdates>>> {
        self.vectors.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn write(&self) -> RwLockWriteGuard<'_, Vec<Option<VectorUpdates>>> {
        self.vectors.write().unwrap_or_else(PoisonError::into_inner)
    }

    /// Keeps the update heap alive in `result` when it may hold updated strings.
    fn keep_heap_alive(&self, result: &mut Vector) {
        if self.physical() == PhysicalType::Varchar {
            let heap: Arc<dyn Any + Send + Sync> = self.heap.clone();
            result.add_keep_alive(heap);
        }
    }

    /// Whether vector `vector_index` has updates.
    pub fn has_updates_in(&self, vector_index: usize) -> bool {
        self.read().get(vector_index).is_some_and(Option::is_some)
    }

    /// Whether vector `vector_index` has updates of not yet cleaned-up transactions.
    pub fn has_uncommitted_updates(&self, vector_index: usize) -> bool {
        self.read()
            .get(vector_index)
            .and_then(Option::as_ref)
            .is_some_and(|updates| !updates.chain.is_empty())
    }

    /// Whether any row in `start_row..=end_row` has updates.
    pub fn has_updates_between(&self, start_row: u64, end_row: u64) -> bool {
        let vectors = self.read();
        (start_row / VECTOR_SIZE..=end_row / VECTOR_SIZE)
            .any(|vector| vectors.get(vector as usize).is_some_and(Option::is_some))
    }

    /// Merges the updates visible to `transaction` into the flat vector `result`, which
    /// holds vector `vector_index` (`FetchUpdates`).
    pub fn fetch_updates(&self, transaction: TransactionData, vector_index: usize, result: &mut Vector) {
        let vectors = self.read();
        let Some(Some(updates)) = vectors.get(vector_index) else {
            return;
        };
        let physical = self.physical();
        merge_node(physical, &updates.base, result);
        for node in updates
            .chain
            .iter()
            .filter(|node| node.version.applies_to(transaction))
        {
            merge_node(physical, &node.node, result);
        }
        drop(vectors);
        self.keep_heap_alive(result);
    }

    /// Merges the newest values (committed or not) into vector `vector_index`
    /// (`FetchCommitted`).
    pub fn fetch_committed(&self, vector_index: usize, result: &mut Vector) {
        let vectors = self.read();
        let Some(Some(updates)) = vectors.get(vector_index) else {
            return;
        };
        merge_node(self.physical(), &updates.base, result);
        drop(vectors);
        self.keep_heap_alive(result);
    }

    /// Merges the newest values of rows `start_row..start_row + count` into `result`
    /// starting at row 0 (`FetchCommittedRange`).
    pub fn fetch_committed_range(&self, start_row: u64, count: u64, result: &mut Vector) {
        if count == 0 {
            return;
        }
        let vectors = self.read();
        let end_row = start_row + count;
        let physical = self.physical();
        for vector in start_row / VECTOR_SIZE..=(end_row - 1) / VECTOR_SIZE {
            let Some(Some(updates)) = vectors.get(vector as usize) else {
                continue;
            };
            let vector_row = vector * VECTOR_SIZE;
            let start = start_row.saturating_sub(vector_row).min(VECTOR_SIZE) as usize;
            let end = (end_row - vector_row).min(VECTOR_SIZE) as usize;
            let result_offset = (vector_row + start as u64 - start_row) as usize;
            merge_node_range(physical, &updates.base, start, end, result_offset, result);
        }
        drop(vectors);
        self.keep_heap_alive(result);
    }

    /// Merges the updates of row `row` (within the row group) visible to `transaction`
    /// into `result[result_index]` (`FetchRow`).
    pub fn fetch_row(
        &self,
        transaction: TransactionData,
        row: u64,
        result: &mut Vector,
        result_index: usize,
    ) {
        let vectors = self.read();
        let Some(Some(updates)) = vectors.get((row / VECTOR_SIZE) as usize) else {
            return;
        };
        let tuple = (row % VECTOR_SIZE) as usize;
        let physical = self.physical();
        let nodes = std::iter::once(&updates.base).chain(
            updates
                .chain
                .iter()
                .filter(|node| node.version.applies_to(transaction))
                .map(|node| &node.node),
        );
        for node in nodes {
            merge_node_range(physical, node, tuple, tuple + 1, result_index, result);
        }
        drop(vectors);
        self.keep_heap_alive(result);
    }

    /// The statistics of all values ever written by updates.
    pub fn statistics(&self) -> BaseStatistics {
        self.stats.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// Updates rows `ids` (row ids, all in one vector of the row group starting at
    /// `row_group_start`) to the rows of `update`. `base` holds the vector's current
    /// segment data (flat, `count` = rows of that vector).
    ///
    /// Returns the version of a newly created transaction node, which the caller must
    /// register in the transaction's undo log. Updates of transaction-local data
    /// (`transaction_id` 0) are applied directly and return `None`.
    pub fn update(
        &self,
        transaction: TransactionData,
        update: &Vector,
        ids: &[i64],
        base: &Vector,
        row_group_start: u64,
    ) -> Result<Option<Arc<UpdateVersion>>> {
        let physical = self.physical();
        dispatch!(
            physical,
            T => self.update_typed::<T>(transaction, update, ids, base, row_group_start),
            if physical == PhysicalType::Bit {
                self.update_validity(transaction, update, ids, base, row_group_start)
            } else {
                Err(unsupported(physical))
            }
        )
    }

    /// Sorts the rows to update by id and drops duplicates (`SortSelectionVector`);
    /// returns indexes into `ids`.
    fn sorted_rows(ids: &[i64], rows: Vec<u32>) -> Vec<u32> {
        let mut rows = rows;
        if rows
            .windows(2)
            .all(|pair| ids[pair[0] as usize] < ids[pair[1] as usize])
        {
            return rows;
        }
        rows.sort_by_key(|&row| ids[row as usize]);
        rows.dedup_by_key(|row| ids[*row as usize]);
        rows
    }

    fn update_typed<T: UpdateType>(
        &self,
        transaction: TransactionData,
        update: &Vector,
        ids: &[i64],
        base: &Vector,
        row_group_start: u64,
    ) -> Result<Option<Arc<UpdateVersion>>> {
        let mut vectors = self.write();
        let source = update.unified::<T>();
        // Statistics; NULL updates are handled by the validity column.
        let mut rows: Vec<u32> = Vec::with_capacity(ids.len());
        {
            let mut stats = self.stats.lock().unwrap_or_else(PoisonError::into_inner);
            for row in 0..ids.len() {
                if let Some(value) = source.get(row) {
                    stats.set_has_no_null_fast();
                    T::update_stats(&mut stats, value, update)?;
                    rows.push(row as u32);
                } else {
                    stats.set_has_null_fast();
                }
            }
        }
        if rows.is_empty() {
            return Ok(None);
        }
        let mut rows = Self::sorted_rows(ids, rows);
        let first_id = ids[rows[0] as usize] as u64;
        let vector_index = ((first_id - row_group_start) / VECTOR_SIZE) as usize;
        let vector_offset = row_group_start + vector_index as u64 * VECTOR_SIZE;
        let tuple_of = |row: u32| (ids[row as usize] as u64 - vector_offset) as u16;
        let base_data = base.values::<T>();
        let base_validity = base.validity();
        let has_updates = vectors.get(vector_index).is_some_and(Option::is_some);
        if !has_updates {
            // Skip rows whose value does not change (`GetEffectiveUpdates`).
            rows.retain(|&row| {
                let tuple = usize::from(tuple_of(row));
                let value = source.values[source.sel.slot(row as usize)];
                !(base_validity.is_valid(tuple) && value.same(base_data[tuple], update, base))
            });
            if rows.is_empty() {
                return Ok(None);
            }
        }
        let mut heap = self.heap.lock();
        let new_values: Vec<(u16, T)> = rows
            .iter()
            .map(|&row| {
                let value = source.values[source.sel.slot(row as usize)];
                (tuple_of(row), value.own(update, &mut heap))
            })
            .collect();
        let base_value = |tuple: u16, heap: &mut StringHeap| base_data[usize::from(tuple)].own(base, heap);
        if vectors.len() <= vector_index {
            vectors.resize_with(vector_index + 1, || None);
        }
        let Some(updates) = &mut vectors[vector_index] else {
            // First update of this vector: the base node holds the new values, the
            // transaction node the old ones.
            let tuples: Vec<u16> = new_values.iter().map(|(tuple, _)| *tuple).collect();
            let old: Vec<T> = tuples.iter().map(|&tuple| base_value(tuple, &mut heap)).collect();
            let base_node = UpdateNode {
                tuples: tuples.clone(),
                values: T::wrap(new_values.iter().map(|(_, value)| *value).collect()),
            };
            let mut chain = Vec::new();
            let version =
                (transaction.transaction_id != 0).then(|| UpdateVersion::new(transaction.transaction_id));
            if let Some(version) = &version {
                chain.push(TransactionNode {
                    version: Arc::clone(version),
                    node: UpdateNode {
                        tuples,
                        values: T::wrap(old),
                    },
                });
            }
            vectors[vector_index] = Some(VectorUpdates {
                base: base_node,
                chain,
            });
            return Ok(version);
        };
        // Check for conflicts with transactions we cannot see, and find our own node.
        let mut own_node = None;
        for (index, node) in updates.chain.iter().enumerate() {
            let version = node.version.get();
            if version == transaction.transaction_id {
                own_node = Some(index);
            } else if version > transaction.start_time
                && new_values
                    .iter()
                    .any(|(tuple, _)| node.node.tuples.binary_search(tuple).is_ok())
            {
                return Err(conflict_on_update());
            }
        }
        let mut created = None;
        let own_index = if let Some(index) = own_node {
            index
        } else {
            let version = UpdateVersion::new(transaction.transaction_id);
            if transaction.transaction_id != 0 {
                created = Some(Arc::clone(&version));
                updates.chain.insert(
                    0,
                    TransactionNode {
                        version,
                        node: UpdateNode {
                            tuples: Vec::new(),
                            values: T::wrap(Vec::new()),
                        },
                    },
                );
                0
            } else {
                // Transaction-local updates keep no undo information.
                usize::MAX
            }
        };
        // Old values: keep what our node already saved, otherwise take the current value
        // from the base node or the segment (`MergeUpdateLoopInternal`).
        if own_index != usize::MAX {
            let base_tuples = &updates.base.tuples;
            let base_values = T::values(&updates.base.values).cloned().unwrap_or_default();
            let own = &mut updates.chain[own_index].node;
            let own_values = T::values(&own.values).cloned().unwrap_or_default();
            let mut merged: Vec<(u16, T)> = own.tuples.iter().copied().zip(own_values).collect();
            for (tuple, _) in &new_values {
                if merged.binary_search_by_key(tuple, |(t, _)| *t).is_ok() {
                    continue;
                }
                let old = match base_tuples.binary_search(tuple) {
                    Ok(position) => base_values[position],
                    Err(_) => base_value(*tuple, &mut heap),
                };
                let position = merged.partition_point(|(t, _)| t < tuple);
                merged.insert(position, (*tuple, old));
            }
            own.tuples = merged.iter().map(|(tuple, _)| *tuple).collect();
            own.values = T::wrap(merged.into_iter().map(|(_, value)| value).collect());
        }
        // New values go into the base node.
        let base_values = T::values(&updates.base.values).cloned().unwrap_or_default();
        let mut merged: Vec<(u16, T)> = updates.base.tuples.iter().copied().zip(base_values).collect();
        for (tuple, value) in new_values {
            match merged.binary_search_by_key(&tuple, |(t, _)| *t) {
                Ok(position) => merged[position].1 = value,
                Err(position) => merged.insert(position, (tuple, value)),
            }
        }
        updates.base.tuples = merged.iter().map(|(tuple, _)| *tuple).collect();
        updates.base.values = T::wrap(merged.into_iter().map(|(_, value)| value).collect());
        Ok(created)
    }

    fn update_validity(
        &self,
        transaction: TransactionData,
        update: &Vector,
        ids: &[i64],
        base: &Vector,
        row_group_start: u64,
    ) -> Result<Option<Arc<UpdateVersion>>> {
        let mut vectors = self.write();
        let count = ids.len();
        let update_sel = update.sel();
        let update_validity = update.validity();
        {
            let mut stats = self.stats.lock().unwrap_or_else(PoisonError::into_inner);
            for row in 0..count {
                if update_validity.is_valid(update_sel.slot(row)) {
                    stats.set_has_no_null_fast();
                } else {
                    stats.set_has_null_fast();
                }
            }
        }
        let mut rows = Self::sorted_rows(ids, (0..count as u32).collect());
        if rows.is_empty() {
            return Ok(None);
        }
        let first_id = ids[rows[0] as usize] as u64;
        let vector_index = ((first_id - row_group_start) / VECTOR_SIZE) as usize;
        let vector_offset = row_group_start + vector_index as u64 * VECTOR_SIZE;
        let tuple_of = |row: u32| (ids[row as usize] as u64 - vector_offset) as u16;
        let base_validity: &ValidityMask = base.validity();
        let new_valid = |row: u32| update_validity.is_valid(update_sel.slot(row as usize));
        let has_updates = vectors.get(vector_index).is_some_and(Option::is_some);
        if !has_updates {
            rows.retain(|&row| base_validity.is_valid(usize::from(tuple_of(row))) != new_valid(row));
            if rows.is_empty() {
                return Ok(None);
            }
        }
        let new_values: Vec<(u16, bool)> = rows.iter().map(|&row| (tuple_of(row), new_valid(row))).collect();
        if vectors.len() <= vector_index {
            vectors.resize_with(vector_index + 1, || None);
        }
        let Some(updates) = &mut vectors[vector_index] else {
            let tuples: Vec<u16> = new_values.iter().map(|(tuple, _)| *tuple).collect();
            let old = tuples
                .iter()
                .map(|&tuple| base_validity.is_valid(usize::from(tuple)))
                .collect();
            let base_node = UpdateNode {
                tuples: tuples.clone(),
                values: Values::Validity(new_values.iter().map(|(_, valid)| *valid).collect()),
            };
            let version =
                (transaction.transaction_id != 0).then(|| UpdateVersion::new(transaction.transaction_id));
            let mut chain = Vec::new();
            if let Some(version) = &version {
                chain.push(TransactionNode {
                    version: Arc::clone(version),
                    node: UpdateNode {
                        tuples,
                        values: Values::Validity(old),
                    },
                });
            }
            vectors[vector_index] = Some(VectorUpdates {
                base: base_node,
                chain,
            });
            return Ok(version);
        };
        let mut own_node = None;
        for (index, node) in updates.chain.iter().enumerate() {
            let version = node.version.get();
            if version == transaction.transaction_id {
                own_node = Some(index);
            } else if version > transaction.start_time
                && new_values
                    .iter()
                    .any(|(tuple, _)| node.node.tuples.binary_search(tuple).is_ok())
            {
                return Err(conflict_on_update());
            }
        }
        let mut created = None;
        let own_index = match own_node {
            Some(index) => Some(index),
            None if transaction.transaction_id != 0 => {
                let version = UpdateVersion::new(transaction.transaction_id);
                created = Some(Arc::clone(&version));
                updates.chain.insert(
                    0,
                    TransactionNode {
                        version,
                        node: UpdateNode {
                            tuples: Vec::new(),
                            values: Values::Validity(Vec::new()),
                        },
                    },
                );
                Some(0)
            }
            None => None,
        };
        let Values::Validity(base_values) = updates.base.values.clone() else {
            return Err(Error::internal(
                "validity update segment holds non-validity values",
            ));
        };
        if let Some(own_index) = own_index {
            let base_tuples = updates.base.tuples.clone();
            let own = &mut updates.chain[own_index].node;
            let Values::Validity(own_values) = own.values.clone() else {
                return Err(Error::internal("validity update node holds non-validity values"));
            };
            let mut merged: Vec<(u16, bool)> = own.tuples.iter().copied().zip(own_values).collect();
            for (tuple, _) in &new_values {
                if merged.binary_search_by_key(tuple, |(t, _)| *t).is_ok() {
                    continue;
                }
                let old = match base_tuples.binary_search(tuple) {
                    Ok(position) => base_values[position],
                    Err(_) => base_validity.is_valid(usize::from(*tuple)),
                };
                let position = merged.partition_point(|(t, _)| t < tuple);
                merged.insert(position, (*tuple, old));
            }
            own.tuples = merged.iter().map(|(tuple, _)| *tuple).collect();
            own.values = Values::Validity(merged.into_iter().map(|(_, valid)| valid).collect());
        }
        let mut merged: Vec<(u16, bool)> = updates.base.tuples.iter().copied().zip(base_values).collect();
        for (tuple, valid) in new_values {
            match merged.binary_search_by_key(&tuple, |(t, _)| *t) {
                Ok(position) => merged[position].1 = valid,
                Err(position) => merged.insert(position, (tuple, valid)),
            }
        }
        updates.base.tuples = merged.iter().map(|(tuple, _)| *tuple).collect();
        updates.base.values = Values::Validity(merged.into_iter().map(|(_, valid)| valid).collect());
        Ok(created)
    }

    /// Restores the values overwritten by the transaction owning `version` and removes
    /// its node (`RollbackUpdate`).
    pub fn rollback_update(&self, vector_index: usize, version: &Arc<UpdateVersion>) {
        let mut vectors = self.write();
        let Some(Some(updates)) = vectors.get_mut(vector_index) else {
            return;
        };
        let Some(position) = updates
            .chain
            .iter()
            .position(|node| Arc::ptr_eq(&node.version, version))
        else {
            return;
        };
        let removed = updates.chain.remove(position);
        let base = &mut updates.base;
        macro_rules! restore {
            ($old:expr, $current:expr) => {
                for (tuple, value) in removed.node.tuples.iter().zip($old) {
                    if let Ok(index) = base.tuples.binary_search(tuple) {
                        $current[index] = *value;
                    }
                }
            };
        }
        match (&removed.node.values, &mut base.values) {
            (Values::Validity(old), Values::Validity(current))
            | (Values::Bool(old), Values::Bool(current)) => {
                restore!(old, current);
            }
            (Values::I8(old), Values::I8(current)) => restore!(old, current),
            (Values::I16(old), Values::I16(current)) => restore!(old, current),
            (Values::I32(old), Values::I32(current)) => restore!(old, current),
            (Values::I64(old), Values::I64(current)) => restore!(old, current),
            (Values::I128(old), Values::I128(current)) => restore!(old, current),
            (Values::U8(old), Values::U8(current)) => restore!(old, current),
            (Values::U16(old), Values::U16(current)) => restore!(old, current),
            (Values::U32(old), Values::U32(current)) => restore!(old, current),
            (Values::U64(old), Values::U64(current)) => restore!(old, current),
            (Values::U128(old), Values::U128(current)) => restore!(old, current),
            (Values::F32(old), Values::F32(current)) => restore!(old, current),
            (Values::F64(old), Values::F64(current)) => restore!(old, current),
            (Values::Interval(old), Values::Interval(current)) => restore!(old, current),
            (Values::Str(old), Values::Str(current)) => restore!(old, current),
            _ => {}
        }
    }

    /// Unlinks the node of a transaction that no reader needs any more
    /// (`CleanupUpdate`).
    pub fn cleanup_update(&self, vector_index: usize, version: &Arc<UpdateVersion>) {
        let mut vectors = self.write();
        if let Some(Some(updates)) = vectors.get_mut(vector_index) {
            updates.chain.retain(|node| !Arc::ptr_eq(&node.version, version));
        }
    }

    /// The current values and the row ids of the tuples of vector `vector_index` that
    /// the transaction owning `version` updated, as the WAL logs them
    /// (`WALWriteState::WriteUpdate`): validity updates are logged as BOOLEAN vectors
    /// whose validity mask is the update.
    pub fn wal_update(
        &self,
        vector_index: usize,
        version: &Arc<UpdateVersion>,
        row_group_start: u64,
    ) -> Option<DataChunk> {
        let tuples = self.updated_tuples(vector_index, version);
        if tuples.is_empty() {
            return None;
        }
        let ty = if self.physical() == PhysicalType::Bit {
            LogicalType::BOOLEAN
        } else {
            self.ty.clone()
        };
        let mut values = Vector::flat(ty, STANDARD_VECTOR_SIZE);
        self.fetch_committed(vector_index, &mut values);
        let count = tuples.len();
        let sel = SelectionVector::new(tuples.iter().map(|&tuple| u32::from(tuple)).collect::<Vec<_>>());
        let values = values.slice(&sel).flattened(count);
        let start = row_group_start + vector_index as u64 * VECTOR_SIZE;
        let mut row_ids = Vector::flat(LogicalType::BIGINT, count);
        for (slot, &tuple) in row_ids.values_mut::<i64>().iter_mut().zip(&tuples) {
            *slot = (start + u64::from(tuple)) as i64;
        }
        Some(DataChunk::new(vec![values, row_ids], count))
    }

    /// The tuples (within the vector) updated by the transaction owning `version`; the WAL
    /// logs their current values.
    pub fn updated_tuples(&self, vector_index: usize, version: &Arc<UpdateVersion>) -> Vec<u16> {
        let vectors = self.read();
        let Some(Some(updates)) = vectors.get(vector_index) else {
            return Vec::new();
        };
        updates
            .chain
            .iter()
            .find(|node| Arc::ptr_eq(&node.version, version))
            .map(|node| node.node.tuples.clone())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transaction::TRANSACTION_ID_START;
    use rustdb_types::Value;

    const T1: u64 = TRANSACTION_ID_START + 1;
    const T2: u64 = TRANSACTION_ID_START + 2;

    fn ints(values: &[i32]) -> Vector {
        let mut vector = Vector::flat(LogicalType::INTEGER, values.len().max(1));
        vector.values_mut::<i32>()[..values.len()].copy_from_slice(values);
        vector
    }

    fn read(segment: &UpdateSegment, transaction: TransactionData, base: &[i32]) -> Vec<i32> {
        let mut result = ints(base);
        segment.fetch_updates(transaction, 0, &mut result);
        result.values::<i32>()[..base.len()].to_vec()
    }

    #[test]
    fn updates_are_isolated_until_commit() {
        let segment = UpdateSegment::new(LogicalType::INTEGER);
        let base = [1, 2, 3, 4];
        let t1 = TransactionData::new(T1, 5);
        let version = segment
            .update(t1, &ints(&[20, 40]), &[1, 3], &ints(&base), 0)
            .unwrap()
            .expect("a new transaction node");
        assert_eq!(read(&segment, t1, &base), [1, 20, 3, 40]);
        let t2 = TransactionData::new(T2, 5);
        assert_eq!(read(&segment, t2, &base), [1, 2, 3, 4]);
        let conflict = segment
            .update(t2, &ints(&[99]), &[3], &ints(&base), 0)
            .unwrap_err();
        assert_eq!(
            conflict.to_string(),
            "TransactionContext Error: Conflict on update!"
        );
        // t2 can update other rows
        let v2 = segment
            .update(t2, &ints(&[30]), &[2], &ints(&base), 0)
            .unwrap()
            .expect("t2 node");
        version.set(6);
        assert_eq!(
            read(&segment, TransactionData::new(T2 + 1, 7), &base),
            [1, 20, 3, 40]
        );
        segment.rollback_update(0, &v2);
        assert_eq!(
            read(&segment, TransactionData::new(T2 + 1, 7), &base),
            [1, 20, 3, 40]
        );
        // a transaction that started before the commit still sees the old values
        assert_eq!(
            read(&segment, TransactionData::new(T2 + 2, 5), &base),
            [1, 2, 3, 4]
        );
        segment.cleanup_update(0, &version);
        assert!(!segment.has_uncommitted_updates(0));
    }

    #[test]
    fn rollback_restores_and_strings_are_owned() {
        let segment = UpdateSegment::new(LogicalType::VARCHAR);
        let mut base = Vector::flat(LogicalType::VARCHAR, 2);
        base.set_value(0, &Value::varchar("a long original string"))
            .unwrap();
        base.set_value(1, &Value::varchar("b")).unwrap();
        let mut update = Vector::flat(LogicalType::VARCHAR, 1);
        update
            .set_value(0, &Value::varchar("a long replacement string"))
            .unwrap();
        let t1 = TransactionData::new(T1, 5);
        let version = segment.update(t1, &update, &[0], &base, 0).unwrap().unwrap();
        drop(update);
        let mut result = base.clone();
        segment.fetch_updates(t1, 0, &mut result);
        assert_eq!(result.get_value(0), Value::varchar("a long replacement string"));
        segment.rollback_update(0, &version);
        let mut result = base.clone();
        drop(base);
        segment.fetch_updates(t1, 0, &mut result);
        assert_eq!(result.get_value(0), Value::varchar("a long original string"));
    }
}
