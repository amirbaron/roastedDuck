//! The data of one column within one row group (DuckDB's `ColumnData` and its
//! `StandardColumnData`, `ValidityColumnData`, `ListColumnData`, `StructColumnData` and
//! `ArrayColumnData` subclasses).
//!
//! Nested types are split like DuckDB's storage: every non-validity column has a validity
//! child; LIST columns store cumulative `u64` end offsets plus a child column; STRUCT
//! columns store one child per field; ARRAY columns store a child with `size` rows per row.

use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use rustdb_common::{Error, Result, STANDARD_VECTOR_SIZE};
use rustdb_types::{
    ListEntry, LogicalType, LogicalTypeId, PhysicalType, SelectionVector, Value, Vector, VectorKind,
};

use super::chunk_info::UpdateScanType;
use super::column_segment::{ColumnSegment, SegmentAppendState, SegmentScanState};
use super::segment_tree::{Segment, SegmentNode, SegmentTree};
use super::update_segment::{UpdateSegment, UpdateVersion};
use crate::VECTOR_SIZE;
use crate::data_table::TableIoManager;
use crate::statistics::{BaseStatistics, FilterPropagateResult};
use crate::table_filter::TableFilter;
use crate::transaction::TransactionData;

impl Segment for ColumnSegment {
    fn row_count(&self) -> u64 {
        self.count()
    }
}

/// Who owns a column's data (DuckDB's `ColumnDataType`). It decides the size of the first
/// segment: transaction-local data starts with vector-sized segments.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ColumnDataType {
    MainTable = 0,
    InitialTransactionLocal = 1,
    TransactionLocal = 2,
    CheckpointTarget = 3,
}

impl ColumnDataType {
    fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::InitialTransactionLocal,
            2 => Self::TransactionLocal,
            3 => Self::CheckpointTarget,
            _ => Self::MainTable,
        }
    }
}

/// Type-specific children of a column.
#[derive(Debug)]
pub enum ColumnKind {
    /// A validity mask (the child of every other column).
    Validity,
    /// A fixed-size or string column.
    Standard { validity: Box<ColumnData> },
    List {
        validity: Box<ColumnData>,
        child: Box<ColumnData>,
    },
    Struct {
        validity: Box<ColumnData>,
        fields: Vec<ColumnData>,
    },
    Array {
        validity: Box<ColumnData>,
        child: Box<ColumnData>,
    },
}

/// A registered update of one update segment, to be put in the transaction's undo log.
#[derive(Debug)]
pub struct UpdateRegistration {
    pub segment: Arc<UpdateSegment>,
    pub vector_index: usize,
    pub version: Arc<UpdateVersion>,
    /// The path from the updated top-level column to the column owning `segment`: child
    /// indexes where validity is child 0 and struct fields follow (the WAL's
    /// `column_indexes` without the top-level index).
    pub column_path: Vec<usize>,
    /// The first row of the row group.
    pub row_group_start: u64,
}

/// Prefixes the column paths of `registrations` with `child`.
fn under_child(
    child: usize,
    registrations: Vec<UpdateRegistration>,
) -> impl Iterator<Item = UpdateRegistration> {
    registrations.into_iter().map(move |mut registration| {
        registration.column_path.insert(0, child);
        registration
    })
}

/// The state of a scan of one column (DuckDB's `ColumnScanState`).
#[derive(Debug, Default)]
pub struct ColumnScanState {
    /// The segment being scanned and its position in the column's segment list.
    current: Option<(usize, SegmentNode<ColumnSegment>)>,
    segment_state: Option<SegmentScanState>,
    /// Row (within the column) the scan is at.
    pub offset_in_column: u64,
    /// Whether the current segment's zone map was already checked.
    pub segment_checked: bool,
    /// Validity first, then list/array child or struct fields.
    pub child_states: Vec<ColumnScanState>,
    /// The child offset after the last scanned list (LIST columns).
    last_offset: u64,
    /// Which struct fields are scanned (all when empty).
    pub scan_child_column: Vec<bool>,
    pub update_scan_type: UpdateScanType,
}

impl ColumnScanState {
    /// A scan state shaped for a column of type `ty` (`ColumnScanState::Initialize`).
    pub fn for_type(ty: &LogicalType) -> Self {
        let mut state = Self::default();
        if ty.id() == LogicalTypeId::Validity {
            return state;
        }
        state.child_states.push(Self::default());
        match ty.physical_type() {
            PhysicalType::Struct => {
                let fields = ty.struct_children().unwrap_or_default();
                state
                    .child_states
                    .extend(fields.iter().map(|(_, field)| Self::for_type(field)));
                state.scan_child_column = vec![true; fields.len()];
            }
            PhysicalType::List => {
                state
                    .child_states
                    .push(Self::for_type(ty.child_type().unwrap_or(&LogicalType::INVALID)));
            }
            PhysicalType::Array => {
                let child = ty
                    .array_child()
                    .map_or(LogicalType::INVALID, |(child, _)| child.clone());
                state.child_states.push(Self::for_type(&child));
            }
            _ => {}
        }
        state
    }

    /// Moves the scan forward by `count` rows without reading (`Next`).
    pub fn next(&mut self, count: u64) {
        self.offset_in_column += count;
        for child in &mut self.child_states {
            child.next(count);
        }
    }
}

/// The state of an append to one column (DuckDB's `ColumnAppendState`).
#[derive(Debug, Default)]
pub struct ColumnAppendState {
    current: Option<SegmentNode<ColumnSegment>>,
    segment_state: Option<SegmentAppendState>,
    child_appends: Vec<ColumnAppendState>,
}

/// The data of one column of a row group.
#[derive(Debug)]
pub struct ColumnData {
    ty: LogicalType,
    column_index: usize,
    count: AtomicU64,
    data_type: AtomicU8,
    segments: SegmentTree<ColumnSegment>,
    updates: Mutex<Option<Arc<UpdateSegment>>>,
    /// Statistics of the whole column; only top-level columns have them (the statistics of
    /// nested children live inside their parent's).
    stats: Option<Mutex<BaseStatistics>>,
    allocation_size: AtomicU64,
    io: Arc<TableIoManager>,
    kind: ColumnKind,
}

fn validity_type() -> LogicalType {
    LogicalType::simple(LogicalTypeId::Validity)
}

fn not_implemented_nested(what: &str, ty: &LogicalType) -> Error {
    Error::not_implemented(format!("{what} is not supported for columns of type {ty}"))
}

impl ColumnData {
    /// An empty top-level column (`ColumnData::CreateColumn`).
    pub fn new(
        io: Arc<TableIoManager>,
        column_index: usize,
        ty: LogicalType,
        data_type: ColumnDataType,
    ) -> Self {
        Self::create(io, column_index, ty, data_type, false)
    }

    fn create(
        io: Arc<TableIoManager>,
        column_index: usize,
        ty: LogicalType,
        data_type: ColumnDataType,
        has_parent: bool,
    ) -> Self {
        let validity = || Box::new(Self::create(Arc::clone(&io), 0, validity_type(), data_type, true));
        let kind = if ty.id() == LogicalTypeId::Validity {
            ColumnKind::Validity
        } else {
            match ty.physical_type() {
                PhysicalType::List => ColumnKind::List {
                    validity: validity(),
                    child: Box::new(Self::create(
                        Arc::clone(&io),
                        1,
                        ty.child_type().cloned().unwrap_or_default(),
                        data_type,
                        true,
                    )),
                },
                PhysicalType::Struct => ColumnKind::Struct {
                    validity: validity(),
                    fields: ty
                        .struct_children()
                        .unwrap_or_default()
                        .iter()
                        .enumerate()
                        .map(|(index, (_, field))| {
                            Self::create(Arc::clone(&io), index + 1, field.clone(), data_type, true)
                        })
                        .collect(),
                },
                PhysicalType::Array => ColumnKind::Array {
                    validity: validity(),
                    child: Box::new(Self::create(
                        Arc::clone(&io),
                        1,
                        ty.array_child()
                            .map(|(child, _)| child.clone())
                            .unwrap_or_default(),
                        data_type,
                        true,
                    )),
                },
                _ => ColumnKind::Standard { validity: validity() },
            }
        };
        let stats = (!has_parent).then(|| Mutex::new(BaseStatistics::create_empty(ty.clone())));
        let data_type = if data_type == ColumnDataType::CheckpointTarget {
            ColumnDataType::MainTable
        } else {
            data_type
        };
        Self {
            ty,
            column_index,
            count: AtomicU64::new(0),
            data_type: AtomicU8::new(data_type as u8),
            segments: SegmentTree::default(),
            updates: Mutex::default(),
            stats,
            allocation_size: AtomicU64::new(0),
            io,
            kind,
        }
    }

    pub fn logical_type(&self) -> &LogicalType {
        &self.ty
    }

    pub fn column_index(&self) -> usize {
        self.column_index
    }

    pub fn kind(&self) -> &ColumnKind {
        &self.kind
    }

    /// Rows in the column.
    pub fn count(&self) -> u64 {
        self.count.load(Ordering::Acquire)
    }

    /// The segments of the column.
    pub fn segments(&self) -> &SegmentTree<ColumnSegment> {
        &self.segments
    }

    /// Bytes of transient segments allocated by appends.
    pub fn allocation_size(&self) -> u64 {
        self.allocation_size.load(Ordering::Relaxed)
            + match &self.kind {
                ColumnKind::Validity => 0,
                ColumnKind::Standard { validity } => validity.allocation_size(),
                ColumnKind::List { validity, child } | ColumnKind::Array { validity, child } => {
                    validity.allocation_size() + child.allocation_size()
                }
                ColumnKind::Struct { validity, fields } => {
                    validity.allocation_size() + fields.iter().map(Self::allocation_size).sum::<u64>()
                }
            }
    }

    pub fn data_type(&self) -> ColumnDataType {
        ColumnDataType::from_u8(self.data_type.load(Ordering::Relaxed))
    }

    /// Changes who owns the data, recursively (`SetDataType`).
    pub fn set_data_type(&self, data_type: ColumnDataType) {
        self.data_type.store(data_type as u8, Ordering::Relaxed);
        self.for_each_child(|child| child.set_data_type(data_type));
    }

    fn for_each_child(&self, mut f: impl FnMut(&Self)) {
        match &self.kind {
            ColumnKind::Validity => {}
            ColumnKind::Standard { validity } => f(validity),
            ColumnKind::List { validity, child } | ColumnKind::Array { validity, child } => {
                f(validity);
                f(child);
            }
            ColumnKind::Struct { validity, fields } => {
                f(validity);
                fields.iter().for_each(f);
            }
        }
    }

    fn validity_child(&self) -> Option<&Self> {
        match &self.kind {
            ColumnKind::Validity => None,
            ColumnKind::Standard { validity }
            | ColumnKind::List { validity, .. }
            | ColumnKind::Struct { validity, .. }
            | ColumnKind::Array { validity, .. } => Some(validity),
        }
    }

    fn array_size(&self) -> u64 {
        self.ty.array_child().map_or(0, |(_, size)| u64::from(size))
    }

    fn updates(&self) -> MutexGuard<'_, Option<Arc<UpdateSegment>>> {
        self.updates.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn update_segment(&self) -> Option<Arc<UpdateSegment>> {
        self.updates().clone()
    }

    /// Whether this column (not its children) has updates.
    pub fn has_updates(&self) -> bool {
        self.updates().is_some()
    }

    /// Whether the column or any child has updates.
    pub fn has_any_updates(&self) -> bool {
        let mut any = self.has_updates();
        self.for_each_child(|child| any |= child.has_any_updates());
        any
    }

    // ----------------------------------------------------------------------------------
    // Statistics
    // ----------------------------------------------------------------------------------

    fn stats_lock(&self) -> Result<MutexGuard<'_, BaseStatistics>> {
        self.stats
            .as_ref()
            .map(|stats| stats.lock().unwrap_or_else(PoisonError::into_inner))
            .ok_or_else(|| Error::internal("statistics requested of a nested child column"))
    }

    /// A copy of the column statistics (`GetStatistics`).
    pub fn statistics(&self) -> Result<BaseStatistics> {
        Ok(self.stats_lock()?.clone())
    }

    /// Merges `other` into the column statistics (`MergeStatistics`).
    pub fn merge_statistics(&self, other: &BaseStatistics) -> Result<()> {
        self.stats_lock()?.merge(other);
        Ok(())
    }

    /// Merges the column statistics into `other` (`MergeIntoStatistics`).
    pub fn merge_into_statistics(&self, other: &mut BaseStatistics) -> Result<()> {
        let stats = self.stats_lock()?;
        other.merge(&stats);
        Ok(())
    }

    /// The statistics of every value written by updates of this column and its validity
    /// (`GetUpdateStatistics`).
    pub fn update_statistics(&self) -> Option<BaseStatistics> {
        let own = self.update_segment().map(|segment| segment.statistics());
        let validity = self.validity_child().and_then(Self::update_statistics);
        match (own, validity) {
            (None, None) => None,
            (own, validity) => {
                let mut stats = own.unwrap_or_else(|| BaseStatistics::create_empty(self.ty.clone()));
                if let Some(validity) = validity {
                    stats.merge(&validity);
                }
                Some(stats)
            }
        }
    }

    /// Whether `filter` can be true for the segment being scanned (`CheckZonemap`).
    pub fn check_zonemap(&self, state: &mut ColumnScanState, filter: &TableFilter) -> FilterPropagateResult {
        if state.segment_checked {
            return FilterPropagateResult::NoPruningPossible;
        }
        let Some((_, current)) = &state.current else {
            return FilterPropagateResult::NoPruningPossible;
        };
        state.segment_checked = !matches!(filter, TableFilter::Dynamic(_));
        let result = filter.check_statistics(&current.node.statistics());
        if result == FilterPropagateResult::NoPruningPossible {
            return result;
        }
        match self.update_statistics() {
            None => result,
            Some(update_stats) if filter.check_statistics(&update_stats) == result => result,
            Some(_) => FilterPropagateResult::NoPruningPossible,
        }
    }

    /// Whether `filter` can be true for any value of the column (row-group zone map).
    pub fn check_zonemap_column(&self, filter: &TableFilter) -> FilterPropagateResult {
        match self.stats_lock() {
            Ok(stats) => filter.check_statistics(&stats),
            Err(_) => FilterPropagateResult::NoPruningPossible,
        }
    }

    // ----------------------------------------------------------------------------------
    // Scans
    // ----------------------------------------------------------------------------------

    /// Positions `state` at the start of the column (`InitializeScan`).
    pub fn initialize_scan(&self, state: &mut ColumnScanState) {
        state.current = self.segments.root().map(|node| (0, node));
        state.offset_in_column = state.current.as_ref().map_or(0, |(_, node)| node.row_start);
        state.segment_state = None;
        state.segment_checked = false;
        state.last_offset = 0;
        self.initialize_children(state, 0);
    }

    /// Positions `state` at row `row` of the column (`InitializeScanWithOffset`).
    pub fn initialize_scan_with_offset(&self, state: &mut ColumnScanState, row: u64) -> Result<()> {
        if row > self.count() {
            return Err(Error::internal(
                "row_idx in InitializeScanWithOffset out of range",
            ));
        }
        if row == 0 {
            self.initialize_scan(state);
            return Ok(());
        }
        state.current = self.segments.find(row);
        state.offset_in_column = row;
        state.segment_state = None;
        state.segment_checked = false;
        state.last_offset = 0;
        let ColumnKind::List { validity, child } = &self.kind else {
            self.initialize_children(state, row);
            return Ok(());
        };
        if let Some(validity_state) = state.child_states.first_mut() {
            validity.initialize_scan_with_offset(validity_state, row)?;
        }
        let child_offset = self.fetch_list_offset(row - 1)?;
        if let Some(child_state) = state.child_states.get_mut(1) {
            if child_offset < child.count() {
                child.initialize_scan_with_offset(child_state, child_offset)?;
            } else {
                child.initialize_scan(child_state);
                child_state.next(child_offset);
            }
        }
        state.last_offset = child_offset;
        Ok(())
    }

    fn initialize_children(&self, state: &mut ColumnScanState, row: u64) {
        let init = |child: &Self, child_state: &mut ColumnScanState, child_row: u64| {
            if child_row == 0 || child.initialize_scan_with_offset(child_state, child_row).is_err() {
                child.initialize_scan(child_state);
            }
        };
        let states = &mut state.child_states;
        match &self.kind {
            ColumnKind::Validity => {}
            ColumnKind::Standard { validity } => {
                if let Some(child_state) = states.first_mut() {
                    init(validity, child_state, row);
                }
            }
            ColumnKind::List { validity, child } => {
                if let [validity_state, child_state, ..] = states.as_mut_slice() {
                    init(validity, validity_state, row);
                    init(child, child_state, 0);
                }
            }
            ColumnKind::Array { validity, child } => {
                if let [validity_state, child_state, ..] = states.as_mut_slice() {
                    init(validity, validity_state, row);
                    init(child, child_state, row * self.array_size());
                }
            }
            ColumnKind::Struct { validity, fields } => {
                if let Some((validity_state, field_states)) = states.split_first_mut() {
                    init(validity, validity_state, row);
                    for (field, field_state) in fields.iter().zip(field_states) {
                        init(field, field_state, row);
                    }
                }
            }
        }
    }

    /// Number of rows of vector `vector_index` (`GetVectorCount`).
    pub fn vector_count(&self, vector_index: usize) -> usize {
        let start = vector_index as u64 * VECTOR_SIZE;
        self.count().saturating_sub(start).min(VECTOR_SIZE) as usize
    }

    /// Reads `count` rows of this column's own segments into `result` at `result_offset`
    /// (`ScanVector` without updates).
    fn scan_segments(
        &self,
        state: &mut ColumnScanState,
        result: &mut Vector,
        count: usize,
        result_offset: usize,
    ) -> Result<usize> {
        let mut remaining = count;
        while remaining > 0 {
            let Some((index, node)) = state.current.clone() else {
                break;
            };
            let segment_end = node.row_end();
            if state.offset_in_column >= segment_end {
                let Some(next) = self.segments.get(index + 1) else {
                    break;
                };
                state.current = Some((index + 1, next));
                state.segment_state = None;
                state.segment_checked = false;
                continue;
            }
            if state.segment_state.is_none() {
                state.segment_state = Some(node.node.init_scan()?);
            }
            let scan_count = remaining.min((segment_end - state.offset_in_column) as usize);
            let segment_state = state
                .segment_state
                .as_mut()
                .ok_or_else(|| Error::internal("segment scan state missing"))?;
            node.node.scan(
                segment_state,
                state.offset_in_column - node.row_start,
                scan_count,
                result,
                result_offset + count - remaining,
            )?;
            state.offset_in_column += scan_count as u64;
            remaining -= scan_count;
        }
        Ok(count - remaining)
    }

    /// Prepares `result` for scanning `count` rows into it.
    fn prepare_result(result: &mut Vector, count: usize) {
        let capacity = count.max(STANDARD_VECTOR_SIZE);
        if result.kind() != VectorKind::Flat || result.row_capacity() < capacity {
            *result = Vector::flat(result.logical_type().clone(), capacity);
        } else {
            result.reset_flat(capacity);
        }
    }

    /// Scans vector `vector_index` (`count` rows) of the column with the updates visible to
    /// `transaction` into `result` (`ColumnData::Scan`).
    pub fn scan(
        &self,
        transaction: TransactionData,
        vector_index: usize,
        state: &mut ColumnScanState,
        result: &mut Vector,
        count: usize,
    ) -> Result<usize> {
        Self::prepare_result(result, count);
        self.scan_into(Some((transaction, vector_index)), state, result, count, 0)
    }

    /// Scans `count` rows without merging updates into `result` at `result_offset`
    /// (`ScanCount`).
    pub fn scan_count(
        &self,
        state: &mut ColumnScanState,
        result: &mut Vector,
        count: usize,
        result_offset: usize,
    ) -> Result<usize> {
        if result_offset == 0 {
            Self::prepare_result(result, count);
        }
        self.scan_into(None, state, result, count, result_offset)
    }

    /// Scans committed data (newest values of updates included) of rows
    /// `offset..offset + count` of the column into `result` (`ScanCommittedRange`).
    pub fn scan_committed_range(&self, offset: u64, count: usize, result: &mut Vector) -> Result<usize> {
        let mut state = ColumnScanState::for_type(&self.ty);
        self.initialize_scan_with_offset(&mut state, offset)?;
        Self::prepare_result(result, count);
        let scanned = self.scan_into(None, &mut state, result, count, 0)?;
        self.merge_committed_updates(offset, count, result);
        Ok(scanned)
    }

    fn merge_committed_updates(&self, offset: u64, count: usize, result: &mut Vector) {
        if let Some(updates) = self.update_segment() {
            updates.fetch_committed_range(offset, count as u64, result);
        }
        match &self.kind {
            ColumnKind::Standard { validity } => validity.merge_committed_updates(offset, count, result),
            ColumnKind::Struct { validity, fields } => {
                validity.merge_committed_updates(offset, count, result);
                if let Some(children) = result.struct_children_mut() {
                    for (field, child) in fields.iter().zip(children.iter_mut()) {
                        field.merge_committed_updates(offset, count, child);
                    }
                }
            }
            _ => {}
        }
    }

    /// The shared scan routine. `updates` is the transaction and vector index whose
    /// updates are merged (top-level and struct-field scans); `None` scans raw data.
    fn scan_into(
        &self,
        updates: Option<(TransactionData, usize)>,
        state: &mut ColumnScanState,
        result: &mut Vector,
        count: usize,
        result_offset: usize,
    ) -> Result<usize> {
        match &self.kind {
            ColumnKind::Validity => {
                let scanned = self.scan_segments(state, result, count, result_offset)?;
                if let Some((transaction, vector_index)) = updates {
                    self.fetch_updates(transaction, vector_index, state.update_scan_type, result)?;
                }
                Ok(scanned)
            }
            ColumnKind::Standard { validity } => {
                let scanned = self.scan_segments(state, result, count, result_offset)?;
                if let Some((transaction, vector_index)) = updates {
                    self.fetch_updates(transaction, vector_index, state.update_scan_type, result)?;
                }
                if let Some(validity_state) = state.child_states.first_mut() {
                    validity_state.update_scan_type = state.update_scan_type;
                    validity.scan_into(updates, validity_state, result, count, result_offset)?;
                }
                Ok(scanned)
            }
            ColumnKind::List { validity, child } => {
                if result_offset > 0 {
                    return Err(Error::internal(
                        "ListColumnData::ScanCount not supported with result_offset > 0",
                    ));
                }
                self.scan_list(validity, child, state, result, count)
            }
            ColumnKind::Struct { validity, fields } => {
                let [validity_state, field_states @ ..] = state.child_states.as_mut_slice() else {
                    return Err(Error::internal("struct scan state without children"));
                };
                let scanned = validity.scan_into(updates, validity_state, result, count, result_offset)?;
                let scan_all = state.scan_child_column.is_empty();
                let children = result
                    .struct_children_mut()
                    .ok_or_else(|| Error::internal("struct scan into a non-struct vector"))?;
                for (index, ((field, field_state), child)) in fields
                    .iter()
                    .zip(field_states)
                    .zip(children.iter_mut())
                    .enumerate()
                {
                    if scan_all || state.scan_child_column.get(index).copied().unwrap_or(true) {
                        field.scan_into(updates, field_state, child, count, result_offset)?;
                    } else {
                        field_state.next(count as u64);
                    }
                }
                state.offset_in_column += count as u64;
                Ok(scanned)
            }
            ColumnKind::Array { validity, child } => {
                let [validity_state, child_state, ..] = state.child_states.as_mut_slice() else {
                    return Err(Error::internal("array scan state without children"));
                };
                let scanned = validity.scan_into(None, validity_state, result, count, result_offset)?;
                let array_size = self.array_size() as usize;
                let child_vector = result
                    .array_child_mut()
                    .ok_or_else(|| Error::internal("array scan into a non-array vector"))?;
                let child_rows = (result_offset + count) * array_size;
                if child_vector.row_capacity() < child_rows {
                    child_vector.resize(child_rows);
                }
                child.scan_into(
                    None,
                    child_state,
                    child_vector,
                    count * array_size,
                    result_offset * array_size,
                )?;
                state.offset_in_column += count as u64;
                Ok(scanned)
            }
        }
    }

    fn scan_list(
        &self,
        validity: &Self,
        child: &Self,
        state: &mut ColumnScanState,
        result: &mut Vector,
        count: usize,
    ) -> Result<usize> {
        if count == 0 {
            return Ok(0);
        }
        let mut offsets = Vector::flat(LogicalType::UBIGINT, count);
        let scanned = self.scan_segments(state, &mut offsets, count, 0)?;
        let [validity_state, child_state, ..] = state.child_states.as_mut_slice() else {
            return Err(Error::internal("list scan state without children"));
        };
        validity.scan_into(None, validity_state, result, scanned, 0)?;
        let ends = offsets.values::<u64>();
        let base = state.last_offset;
        let last = ends.get(scanned.wrapping_sub(1)).copied().unwrap_or(base);
        let child_count = (last - base) as usize;
        let mut current = 0u64;
        {
            let entries = result.values_mut::<ListEntry>();
            for (entry, &end) in entries.iter_mut().zip(&ends[..scanned]) {
                let length = end - current - base;
                *entry = ListEntry {
                    offset: current,
                    length,
                };
                current += length;
            }
        }
        let child_type = child.logical_type().clone();
        let mut child_vector = Vector::flat(child_type, child_count.max(1));
        if child_count > 0 {
            child.scan_into(None, child_state, &mut child_vector, child_count, 0)?;
        }
        result.set_list_child(child_vector, child_count);
        state.last_offset = last;
        Ok(scanned)
    }

    /// Merges the updates visible to `transaction` into vector `vector_index` of the scan
    /// (`FetchUpdates`).
    fn fetch_updates(
        &self,
        transaction: TransactionData,
        vector_index: usize,
        update_type: UpdateScanType,
        result: &mut Vector,
    ) -> Result<()> {
        let Some(updates) = self.update_segment() else {
            return Ok(());
        };
        if update_type == UpdateScanType::DisallowUpdates && updates.has_uncommitted_updates(vector_index) {
            return Err(Error::new(
                rustdb_common::ErrorKind::Transaction,
                "Cannot create index with outstanding updates",
            ));
        }
        updates.fetch_updates(transaction, vector_index, result);
        Ok(())
    }

    /// Advances the scan by `count` rows (`Skip`).
    pub fn skip(&self, state: &mut ColumnScanState, count: u64) -> Result<()> {
        match &self.kind {
            ColumnKind::List { validity, child } => {
                if let Some(validity_state) = state.child_states.first_mut() {
                    validity.skip(validity_state, count)?;
                }
                if count == 0 {
                    return Ok(());
                }
                let mut offsets = Vector::flat(LogicalType::UBIGINT, count as usize);
                let scanned = self.scan_segments(state, &mut offsets, count as usize, 0)?;
                let last = offsets
                    .values::<u64>()
                    .get(scanned.wrapping_sub(1))
                    .copied()
                    .unwrap_or(state.last_offset);
                let child_count = last - state.last_offset;
                state.last_offset = last;
                if let Some(child_state) = state.child_states.get_mut(1) {
                    child.skip(child_state, child_count)?;
                }
                Ok(())
            }
            ColumnKind::Array { validity, child } => {
                state.offset_in_column += count;
                if let [validity_state, child_state, ..] = state.child_states.as_mut_slice() {
                    validity.skip(validity_state, count)?;
                    child.skip(child_state, count * self.array_size())?;
                }
                Ok(())
            }
            ColumnKind::Struct { validity, fields } => {
                state.offset_in_column += count;
                if let Some((validity_state, field_states)) = state.child_states.split_first_mut() {
                    validity.skip(validity_state, count)?;
                    for (field, field_state) in fields.iter().zip(field_states) {
                        field.skip(field_state, count)?;
                    }
                }
                Ok(())
            }
            ColumnKind::Validity | ColumnKind::Standard { .. } => {
                state.offset_in_column += count;
                if let (ColumnKind::Standard { validity }, Some(validity_state)) =
                    (&self.kind, state.child_states.first_mut())
                {
                    validity.skip(validity_state, count)?;
                }
                Ok(())
            }
        }
    }

    /// Scans the vector and keeps only the rows of `sel` (`Select`).
    pub fn select(
        &self,
        transaction: TransactionData,
        vector_index: usize,
        state: &mut ColumnScanState,
        result: &mut Vector,
        sel: &SelectionVector,
    ) -> Result<()> {
        let count = self.vector_count(vector_index);
        self.scan(transaction, vector_index, state, result, count)?;
        *result = result.slice(sel);
        Ok(())
    }

    /// Scans the vector and keeps the rows of `sel` that pass `filter` (`Filter`).
    pub fn filter(
        &self,
        transaction: TransactionData,
        vector_index: usize,
        state: &mut ColumnScanState,
        result: &mut Vector,
        sel: &mut Vec<u32>,
        filter: &TableFilter,
    ) -> Result<()> {
        let count = self.vector_count(vector_index);
        self.scan(transaction, vector_index, state, result, count)?;
        filter.select(result, count, sel)
    }

    // ----------------------------------------------------------------------------------
    // Appends
    // ----------------------------------------------------------------------------------

    /// Adds a fresh transient segment starting at row `start_row` (`AppendTransientSegment`).
    fn append_transient_segment(&self, start_row: u64) -> Result<SegmentNode<ColumnSegment>> {
        let block_manager = self.io.block_manager();
        let block_size = block_manager.block_size();
        let type_size = match self.ty.physical_type() {
            PhysicalType::Bit | PhysicalType::Bool => 1,
            other => other.size(),
        };
        let mut segment_size = block_size;
        if self.data_type() == ColumnDataType::InitialTransactionLocal && start_row == 0 {
            segment_size = (STANDARD_VECTOR_SIZE * type_size).min(block_size);
        }
        let segment = ColumnSegment::create_transient(
            self.io.buffer_manager(),
            block_manager.as_ref(),
            self.ty.clone(),
            segment_size,
        )?;
        self.allocation_size
            .fetch_add(segment_size as u64, Ordering::Relaxed);
        let node = Arc::new(segment);
        self.segments.push(start_row, Arc::clone(&node));
        Ok(SegmentNode {
            row_start: start_row,
            node,
        })
    }

    /// A column of type `ty` whose segments are stored in the database file
    /// (`ColumnData::Deserialize` + `InitializeColumn`).
    pub fn from_persistent(
        io: Arc<TableIoManager>,
        column_index: usize,
        ty: LogicalType,
        data: &crate::checkpoint::pointers::PersistentColumnData,
    ) -> Result<Self> {
        let column = Self::new(io, column_index, ty, ColumnDataType::MainTable);
        let mut stats = BaseStatistics::create_empty(column.ty.clone());
        column.initialize_persistent(data, &mut stats)?;
        if let Some(own) = &column.stats {
            *own.lock().unwrap_or_else(PoisonError::into_inner) = stats;
        }
        Ok(column)
    }

    /// Creates the persistent segments of `data` and of the children
    /// (`InitializeColumn`); their statistics are merged into `stats`.
    fn initialize_persistent(
        &self,
        data: &crate::checkpoint::pointers::PersistentColumnData,
        stats: &mut BaseStatistics,
    ) -> Result<()> {
        let buffer_manager = self.io.buffer_manager();
        let block_manager = self.io.block_manager();
        let mut count = 0u64;
        for pointer in &data.pointers {
            let segment = ColumnSegment::create_persistent(
                buffer_manager,
                block_manager,
                pointer.block_pointer.block_id,
                pointer.block_pointer.offset as usize,
                self.ty.clone(),
                pointer.tuple_count,
                pointer.compression,
                pointer.statistics.clone(),
                pointer.overflow_blocks.clone(),
            )?;
            if self.ty.id() == LogicalTypeId::Validity {
                stats.merge_validity(&pointer.statistics);
            } else {
                stats.merge(&pointer.statistics);
            }
            self.segments.push(count, Arc::new(segment));
            count += pointer.tuple_count;
        }
        self.count.store(count, Ordering::Release);
        let child = |index: usize| {
            data.child_columns
                .get(index)
                .ok_or_else(|| Error::internal("persistent column data without its children"))
        };
        match &self.kind {
            ColumnKind::Validity => {}
            ColumnKind::Standard { validity } => validity.initialize_persistent(child(0)?, stats)?,
            ColumnKind::List {
                validity,
                child: list_child,
            }
            | ColumnKind::Array {
                validity,
                child: list_child,
            } => {
                validity.initialize_persistent(child(0)?, stats)?;
                let mut child_stats = BaseStatistics::create_empty(list_child.ty.clone());
                list_child.initialize_persistent(child(1)?, &mut child_stats)?;
                if let Some(target) = stats.child_mut() {
                    target.merge(&child_stats);
                }
            }
            ColumnKind::Struct { validity, fields } => {
                validity.initialize_persistent(child(0)?, stats)?;
                for (index, field) in fields.iter().enumerate() {
                    let mut field_stats = BaseStatistics::create_empty(field.ty.clone());
                    field.initialize_persistent(child(index + 1)?, &mut field_stats)?;
                    if let Some(target) = stats.struct_children_mut().and_then(|c| c.get_mut(index)) {
                        target.merge(&field_stats);
                    }
                }
            }
        }
        Ok(())
    }

    /// A copy of the column's committed data (with committed updates applied) in fresh
    /// transient segments, for writing at a checkpoint (`ColumnData::Checkpoint` re-reads
    /// the column the same way).
    pub fn checkpoint_copy(&self, count: u64) -> Result<Self> {
        let target = Self::new(
            Arc::clone(&self.io),
            self.column_index,
            self.ty.clone(),
            ColumnDataType::CheckpointTarget,
        );
        let mut state = target.initialize_append()?;
        let mut vector = Vector::flat(self.ty.clone(), STANDARD_VECTOR_SIZE);
        let mut offset = 0u64;
        while offset < count {
            let rows = (count - offset).min(VECTOR_SIZE) as usize;
            let scanned = self.scan_committed_range(offset, rows, &mut vector)?;
            target.append(&mut state, &vector, scanned)?;
            offset += rows as u64;
        }
        Ok(target)
    }

    /// Prepares appending to the column (`InitializeAppend`).
    pub fn initialize_append(&self) -> Result<ColumnAppendState> {
        let mut state = ColumnAppendState::default();
        if !matches!(self.kind, ColumnKind::Struct { .. } | ColumnKind::Array { .. }) {
            let last = self.segments.last();
            let current = match last {
                Some(node)
                    if node.node.segment_type() == super::column_segment::SegmentType::Transient
                        && node.node.compression() == crate::compression::CompressionType::Uncompressed =>
                {
                    node
                }
                Some(node) => self.append_transient_segment(node.row_end())?,
                None => self.append_transient_segment(0)?,
            };
            state.segment_state = Some(current.node.init_append()?);
            state.current = Some(current);
        }
        let mut push_child = |child: &Self| -> Result<()> {
            state.child_appends.push(child.initialize_append()?);
            Ok(())
        };
        match &self.kind {
            ColumnKind::Validity => {}
            ColumnKind::Standard { validity } => push_child(validity)?,
            ColumnKind::List { validity, child } | ColumnKind::Array { validity, child } => {
                push_child(validity)?;
                push_child(child)?;
            }
            ColumnKind::Struct { validity, fields } => {
                push_child(validity)?;
                for field in fields {
                    push_child(field)?;
                }
            }
        }
        Ok(state)
    }

    /// Appends `count` rows of `vector` to a top-level column, updating its statistics.
    pub fn append(&self, state: &mut ColumnAppendState, vector: &Vector, count: usize) -> Result<()> {
        let stats = self.stats.as_ref().ok_or_else(|| {
            Error::internal("ColumnData::Append called on a column with a parent or without stats")
        })?;
        let mut stats = stats.lock().unwrap_or_else(PoisonError::into_inner);
        self.append_with_stats(&mut stats, state, vector, count)
    }

    /// Appends `count` rows of `vector`, merging into `stats` (the parent's statistics
    /// for nested children).
    pub fn append_with_stats(
        &self,
        stats: &mut BaseStatistics,
        state: &mut ColumnAppendState,
        vector: &Vector,
        count: usize,
    ) -> Result<()> {
        if count == 0 {
            return Ok(());
        }
        let flat;
        let vector = if vector.kind() == VectorKind::Sequence || self.ty.is_nested() {
            flat = vector.flattened(count);
            &flat
        } else {
            vector
        };
        match &self.kind {
            ColumnKind::Validity => self.append_data(stats, state, vector, count),
            ColumnKind::Standard { validity } => {
                self.append_data(stats, state, vector, count)?;
                let validity_state = state
                    .child_appends
                    .first_mut()
                    .ok_or_else(|| Error::internal("append state without validity"))?;
                validity.append_data(stats, validity_state, vector, count)
            }
            ColumnKind::List { validity, child } => {
                self.append_list(validity, child, stats, state, vector, count)
            }
            ColumnKind::Struct { validity, fields } => {
                let [validity_state, field_states @ ..] = state.child_appends.as_mut_slice() else {
                    return Err(Error::internal("struct append state without children"));
                };
                validity.append_data(stats, validity_state, vector, count)?;
                let children = vector
                    .struct_children()
                    .ok_or_else(|| Error::internal("struct append of a non-struct vector"))?;
                let child_stats = stats
                    .struct_children_mut()
                    .ok_or_else(|| Error::internal("struct column without struct statistics"))?;
                for (((field, field_state), child), field_stats) in
                    fields.iter().zip(field_states).zip(children).zip(child_stats)
                {
                    field.append_with_stats(field_stats, field_state, child, count)?;
                }
                self.count.fetch_add(count as u64, Ordering::AcqRel);
                Ok(())
            }
            ColumnKind::Array { validity, child } => {
                let [validity_state, child_state, ..] = state.child_appends.as_mut_slice() else {
                    return Err(Error::internal("array append state without children"));
                };
                validity.append_data(stats, validity_state, vector, count)?;
                let child_vector = vector
                    .array_child()
                    .ok_or_else(|| Error::internal("array append of a non-array vector"))?;
                let child_stats = stats
                    .child_mut()
                    .ok_or_else(|| Error::internal("array column without child statistics"))?;
                child.append_with_stats(
                    child_stats,
                    child_state,
                    child_vector,
                    count * self.array_size() as usize,
                )?;
                self.count.fetch_add(count as u64, Ordering::AcqRel);
                Ok(())
            }
        }
    }

    /// Appends to this column's own segments, opening new segments as they fill up
    /// (`AppendData`).
    fn append_data(
        &self,
        stats: &mut BaseStatistics,
        state: &mut ColumnAppendState,
        vector: &Vector,
        count: usize,
    ) -> Result<()> {
        let mut offset = 0;
        let mut remaining = count;
        loop {
            let current = state
                .current
                .clone()
                .ok_or_else(|| Error::internal("append without a current segment"))?;
            let segment_state = state
                .segment_state
                .as_mut()
                .ok_or_else(|| Error::internal("append without a segment append state"))?;
            let copied = current.node.append(segment_state, vector, offset, remaining)?;
            self.count.fetch_add(copied as u64, Ordering::AcqRel);
            stats.merge(&current.node.statistics());
            if copied == remaining {
                return Ok(());
            }
            let next = self.append_transient_segment(current.row_end())?;
            state.segment_state = Some(next.node.init_append()?);
            state.current = Some(next);
            offset += copied;
            remaining -= copied;
        }
    }

    fn append_list(
        &self,
        validity: &Self,
        child: &Self,
        stats: &mut BaseStatistics,
        state: &mut ColumnAppendState,
        vector: &Vector,
        count: usize,
    ) -> Result<()> {
        let entries = vector.unified::<ListEntry>();
        let start_offset = child.count();
        let mut child_rows: Vec<u32> = Vec::new();
        let mut contiguous = true;
        let mut child_count = 0u64;
        let mut ends = Vector::flat(LogicalType::UBIGINT, count);
        let mut mask = Vector::flat(LogicalType::UBIGINT, count);
        {
            let end_values = ends.values_mut::<u64>();
            for (row, end) in end_values.iter_mut().enumerate() {
                if let Some(entry) = entries.get(row) {
                    if entry.offset != child_count {
                        contiguous = false;
                    }
                    child_rows.extend((entry.offset..entry.offset + entry.length).map(|r| r as u32));
                    child_count += entry.length;
                }
                *end = start_offset + child_count;
            }
        }
        for row in 0..count {
            if !entries.is_valid(row) {
                mask.validity_mut().set_invalid(row, count);
            }
        }
        if child_count > 0 {
            let child_state = state
                .child_appends
                .get_mut(1)
                .ok_or_else(|| Error::internal("list append state without children"))?;
            let (list_child, _) = vector
                .list_child()
                .ok_or_else(|| Error::internal("list append of a non-list vector"))?;
            let child_stats = stats
                .child_mut()
                .ok_or_else(|| Error::internal("list column without child statistics"))?;
            if contiguous {
                child.append_with_stats(child_stats, child_state, list_child, child_count as usize)?;
            } else {
                let sliced = list_child.slice(&SelectionVector::new(child_rows));
                child.append_with_stats(child_stats, child_state, &sliced, child_count as usize)?;
            }
        }
        // The offsets are appended without statistics (`ListFixedSizeAppend`).
        let mut offsets_stats = BaseStatistics::create_empty(LogicalType::UBIGINT);
        self.append_data(&mut offsets_stats, state, &ends, count)?;
        let validity_state = state
            .child_appends
            .first_mut()
            .ok_or_else(|| Error::internal("list append state without validity"))?;
        validity.append_data(stats, validity_state, &mask, count)
    }

    /// Removes rows at and after `new_count` (`RevertAppend`).
    pub fn revert_append(&self, new_count: u64) -> Result<()> {
        match &self.kind {
            ColumnKind::Struct { validity, fields } => {
                validity.revert_append(new_count)?;
                for field in fields {
                    field.revert_append(new_count)?;
                }
                self.count.store(new_count, Ordering::Release);
                return Ok(());
            }
            ColumnKind::Array { validity, child } => {
                validity.revert_append(new_count)?;
                child.revert_append(new_count * self.array_size())?;
                self.count.store(new_count, Ordering::Release);
                return Ok(());
            }
            _ => {}
        }
        self.revert_segments(new_count)?;
        match &self.kind {
            ColumnKind::Standard { validity } => validity.revert_append(new_count)?,
            ColumnKind::List { validity, child } => {
                validity.revert_append(new_count)?;
                let child_count = if new_count == 0 {
                    0
                } else {
                    self.fetch_list_offset(new_count - 1)?
                };
                child.revert_append(child_count)?;
            }
            _ => {}
        }
        Ok(())
    }

    fn revert_segments(&self, new_count: u64) -> Result<()> {
        let mut nodes = self.segments.write();
        let Some(last) = nodes.last() else { return Ok(()) };
        if new_count >= last.row_end() {
            return Ok(());
        }
        let index = nodes
            .partition_point(|node| node.row_start <= new_count)
            .saturating_sub(1);
        if nodes[index].row_start == new_count {
            nodes.truncate(index);
        } else {
            nodes.truncate(index + 1);
            let node = &nodes[index];
            node.node.revert_append(new_count - node.row_start)?;
        }
        self.count.store(new_count, Ordering::Release);
        Ok(())
    }

    /// The cumulative child end offset of list row `row` (`FetchListOffset`).
    fn fetch_list_offset(&self, row: u64) -> Result<u64> {
        let (_, node) = self
            .segments
            .find(row)
            .ok_or_else(|| Error::internal("list offset out of range"))?;
        let mut result = Vector::flat(LogicalType::UBIGINT, 1);
        node.node.fetch_row(row - node.row_start, &mut result, 0)?;
        Ok(result.values::<u64>()[0])
    }

    // ----------------------------------------------------------------------------------
    // Fetches and updates
    // ----------------------------------------------------------------------------------

    /// Reads row `row` (within the row group) with the updates visible to `transaction`
    /// into `result[result_index]` (`FetchRow`).
    pub fn fetch_row(
        &self,
        transaction: TransactionData,
        row: u64,
        result: &mut Vector,
        result_index: usize,
    ) -> Result<()> {
        if row > self.count() {
            return Err(Error::internal("ColumnData::FetchRow - row_id out of range"));
        }
        match &self.kind {
            ColumnKind::Validity | ColumnKind::Standard { .. } => {
                if let Some((_, node)) = self.segments.find(row) {
                    node.node.fetch_row(row - node.row_start, result, result_index)?;
                }
                if let Some(updates) = self.update_segment() {
                    updates.fetch_row(transaction, row, result, result_index);
                }
                if let ColumnKind::Standard { validity } = &self.kind {
                    validity.fetch_row(transaction, row, result, result_index)?;
                }
                Ok(())
            }
            ColumnKind::List { .. } | ColumnKind::Struct { .. } | ColumnKind::Array { .. } => {
                // Nested rows are materialised through a scan of their vector.
                let vector_start = row / VECTOR_SIZE * VECTOR_SIZE;
                let mut state = ColumnScanState::for_type(&self.ty);
                self.initialize_scan_with_offset(&mut state, vector_start)?;
                let count = (self.count() - vector_start).min(VECTOR_SIZE) as usize;
                let mut vector = Vector::flat(self.ty.clone(), count);
                let vector_index = (vector_start / VECTOR_SIZE) as usize;
                self.scan(transaction, vector_index, &mut state, &mut vector, count)?;
                let value: Value = vector.get_value((row - vector_start) as usize);
                if result.row_capacity() <= result_index {
                    result.resize(result_index + 1);
                }
                result.set_value(result_index, &value)
            }
        }
    }

    /// The flat vector of this column containing row `row` (`Fetch` + `Flatten`), used as
    /// the base data of updates.
    fn fetch_vector(&self, row: u64) -> Result<Vector> {
        let vector_start = row / VECTOR_SIZE * VECTOR_SIZE;
        let count = (self.count() - vector_start).min(VECTOR_SIZE) as usize;
        let mut state = ColumnScanState::for_type(&self.ty);
        self.initialize_scan_with_offset(&mut state, vector_start)?;
        let ty = if self.ty.id() == LogicalTypeId::Validity {
            LogicalType::BOOLEAN
        } else {
            self.ty.clone()
        };
        let mut vector = Vector::flat(ty, STANDARD_VECTOR_SIZE);
        self.scan_into(None, &mut state, &mut vector, count, 0)?;
        Ok(vector)
    }

    /// Updates the sub-column at `column_path` (validity is child 0, struct fields
    /// follow) like [`ColumnData::update`] (`UpdateColumn`); the WAL logs updates per
    /// sub-column.
    pub fn update_column(
        &self,
        transaction: TransactionData,
        column_path: &[usize],
        update: &Vector,
        row_ids: &[i64],
        row_group_start: u64,
    ) -> Result<Vec<UpdateRegistration>> {
        let Some((&first, rest)) = column_path.split_first() else {
            return self.update(transaction, update, row_ids, row_group_start);
        };
        let child = match (&self.kind, first) {
            (ColumnKind::Standard { validity } | ColumnKind::Struct { validity, .. }, 0) => validity,
            (ColumnKind::Struct { fields, .. }, index) => fields
                .get(index - 1)
                .ok_or_else(|| Error::internal("Corrupt WAL: column index for update out of bounds"))?,
            _ => {
                return Err(Error::internal(
                    "Corrupt WAL: column index for update out of bounds",
                ));
            }
        };
        let registrations = child.update_column(transaction, rest, update, row_ids, row_group_start)?;
        Ok(under_child(first, registrations).collect())
    }

    /// Updates rows `row_ids` (all in one vector of the row group starting at
    /// `row_group_start`) to the values of `update` (`Update`).
    ///
    /// Returns the transaction nodes created, which the caller registers in the undo log.
    pub fn update(
        &self,
        transaction: TransactionData,
        update: &Vector,
        row_ids: &[i64],
        row_group_start: u64,
    ) -> Result<Vec<UpdateRegistration>> {
        let Some(&first) = row_ids.first() else {
            return Ok(Vec::new());
        };
        let first_row = (first as u64)
            .checked_sub(row_group_start)
            .ok_or_else(|| Error::internal("ColumnData::FetchUpdateData out of range"))?;
        let flat;
        let update = if update.kind() == VectorKind::Sequence {
            flat = update.flattened(row_ids.len());
            &flat
        } else {
            update
        };
        let mut registrations = Vec::new();
        match &self.kind {
            ColumnKind::Validity | ColumnKind::Standard { .. } => {
                let base = self.fetch_vector(first_row)?;
                self.update_internal(
                    transaction,
                    update,
                    row_ids,
                    &base,
                    row_group_start,
                    &mut registrations,
                )?;
                if let ColumnKind::Standard { validity } = &self.kind {
                    let mut validity_registrations = Vec::new();
                    validity.update_internal(
                        transaction,
                        update,
                        row_ids,
                        &base,
                        row_group_start,
                        &mut validity_registrations,
                    )?;
                    registrations.extend(under_child(0, validity_registrations));
                }
            }
            ColumnKind::Struct { validity, fields } => {
                let base = self.fetch_vector(first_row)?;
                let mut validity_registrations = Vec::new();
                validity.update_internal(
                    transaction,
                    update,
                    row_ids,
                    &base,
                    row_group_start,
                    &mut validity_registrations,
                )?;
                registrations.extend(under_child(0, validity_registrations));
                let flat_update = update.flattened(row_ids.len());
                let children = flat_update
                    .struct_children()
                    .ok_or_else(|| Error::internal("struct update with a non-struct vector"))?;
                for (index, (field, child)) in fields.iter().zip(children).enumerate() {
                    let field_registrations = field.update(transaction, child, row_ids, row_group_start)?;
                    registrations.extend(under_child(index + 1, field_registrations));
                }
            }
            ColumnKind::List { .. } => return Err(Error::not_implemented("List Update is not supported.")),
            ColumnKind::Array { .. } => return Err(not_implemented_nested("UPDATE", &self.ty)),
        }
        Ok(registrations)
    }

    fn update_internal(
        &self,
        transaction: TransactionData,
        update: &Vector,
        row_ids: &[i64],
        base: &Vector,
        row_group_start: u64,
        registrations: &mut Vec<UpdateRegistration>,
    ) -> Result<()> {
        let segment = {
            let mut updates = self.updates();
            Arc::clone(updates.get_or_insert_with(|| Arc::new(UpdateSegment::new(self.ty.clone()))))
        };
        let first = row_ids.first().map_or(0, |&id| id as u64 - row_group_start);
        let vector_index = (first / VECTOR_SIZE) as usize;
        if let Some(version) = segment.update(transaction, update, row_ids, base, row_group_start)? {
            registrations.push(UpdateRegistration {
                segment,
                vector_index,
                version,
                column_path: Vec::new(),
                row_group_start,
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data_table::TableIoManager;
    use rustdb_buffer::{BufferManager, BufferManagerConfig, InMemoryBlockManager};

    fn io() -> Arc<TableIoManager> {
        let buffer_manager = BufferManager::new(BufferManagerConfig::default()).unwrap();
        Arc::new(TableIoManager::new(
            buffer_manager,
            Arc::new(InMemoryBlockManager::default()),
        ))
    }

    fn scan_all(column: &ColumnData) -> Vec<Value> {
        let mut state = ColumnScanState::for_type(column.logical_type());
        column.initialize_scan(&mut state);
        let mut values = Vec::new();
        let mut vector_index = 0;
        loop {
            let count = column.vector_count(vector_index);
            if count == 0 {
                break;
            }
            let mut vector = Vector::flat(column.logical_type().clone(), count);
            column
                .scan(
                    TransactionData::latest_committed(),
                    vector_index,
                    &mut state,
                    &mut vector,
                    count,
                )
                .unwrap();
            values.extend((0..count).map(|row| vector.get_value(row)));
            vector_index += 1;
        }
        values
    }

    fn append_values(column: &ColumnData, values: &[Value]) {
        let mut state = column.initialize_append().unwrap();
        for chunk in values.chunks(STANDARD_VECTOR_SIZE) {
            let mut vector = Vector::flat(column.logical_type().clone(), chunk.len());
            for (row, value) in chunk.iter().enumerate() {
                vector.set_value(row, value).unwrap();
            }
            column.append(&mut state, &vector, chunk.len()).unwrap();
        }
    }

    #[test]
    fn integers_with_nulls_round_trip_across_segments() {
        let column = ColumnData::new(io(), 0, LogicalType::BIGINT, ColumnDataType::MainTable);
        let values: Vec<Value> = (0..70_000)
            .map(|i| {
                if i % 7 == 0 {
                    Value::null(LogicalType::BIGINT)
                } else {
                    Value::bigint(i)
                }
            })
            .collect();
        append_values(&column, &values);
        assert!(column.segments().len() > 1, "a 256 KiB block holds 32K bigints");
        assert_eq!(scan_all(&column), values);
        let stats = column.statistics().unwrap();
        assert_eq!(stats.numeric_min(), Some(Value::bigint(1)));
        assert_eq!(stats.numeric_max(), Some(Value::bigint(69_999)));
        assert!(stats.can_have_null());
    }

    #[test]
    fn strings_including_big_ones_round_trip() {
        let column = ColumnData::new(io(), 0, LogicalType::VARCHAR, ColumnDataType::MainTable);
        let big = "x".repeat(10_000);
        let values: Vec<Value> = (0..5000)
            .map(|i| match i % 5 {
                0 => Value::null(LogicalType::VARCHAR),
                1 => Value::varchar(&big),
                _ => Value::varchar(format!("string number {i}")),
            })
            .collect();
        append_values(&column, &values);
        assert_eq!(scan_all(&column), values);
    }

    #[test]
    fn nested_types_round_trip() {
        let ty = LogicalType::struct_type(vec![
            ("a".into(), LogicalType::list(LogicalType::INTEGER)),
            ("b".into(), LogicalType::array(LogicalType::VARCHAR, 2)),
        ]);
        let column = ColumnData::new(io(), 0, ty.clone(), ColumnDataType::MainTable);
        let values: Vec<Value> = (0..3000)
            .map(|i| {
                if i % 11 == 0 {
                    return Value::null(ty.clone());
                }
                let list = Value::list(
                    LogicalType::INTEGER,
                    (0..i % 4).map(|j| Value::integer(i * 10 + j)).collect(),
                );
                let array = Value::array(
                    LogicalType::VARCHAR,
                    vec![Value::varchar(format!("v{i}")), Value::null(LogicalType::VARCHAR)],
                );
                Value::struct_value(vec![("a".into(), list), ("b".into(), array)])
            })
            .collect();
        append_values(&column, &values);
        assert_eq!(scan_all(&column), values);
        // reverting truncates every child consistently
        column.revert_append(100).unwrap();
        assert_eq!(scan_all(&column), values[..100]);
    }
}
