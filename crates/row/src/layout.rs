//! The layout of a row (DuckDB's `TupleDataLayout`).
//!
//! A row is `[validity bits][heap size][column data...][aggregate states...]`:
//! - one validity bit per column (omitted when no column can be NULL),
//! - the total size of the row's variable-size data in the heap (only if there is any),
//! - each column at a fixed offset: fixed-width values inline, strings as a 16-byte
//!   `string_t` (inline, or prefix + pointer into the heap), lists/arrays as a pointer into
//!   the heap, structs as a nested row layout,
//! - aggregate states, aligned for the aggregate functions.
//!
//! Columns are unaligned (rows are packed). Sort keys use a separate, fixed-width layout
//! (see [`SortKeyType`]).
//!
//! The heap size is stored as `u32` (DuckDB uses 64 bits), saving four bytes per row; a
//! single row with more than 4 GiB of variable-size data is rejected.

use std::alloc::Layout;
use std::sync::Arc;

use rustdb_common::{Error, Result};
use rustdb_function::{AggregateFunction, FunctionData};
use rustdb_types::{LogicalType, PhysicalType, TypeInfo};

use crate::sort::key::SortKeyType;

/// Whether columns of a layout can hold NULL (DuckDB's `TupleDataValidityType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValidityType {
    CanHaveNull,
    /// No validity bits are stored (e.g. hash table keys that are known non-NULL).
    CannotHaveNull,
}

/// Width of the per-row heap size field.
pub(crate) const HEAP_SIZE_WIDTH: usize = size_of::<u32>();

/// Width of a heap pointer stored in a row (list/array columns).
pub(crate) const POINTER_WIDTH: usize = size_of::<u64>();

/// An aggregate whose state lives in each row (DuckDB's `AggregateObject`).
#[derive(Clone)]
pub struct RowAggregate {
    pub function: AggregateFunction,
    pub bind_data: Option<Arc<dyn FunctionData>>,
    /// Number of input columns the aggregate consumes.
    pub child_count: usize,
    /// Bytes reserved for the state (its size rounded up to its alignment).
    pub payload_size: usize,
    /// Alignment of the state.
    pub alignment: usize,
}

impl std::fmt::Debug for RowAggregate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RowAggregate")
            .field("function", &self.function.name)
            .field("child_count", &self.child_count)
            .field("payload_size", &self.payload_size)
            .finish_non_exhaustive()
    }
}

impl RowAggregate {
    pub fn new(
        function: AggregateFunction,
        bind_data: Option<Arc<dyn FunctionData>>,
        child_count: usize,
    ) -> Self {
        let state: Layout = function.ops.state_layout();
        // DuckDB aligns every state to 8 bytes; larger alignments are honoured as well.
        let alignment = state.align().max(8);
        Self {
            function,
            bind_data,
            child_count,
            payload_size: state.size().next_multiple_of(alignment),
            alignment,
        }
    }
}

/// What determines a sort key's width (one ORDER BY column).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SortKeyColumn {
    pub physical_type: PhysicalType,
    /// Whether the column is a VARCHAR (whose maximum length statistics bound the key).
    pub is_varchar: bool,
    /// From statistics: `Some(false)` if the column has no NULLs.
    pub can_have_null: Option<bool>,
    /// From statistics: the longest string, for VARCHAR columns.
    pub max_string_length: Option<u32>,
}

/// The layout of rows of a [`crate::TupleDataCollection`]; see the module documentation.
#[derive(Clone, Debug)]
pub struct TupleDataLayout {
    types: Vec<LogicalType>,
    aggregates: Vec<RowAggregate>,
    sort_key_type: Option<SortKeyType>,
    /// Nested layouts of STRUCT columns.
    struct_layouts: Vec<Option<TupleDataLayout>>,
    flag_width: usize,
    data_width: usize,
    aggr_width: usize,
    sort_width: Option<usize>,
    sort_skippable_bytes: Vec<usize>,
    row_width: usize,
    offsets: Vec<usize>,
    all_constant: bool,
    variable_columns: Vec<usize>,
    heap_size_offset: usize,
    aggr_destructor_idxs: Vec<usize>,
    validity_type: ValidityType,
    alignment: usize,
}

fn is_constant_size(physical: PhysicalType) -> bool {
    !matches!(
        physical,
        PhysicalType::Varchar | PhysicalType::List | PhysicalType::Array | PhysicalType::Struct
    ) && physical.size() > 0
}

/// Bytes needed for `count` validity bits.
pub(crate) const fn validity_bytes(count: usize) -> usize {
    count.div_ceil(8)
}

impl TupleDataLayout {
    fn empty(types: Vec<LogicalType>, validity_type: ValidityType) -> Self {
        Self {
            types,
            aggregates: Vec::new(),
            sort_key_type: None,
            struct_layouts: Vec::new(),
            flag_width: 0,
            data_width: 0,
            aggr_width: 0,
            sort_width: None,
            sort_skippable_bytes: Vec::new(),
            row_width: 0,
            offsets: Vec::new(),
            all_constant: true,
            variable_columns: Vec::new(),
            heap_size_offset: 0,
            aggr_destructor_idxs: Vec::new(),
            validity_type,
            alignment: 1,
        }
    }

    /// A layout for `types` (DuckDB's `Initialize(types, validity_type)`).
    pub fn new(types: Vec<LogicalType>, validity_type: ValidityType) -> Result<Self> {
        Self::with_aggregates(types, Vec::new(), validity_type)
    }

    /// A layout for `types` followed by the states of `aggregates`.
    pub fn with_aggregates(
        types: Vec<LogicalType>,
        aggregates: Vec<RowAggregate>,
        validity_type: ValidityType,
    ) -> Result<Self> {
        Self::build(types, aggregates, validity_type, true)
    }

    fn build(
        types: Vec<LogicalType>,
        aggregates: Vec<RowAggregate>,
        validity_type: ValidityType,
        top_level: bool,
    ) -> Result<Self> {
        let mut layout = Self::empty(types, validity_type);
        layout.flag_width = if validity_type == ValidityType::CannotHaveNull {
            0
        } else {
            validity_bytes(layout.types.len())
        };
        layout.row_width = layout.flag_width;
        layout.struct_layouts = vec![None; layout.types.len()];

        for (col, ty) in layout.types.iter().enumerate() {
            match ty.physical_type() {
                PhysicalType::Struct => {
                    let Some(TypeInfo::Struct(rustdb_types::StructTypeInfo {
                        child_types: children,
                    })) = ty.info()
                    else {
                        return Err(Error::internal(format!("STRUCT type without children: {ty}")));
                    };
                    let child_types = children.iter().map(|(_, child)| child.clone()).collect();
                    let child_layout =
                        Self::build(child_types, Vec::new(), ValidityType::CanHaveNull, false)?;
                    if !child_layout.all_constant {
                        layout.all_constant = false;
                        layout.variable_columns.push(col);
                    }
                    layout.struct_layouts[col] = Some(child_layout);
                }
                PhysicalType::Varchar | PhysicalType::List | PhysicalType::Array => {
                    layout.all_constant = false;
                    layout.variable_columns.push(col);
                }
                physical if is_constant_size(physical) => {}
                _ => {
                    return Err(Error::not_implemented(format!(
                        "Unsupported type for TupleDataLayout: {ty}"
                    )));
                }
            }
        }

        // The heap size lets out-of-core code recompute heap pointers.
        if top_level && !layout.all_constant {
            layout.heap_size_offset = layout.row_width;
            layout.row_width += HEAP_SIZE_WIDTH;
        }

        for col in 0..layout.types.len() {
            layout.offsets.push(layout.row_width);
            let physical = layout.types[col].physical_type();
            layout.row_width += match physical {
                PhysicalType::Struct => layout.struct_layouts[col]
                    .as_ref()
                    .map_or(0, |child| child.row_width),
                PhysicalType::List | PhysicalType::Array => POINTER_WIDTH,
                other => other.size(),
            };
        }

        let alignment = aggregates.iter().map(|aggr| aggr.alignment).max().unwrap_or(1);
        if !aggregates.is_empty() {
            layout.row_width = layout.row_width.next_multiple_of(alignment);
        }
        layout.data_width = layout.row_width - layout.flag_width;
        for aggregate in &aggregates {
            // Rust states may need more than DuckDB's 8-byte alignment (`i128` needs 16),
            // so each state starts at a multiple of its own alignment.
            layout.row_width = layout.row_width.next_multiple_of(aggregate.alignment);
            layout.offsets.push(layout.row_width);
            layout.row_width += aggregate.payload_size;
        }
        layout.aggr_width = layout.row_width - layout.data_width - layout.flag_width;
        if !aggregates.is_empty() {
            layout.row_width = layout.row_width.next_multiple_of(alignment);
        }
        layout.alignment = alignment;
        layout.aggr_destructor_idxs = aggregates
            .iter()
            .enumerate()
            .filter(|(_, aggr)| aggr.function.ops.needs_destructor())
            .map(|(idx, _)| idx)
            .collect();
        layout.aggregates = aggregates;
        Ok(layout)
    }

    /// A layout holding only aggregate states (ungrouped aggregation).
    pub fn aggregates_only(aggregates: Vec<RowAggregate>) -> Result<Self> {
        Self::with_aggregates(Vec::new(), aggregates, ValidityType::CannotHaveNull)
    }

    /// The layout of sort keys of type `key_type` (BIGINT or BLOB, as chosen by
    /// [`crate::sort::sort_key_type_for`]) for `orders` (DuckDB's `Initialize(orders, type,
    /// has_payload)`). With a payload, every key also stores a pointer to its payload row.
    pub fn sort_key(orders: &[SortKeyColumn], key_type: &LogicalType, has_payload: bool) -> Self {
        let mut layout = Self::empty(vec![key_type.clone()], ValidityType::CanHaveNull);
        let mut sort_width = Some(0usize);
        let mut all_valid_and_constant = true;
        for order in orders {
            let width = sort_width.unwrap_or(0);
            let constant = is_constant_size(order.physical_type);
            if all_valid_and_constant && order.can_have_null == Some(false) && constant && width < 7 {
                // Every value is valid: the validity byte never decides an order.
                layout.sort_skippable_bytes.push(width);
            } else {
                all_valid_and_constant = false;
            }
            if constant {
                sort_width = Some(width + 1 + order.physical_type.size());
            } else if let (true, Some(max_len)) = (order.is_varchar, order.max_string_length) {
                sort_width = Some(width + 1 + max_len as usize + 1);
            } else {
                sort_width = None;
                break;
            }
        }
        layout.sort_width = sort_width;
        let mut temp_row_width = if key_type.id() == rustdb_types::LogicalTypeId::BigInt {
            Some(8)
        } else {
            sort_width
        };
        if let (Some(width), true) = (temp_row_width, has_payload) {
            temp_row_width = Some(width + 8);
        }
        let (row_width, key) = match temp_row_width {
            Some(width) if width <= 8 => (8, SortKeyType::NoPayloadFixed8),
            Some(width) if width <= 16 => (
                16,
                if has_payload {
                    SortKeyType::PayloadFixed16
                } else {
                    SortKeyType::NoPayloadFixed16
                },
            ),
            Some(width) if width <= 24 => (
                24,
                if has_payload {
                    SortKeyType::PayloadFixed24
                } else {
                    SortKeyType::NoPayloadFixed24
                },
            ),
            Some(width) if width <= 32 => (
                32,
                if has_payload {
                    SortKeyType::PayloadFixed32
                } else {
                    SortKeyType::NoPayloadFixed32
                },
            ),
            _ => (
                32,
                if has_payload {
                    SortKeyType::PayloadVariable32
                } else {
                    SortKeyType::NoPayloadVariable32
                },
            ),
        };
        layout.row_width = row_width;
        layout.sort_key_type = Some(key);
        if !key.is_constant_size() {
            layout.all_constant = false;
            layout.heap_size_offset = key.heap_size_offset();
        }
        layout.offsets.push(0);
        layout
    }

    /// The data column types.
    pub fn types(&self) -> &[LogicalType] {
        &self.types
    }

    pub fn column_count(&self) -> usize {
        self.types.len()
    }

    pub fn aggregates(&self) -> &[RowAggregate] {
        &self.aggregates
    }

    pub fn aggregate_count(&self) -> usize {
        self.aggregates.len()
    }

    /// The sort key type, for sort key layouts.
    pub fn sort_key_type(&self) -> Option<SortKeyType> {
        self.sort_key_type
    }

    pub fn is_sort_key_layout(&self) -> bool {
        self.sort_key_type.is_some()
    }

    /// The nested layout of STRUCT column `col`.
    pub fn struct_layout(&self, col: usize) -> Option<&TupleDataLayout> {
        self.struct_layouts.get(col).and_then(Option::as_ref)
    }

    /// Bytes per row, including padding.
    pub fn row_width(&self) -> usize {
        self.row_width
    }

    /// Offset of the first data column.
    pub fn data_offset(&self) -> usize {
        self.flag_width
    }

    pub fn data_width(&self) -> usize {
        self.data_width
    }

    /// Offset of the first aggregate state.
    pub fn aggr_offset(&self) -> usize {
        self.flag_width + self.data_width
    }

    pub fn aggr_width(&self) -> usize {
        self.aggr_width
    }

    /// Byte width of the normalized key (`None` if unbounded); sort key layouts only.
    pub fn sort_width(&self) -> Option<usize> {
        self.sort_width
    }

    /// Key bytes that never influence the order (always-valid validity bytes).
    pub fn sort_skippable_bytes(&self) -> &[usize] {
        &self.sort_skippable_bytes
    }

    /// Offset of each column, then of each aggregate state.
    pub fn offsets(&self) -> &[usize] {
        &self.offsets
    }

    /// Whether every column has a fixed size (no heap).
    pub fn all_constant(&self) -> bool {
        self.all_constant
    }

    /// Columns with variable-size data.
    pub fn variable_columns(&self) -> &[usize] {
        &self.variable_columns
    }

    /// Offset of the heap size field.
    pub fn heap_size_offset(&self) -> usize {
        self.heap_size_offset
    }

    pub fn has_destructor(&self) -> bool {
        !self.aggr_destructor_idxs.is_empty()
    }

    pub fn aggregate_destructor_indices(&self) -> &[usize] {
        &self.aggr_destructor_idxs
    }

    /// Whether no column can be NULL (no validity bits stored).
    pub fn all_valid(&self) -> bool {
        self.validity_type == ValidityType::CannotHaveNull
    }

    /// Alignment rows must have (aggregate states).
    pub fn alignment(&self) -> usize {
        self.alignment
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustdb_types::LogicalTypeId;

    fn struct_of(children: &[(&str, LogicalType)]) -> LogicalType {
        LogicalType::with_info(
            LogicalTypeId::Struct,
            TypeInfo::Struct(rustdb_types::StructTypeInfo {
                child_types: children
                    .iter()
                    .map(|(n, t)| ((*n).to_owned(), t.clone()))
                    .collect(),
            }),
        )
    }

    #[test]
    fn fixed_columns_are_packed_after_validity() {
        let layout = TupleDataLayout::new(
            vec![LogicalType::INTEGER, LogicalType::BIGINT, LogicalType::BOOLEAN],
            ValidityType::CanHaveNull,
        )
        .unwrap();
        assert_eq!(layout.offsets(), &[1, 5, 13]);
        assert_eq!(layout.row_width(), 14);
        assert!(layout.all_constant());
        let no_nulls =
            TupleDataLayout::new(vec![LogicalType::INTEGER], ValidityType::CannotHaveNull).unwrap();
        assert_eq!(no_nulls.row_width(), 4);
    }

    #[test]
    fn variable_columns_get_a_heap_size() {
        let layout = TupleDataLayout::new(
            vec![
                LogicalType::VARCHAR,
                LogicalType::list(LogicalType::INTEGER),
                struct_of(&[("a", LogicalType::INTEGER), ("b", LogicalType::VARCHAR)]),
            ],
            ValidityType::CanHaveNull,
        )
        .unwrap();
        assert!(!layout.all_constant());
        assert_eq!(layout.heap_size_offset(), 1);
        // validity(1) + heap size(4), then string(16), list pointer(8), struct(1 + 4 + 16)
        assert_eq!(layout.offsets(), &[5, 21, 29]);
        assert_eq!(layout.row_width(), 50);
        assert_eq!(layout.variable_columns(), &[0, 1, 2]);
        let child = layout.struct_layout(2).unwrap();
        assert_eq!(child.offsets(), &[1, 5]);
    }

    #[test]
    fn sort_key_layouts_follow_duckdb() {
        let int = SortKeyColumn {
            physical_type: PhysicalType::Int32,
            is_varchar: false,
            can_have_null: None,
            max_string_length: None,
        };
        let layout = TupleDataLayout::sort_key(std::slice::from_ref(&int), &LogicalType::BIGINT, false);
        assert_eq!(layout.sort_key_type(), Some(SortKeyType::NoPayloadFixed8));
        assert_eq!(layout.row_width(), 8);
        let layout = TupleDataLayout::sort_key(std::slice::from_ref(&int), &LogicalType::BIGINT, true);
        assert_eq!(layout.sort_key_type(), Some(SortKeyType::PayloadFixed16));
        let two = [int.clone(), int.clone(), int.clone(), int];
        let layout = TupleDataLayout::sort_key(&two, &LogicalType::BLOB, true);
        assert_eq!(layout.sort_width(), Some(20));
        assert_eq!(layout.sort_key_type(), Some(SortKeyType::PayloadFixed32));
        let string = SortKeyColumn {
            physical_type: PhysicalType::Varchar,
            is_varchar: true,
            can_have_null: Some(true),
            max_string_length: None,
        };
        let layout = TupleDataLayout::sort_key(&[string], &LogicalType::BLOB, false);
        assert_eq!(layout.sort_key_type(), Some(SortKeyType::NoPayloadVariable32));
        assert!(!layout.all_constant());
        assert_eq!(layout.heap_size_offset(), 16);
    }
}
