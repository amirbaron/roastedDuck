//! Comparing input rows with stored rows (DuckDB's `RowMatcher`), e.g. for hash table
//! probes: does key `i` of the input equal the key of the row it hashed to?

use std::cmp::Ordering;

use rustdb_common::{Error, Result};
use rustdb_types::compare::SqlOrd;
use rustdb_types::{LogicalType, PhysicalType, StringT, Vector};

use crate::compare::{Interval, compare_rows};
use crate::format::{ChunkFormat, SlotMap, VectorFormat};
use crate::gather::gather_column;
use crate::layout::TupleDataLayout;
use crate::row::{RowPtr, is_valid, load};

/// The comparison applied to one column (DuckDB's comparison `ExpressionType`s).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatchPredicate {
    Equal,
    NotEqual,
    DistinctFrom,
    NotDistinctFrom,
    GreaterThan,
    GreaterThanOrEqual,
    LessThan,
    LessThanOrEqual,
}

impl MatchPredicate {
    /// Whether NULLs take part in the comparison (`COMPARE_NULL`).
    fn compares_null(self) -> bool {
        matches!(self, Self::DistinctFrom | Self::NotDistinctFrom)
    }

    /// Applies the predicate to flat values compared with `ordering`, with DuckDB's
    /// `ComparisonOperationWrapper` NULL handling.
    #[inline]
    fn evaluate(self, ordering: impl FnOnce() -> Ordering, lhs_null: bool, rhs_null: bool) -> bool {
        if lhs_null || rhs_null {
            return match self {
                Self::DistinctFrom => lhs_null != rhs_null,
                Self::NotDistinctFrom => lhs_null == rhs_null,
                _ => false,
            };
        }
        let ordering = ordering();
        match self {
            Self::Equal | Self::NotDistinctFrom => ordering == Ordering::Equal,
            Self::NotEqual | Self::DistinctFrom => ordering != Ordering::Equal,
            Self::GreaterThan => ordering == Ordering::Greater,
            Self::GreaterThanOrEqual => ordering != Ordering::Less,
            Self::LessThan => ordering == Ordering::Less,
            Self::LessThanOrEqual => ordering != Ordering::Greater,
        }
    }

    /// Applies the predicate to nested values (`NestedEquals`, `DistinctGreaterThan`, ...):
    /// NULL is greater than every value for the ordering predicates.
    fn evaluate_nested(self, ordering_with_nulls_last: Ordering, lhs_null: bool, rhs_null: bool) -> bool {
        match self {
            Self::Equal => !lhs_null && !rhs_null && ordering_with_nulls_last == Ordering::Equal,
            Self::NotEqual => !lhs_null && !rhs_null && ordering_with_nulls_last != Ordering::Equal,
            Self::DistinctFrom => ordering_with_nulls_last != Ordering::Equal,
            Self::NotDistinctFrom => ordering_with_nulls_last == Ordering::Equal,
            Self::GreaterThan => ordering_with_nulls_last == Ordering::Greater,
            Self::GreaterThanOrEqual => ordering_with_nulls_last != Ordering::Less,
            Self::LessThan => ordering_with_nulls_last == Ordering::Less,
            Self::LessThanOrEqual => ordering_with_nulls_last != Ordering::Greater,
        }
    }
}

/// How one column is matched.
#[derive(Debug)]
enum MatchFunction {
    Fixed {
        physical: PhysicalType,
        predicate: MatchPredicate,
    },
    Varchar {
        predicate: MatchPredicate,
    },
    /// `=` / `IS NOT DISTINCT FROM` on structs: NULL handling here, fields recursively.
    StructEquality {
        predicate: MatchPredicate,
        children: Vec<MatchFunction>,
    },
    /// Gathers the stored values and compares them as nested values.
    Nested {
        ty: LogicalType,
        predicate: MatchPredicate,
    },
}

impl MatchFunction {
    fn new(ty: &LogicalType, predicate: MatchPredicate) -> Result<Self> {
        Ok(match ty.physical_type() {
            PhysicalType::Varchar => Self::Varchar { predicate },
            PhysicalType::Struct
                if matches!(predicate, MatchPredicate::Equal | MatchPredicate::NotDistinctFrom) =>
            {
                let Some(rustdb_types::TypeInfo::Struct(rustdb_types::StructTypeInfo {
                    child_types: children,
                })) = ty.info()
                else {
                    return Err(Error::internal("STRUCT type without children"));
                };
                Self::StructEquality {
                    predicate,
                    // Fields are equal if they are not distinct (NULL fields match).
                    children: children
                        .iter()
                        .map(|(_, child)| Self::new(child, MatchPredicate::NotDistinctFrom))
                        .collect::<Result<_>>()?,
                }
            }
            PhysicalType::Struct | PhysicalType::List | PhysicalType::Array => Self::Nested {
                ty: ty.clone(),
                predicate,
            },
            PhysicalType::Bit | PhysicalType::Unknown | PhysicalType::Invalid => {
                return Err(Error::internal(format!(
                    "Unsupported PhysicalType for RowMatcher::GetMatchFunction: {}",
                    ty.physical_type().name()
                )));
            }
            physical => Self::Fixed { physical, predicate },
        })
    }
}

/// Matches input rows against stored rows column by column (DuckDB's `RowMatcher`).
#[derive(Debug)]
pub struct RowMatcher {
    columns: Vec<usize>,
    functions: Vec<MatchFunction>,
}

/// Values that can be loaded from rows and compared with SQL semantics.
trait MatchValue: Copy {
    /// # Safety
    /// `ptr` must be valid for reading the value.
    unsafe fn load(ptr: *const u8) -> Self;
    fn compare(self, other: Self) -> Ordering;
}

macro_rules! match_value {
    ($($ty:ty),*) => {$(
        impl MatchValue for $ty {
            #[inline]
            unsafe fn load(ptr: *const u8) -> Self {
                // SAFETY: guaranteed by the caller.
                unsafe { load::<$ty>(ptr) }
            }
            #[inline]
            fn compare(self, other: Self) -> Ordering {
                if self.sql_eq(other) {
                    Ordering::Equal
                } else if self.sql_gt(other) {
                    Ordering::Greater
                } else {
                    Ordering::Less
                }
            }
        }
    )*};
}
match_value!(i8, i16, i32, i64, i128, u8, u16, u32, u64, u128, f32, f64);

impl MatchValue for bool {
    #[inline]
    unsafe fn load(ptr: *const u8) -> Self {
        // SAFETY: guaranteed by the caller; booleans are stored as 0 or 1.
        unsafe { *ptr != 0 }
    }
    #[inline]
    fn compare(self, other: Self) -> Ordering {
        self.cmp(&other)
    }
}

impl MatchValue for Interval {
    #[inline]
    unsafe fn load(ptr: *const u8) -> Self {
        // SAFETY: guaranteed by the caller.
        Interval::from_bytes(unsafe { std::slice::from_raw_parts(ptr, 16) })
    }
    #[inline]
    fn compare(self, other: Self) -> Ordering {
        if self == other {
            return Ordering::Equal;
        }
        self.normalize().cmp(&other.normalize())
    }
}

/// Removes the non-matching entries from `sel[..count]` (keeping the order) and appends
/// them to `no_match`; returns the number of matches.
#[allow(
    clippy::inline_always,
    reason = "the per-row predicate must be inlined into the loop"
)]
#[inline(always)]
fn compact(
    sel: &mut [u32],
    count: usize,
    no_match: &mut Option<&mut Vec<u32>>,
    mut matches: impl FnMut(usize) -> bool,
) -> usize {
    let mut match_count = 0;
    let Some(no_match) = no_match.as_deref_mut() else {
        for i in 0..count {
            let idx = sel[i];
            if matches(idx as usize) {
                sel[match_count] = idx;
                match_count += 1;
            }
        }
        return match_count;
    };
    // Written by index like DuckDB's `no_match_sel`, into reserved space.
    no_match.reserve(count);
    let start = no_match.len();
    let spare = &mut no_match.spare_capacity_mut()[..count];
    let mut no_match_count = 0;
    for i in 0..count {
        let idx = sel[i];
        if matches(idx as usize) {
            sel[match_count] = idx;
            match_count += 1;
        } else {
            spare[no_match_count].write(idx);
            no_match_count += 1;
        }
    }
    // SAFETY: the first `no_match_count` reserved slots were written above.
    unsafe { no_match.set_len(start + no_match_count) };
    match_count
}

impl RowMatcher {
    /// A matcher comparing `columns` (default: the first `predicates.len()` columns) of
    /// input chunks with the same columns of rows of `layout`.
    pub fn new(
        layout: &TupleDataLayout,
        predicates: &[MatchPredicate],
        columns: Option<Vec<usize>>,
    ) -> Result<Self> {
        let columns = columns.unwrap_or_else(|| (0..predicates.len()).collect());
        if columns.len() != predicates.len() || columns.iter().any(|&col| col >= layout.column_count()) {
            return Err(Error::internal(
                "RowMatcher columns do not match the predicates or layout",
            ));
        }
        let functions = columns
            .iter()
            .zip(predicates)
            .map(|(&col, &predicate)| MatchFunction::new(&layout.types()[col], predicate))
            .collect::<Result<_>>()?;
        Ok(Self { columns, functions })
    }

    /// Keeps in `sel[..count]` the input rows whose columns satisfy the predicates against
    /// `rhs_rows[idx]` (the stored row for input row `idx`), returning how many remain.
    /// Rejected rows are appended to `no_match` (`RowMatcher::Match`).
    ///
    /// # Safety
    /// For every `idx` in `sel[..count]`, `rhs_rows[idx]` must be a row of `layout` in a
    /// pinned block (with its heap pinned too).
    pub unsafe fn match_rows(
        &self,
        layout: &TupleDataLayout,
        lhs: &ChunkFormat<'_>,
        sel: &mut [u32],
        mut count: usize,
        rhs_rows: &[RowPtr],
        mut no_match: Option<&mut Vec<u32>>,
    ) -> Result<usize> {
        for (&col, function) in self.columns.iter().zip(&self.functions) {
            // SAFETY: forwarded from the caller.
            count = unsafe {
                match_column(
                    function,
                    layout,
                    col,
                    lhs.column(col),
                    sel,
                    count,
                    rhs_rows,
                    0,
                    &mut no_match,
                )?
            };
        }
        Ok(count)
    }
}

/// # Safety
/// As for [`RowMatcher::match_rows`]; `base` is the offset of a struct's nested layout.
#[allow(
    clippy::too_many_arguments,
    reason = "mirrors DuckDB's match function signature"
)]
unsafe fn match_column(
    function: &MatchFunction,
    layout: &TupleDataLayout,
    col: usize,
    lhs: &VectorFormat<'_>,
    sel: &mut [u32],
    count: usize,
    rhs_rows: &[RowPtr],
    base: usize,
    no_match: &mut Option<&mut Vec<u32>>,
) -> Result<usize> {
    let offset = base + layout.offsets()[col];
    let all_valid = layout.all_valid();
    let rhs_null = |idx: usize| -> bool {
        // SAFETY: the rows are valid rows of the layout (caller contract).
        !all_valid && unsafe { !is_valid(rhs_rows[idx].as_ptr().add(base), col) }
    };
    // SAFETY (whole function): rows are pinned rows of the layout.
    unsafe {
        match function {
            MatchFunction::Fixed { physical, predicate } => Ok(match_fixed(
                *physical, *predicate, lhs, sel, count, rhs_rows, offset, &rhs_null, all_valid, no_match,
            )),
            MatchFunction::Varchar { predicate } => {
                let equality = matches!(
                    predicate,
                    MatchPredicate::Equal
                        | MatchPredicate::NotEqual
                        | MatchPredicate::DistinctFrom
                        | MatchPredicate::NotDistinctFrom
                );
                Ok(compact(sel, count, no_match, |idx| {
                    let slot = lhs.slot(idx);
                    predicate.evaluate(
                        || {
                            let location = rhs_rows[idx].as_ptr().add(offset);
                            let rhs_string = load::<StringT>(location);
                            let lhs_string = lhs.string(slot);
                            // Equality is mostly decided by length, prefix and inlined bytes.
                            let quick = if equality {
                                lhs_string.quick_eq(&rhs_string)
                            } else {
                                None
                            };
                            if let Some(equal) = quick {
                                return if equal { Ordering::Equal } else { Ordering::Less };
                            }
                            let rhs_bytes = if rhs_string.is_inlined() {
                                std::slice::from_raw_parts(location.add(4), rhs_string.len())
                            } else {
                                let pointer = load::<u64>(location.add(8)) as usize as *const u8;
                                std::slice::from_raw_parts(pointer, rhs_string.len())
                            };
                            let lhs_bytes = lhs.string_bytes(slot);
                            if equality {
                                return if lhs_bytes == rhs_bytes {
                                    Ordering::Equal
                                } else {
                                    Ordering::Less
                                };
                            }
                            lhs_bytes.cmp(rhs_bytes)
                        },
                        !lhs.slot_is_valid(slot),
                        rhs_null(idx),
                    )
                }))
            }
            MatchFunction::StructEquality { predicate, children } => {
                let mut match_count = compact(sel, count, no_match, |idx| {
                    let lhs_null = !lhs.row_is_valid(idx);
                    let rhs_is_null = rhs_null(idx);
                    // Without NULLs the fields decide; with NULLs only IS NOT DISTINCT FROM
                    // can match (both NULL).
                    !(lhs_null || rhs_is_null) || (predicate.compares_null() && lhs_null && rhs_is_null)
                });
                let Some(struct_layout) = layout.struct_layout(col) else {
                    return Err(Error::internal("struct column without struct layout"));
                };
                let struct_rows: Vec<RowPtr> = rhs_rows.iter().map(|row| row.offset(offset)).collect();
                for (child_col, (child, child_lhs)) in children.iter().zip(&lhs.children).enumerate() {
                    match_count = match_column(
                        child,
                        struct_layout,
                        child_col,
                        child_lhs,
                        sel,
                        match_count,
                        &struct_rows,
                        0,
                        no_match,
                    )?;
                }
                Ok(match_count)
            }
            MatchFunction::Nested { ty, predicate } => {
                // Gather the stored values of the candidates into a dense vector.
                let mut gathered = Vector::flat(ty.clone(), count.max(1));
                gather_column(
                    layout,
                    col,
                    rhs_rows,
                    Some(&sel[..count]),
                    count,
                    base,
                    &mut gathered,
                    None,
                )?;
                let rhs = VectorFormat::new(&gathered, count)?;
                let mut position = 0;
                Ok(compact(sel, count, no_match, |idx| {
                    let i = position;
                    position += 1;
                    let lhs_null = !lhs.row_is_valid(idx);
                    let rhs_is_null = !rhs.row_is_valid(i);
                    predicate.evaluate_nested(compare_rows(lhs, idx, &rhs, i), lhs_null, rhs_is_null)
                }))
            }
        }
    }
}

/// # Safety
/// As for [`match_column`].
#[allow(clippy::too_many_arguments, reason = "hot loop kept free of indirection")]
unsafe fn match_fixed(
    physical: PhysicalType,
    predicate: MatchPredicate,
    lhs: &VectorFormat<'_>,
    sel: &mut [u32],
    count: usize,
    rhs_rows: &[RowPtr],
    offset: usize,
    rhs_null: &impl Fn(usize) -> bool,
    rhs_all_valid: bool,
    no_match: &mut Option<&mut Vec<u32>>,
) -> usize {
    #[allow(
        clippy::inline_always,
        reason = "monomorphized hot loop; inlining removes the per-width call"
    )]
    #[inline(always)]
    unsafe fn run<T: MatchValue>(
        predicate: MatchPredicate,
        lhs: &VectorFormat<'_>,
        sel: &mut [u32],
        count: usize,
        rhs_rows: &[RowPtr],
        offset: usize,
        rhs_null: &impl Fn(usize) -> bool,
        rhs_all_valid: bool,
        no_match: &mut Option<&mut Vec<u32>>,
    ) -> usize {
        let width = size_of::<T>().max(1);
        let lhs_all_valid = lhs.all_valid();
        // SAFETY: rows are pinned rows; slots come from the vector's selection.
        unsafe {
            // With no NULL input value, `IS NOT DISTINCT FROM` is `=` (a NULL stored value
            // differs from every input value).
            if matches!(predicate, MatchPredicate::Equal | MatchPredicate::NotDistinctFrom) && lhs_all_valid {
                // The hot path of hash table probes and aggregate lookups: the input's
                // selection and the stored rows' validity are resolved outside the loop.
                #[allow(
                    clippy::inline_always,
                    reason = "called from three loops; each must inline it"
                )]
                #[inline(always)]
                unsafe fn equal<T: MatchValue>(lhs: *const u8, rhs: *const u8) -> bool {
                    // SAFETY: both point to a `T` (caller contract).
                    unsafe { T::load(lhs).compare(T::load(rhs)) == Ordering::Equal }
                }
                let rhs = |idx: usize| rhs_rows[idx].as_ptr().add(offset);
                return match (&lhs.sel, rhs_all_valid) {
                    (SlotMap::Identity, true) => compact(sel, count, no_match, |idx| {
                        equal::<T>(lhs.slot_ptr(idx, width), rhs(idx))
                    }),
                    (SlotMap::Indices(indices), true) => compact(sel, count, no_match, |idx| {
                        equal::<T>(lhs.slot_ptr(indices[idx] as usize, width), rhs(idx))
                    }),
                    _ => compact(sel, count, no_match, |idx| {
                        !rhs_null(idx) && equal::<T>(lhs.slot_ptr(lhs.slot(idx), width), rhs(idx))
                    }),
                };
            }
            compact(sel, count, no_match, |idx| {
                let slot = lhs.slot(idx);
                let lhs_null = !lhs_all_valid && !lhs.slot_is_valid(slot);
                predicate.evaluate(
                    || {
                        T::load(lhs.slot_ptr(slot, width))
                            .compare(T::load(rhs_rows[idx].as_ptr().add(offset)))
                    },
                    lhs_null,
                    rhs_null(idx),
                )
            })
        }
    }
    macro_rules! go {
        ($ty:ty) => {
            // SAFETY: forwarded from the caller.
            unsafe {
                run::<$ty>(
                    predicate,
                    lhs,
                    sel,
                    count,
                    rhs_rows,
                    offset,
                    rhs_null,
                    rhs_all_valid,
                    no_match,
                )
            }
        };
    }
    match physical {
        PhysicalType::Bool => go!(bool),
        PhysicalType::Int8 => go!(i8),
        PhysicalType::Int16 => go!(i16),
        PhysicalType::Int32 => go!(i32),
        PhysicalType::Int64 => go!(i64),
        PhysicalType::Int128 => go!(i128),
        PhysicalType::UInt8 => go!(u8),
        PhysicalType::UInt16 => go!(u16),
        PhysicalType::UInt32 => go!(u32),
        PhysicalType::UInt64 => go!(u64),
        PhysicalType::UInt128 => go!(u128),
        PhysicalType::Float => go!(f32),
        PhysicalType::Double => go!(f64),
        _ => go!(Interval),
    }
}
