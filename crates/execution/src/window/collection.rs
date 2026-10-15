//! The fully materialised argument columns of a hash group (DuckDB's `WindowCollection`).
//!
//! Window functions read arbitrary rows of their arguments (frames, offsets), so the
//! shared "collection" expressions are computed for every row of the hash group and kept
//! as one flat vector per column, indexed by the row's position in the sorted group.

use rustdb_common::{Error, Result};
use rustdb_types::{DataChunk, LogicalType, NativeType, PhysicalType, StringT, Vector, VectorKind};

use rustdb_types::compare::SqlOrd;

use super::mask::WindowMask;

/// Copies rows `0..count` of `source` into rows `offset..offset + count` of the flat vector
/// `target` (DuckDB's `VectorOperations::Copy`).
pub(crate) fn copy_into(source: &Vector, count: usize, target: &mut Vector, offset: usize) -> Result<()> {
    fn typed<T: NativeType>(source: &Vector, count: usize, target: &mut Vector, offset: usize) {
        let unified = source.unified::<T>();
        let data = &mut target.values_mut::<T>()[offset..offset + count];
        for (row, value) in data.iter_mut().enumerate() {
            *value = unified.values[unified.slot(row)];
        }
        if !unified.validity.is_all_valid() {
            let nulls: Vec<usize> = (0..count).filter(|&row| !unified.is_valid(row)).collect();
            let capacity = target.capacity();
            let validity = target.validity_mut();
            for row in nulls {
                validity.set_invalid(offset + row, capacity);
            }
        }
    }
    if count == 0 {
        return Ok(());
    }
    if source.kind() == VectorKind::Sequence {
        return copy_into(&source.flattened(count), count, target, offset);
    }
    match source.logical_type().physical_type() {
        PhysicalType::Bool => typed::<bool>(source, count, target, offset),
        PhysicalType::Int8 => typed::<i8>(source, count, target, offset),
        PhysicalType::Int16 => typed::<i16>(source, count, target, offset),
        PhysicalType::Int32 => typed::<i32>(source, count, target, offset),
        PhysicalType::Int64 => typed::<i64>(source, count, target, offset),
        PhysicalType::Int128 => typed::<i128>(source, count, target, offset),
        PhysicalType::UInt8 => typed::<u8>(source, count, target, offset),
        PhysicalType::UInt16 => typed::<u16>(source, count, target, offset),
        PhysicalType::UInt32 => typed::<u32>(source, count, target, offset),
        PhysicalType::UInt64 => typed::<u64>(source, count, target, offset),
        PhysicalType::UInt128 => typed::<u128>(source, count, target, offset),
        PhysicalType::Float => typed::<f32>(source, count, target, offset),
        PhysicalType::Double => typed::<f64>(source, count, target, offset),
        PhysicalType::Interval => typed::<rustdb_types::Interval>(source, count, target, offset),
        PhysicalType::Varchar => {
            typed::<StringT>(source, count, target, offset);
            target.add_heap_references(source);
        }
        PhysicalType::List | PhysicalType::Struct | PhysicalType::Array => {
            // Nested values are copied value by value until a nested copy kernel exists.
            for row in 0..count {
                target.set_value(offset + row, &source.get_value(row))?;
            }
        }
        other => {
            return Err(Error::internal(format!(
                "Unsupported type for vector copy: {}",
                other.name()
            )));
        }
    }
    Ok(())
}

/// Whether any of the first `count` rows of `vector` is NULL.
pub(crate) fn has_nulls(vector: &Vector, count: usize) -> bool {
    vector.kind() != VectorKind::Sequence
        && !vector.validity().is_all_valid()
        && (0..count).any(|row| vector.is_null(row))
}

/// `a IS DISTINCT FROM b` for optional values compared with `eq`.
#[inline]
fn distinct<T>(a: Option<T>, b: Option<T>, eq: impl Fn(T, T) -> bool) -> bool {
    match (a, b) {
        (None, None) => false,
        (Some(a), Some(b)) => !eq(a, b),
        _ => true,
    }
}

/// ORs into `flags[i]` whether row `i` of `column` is distinct from the row before it; the
/// row before row 0 is `previous` (if any).
fn mark_column(
    column: &Vector,
    count: usize,
    previous: Option<(&Vector, usize)>,
    flags: &mut [bool],
) -> Result<()> {
    fn fixed<T: NativeType>(
        column: &Vector,
        count: usize,
        previous: Option<(&Vector, usize)>,
        flags: &mut [bool],
        eq: impl Fn(T, T) -> bool + Copy,
    ) {
        let values = column.unified::<T>();
        if let Some((vector, row)) = previous {
            flags[0] |= distinct(vector.unified::<T>().get(row), values.get(0), eq);
        }
        if matches!(values.sel, rustdb_types::Sel::Identity) && values.validity.is_all_valid() {
            // Flat without NULLs: compare neighbouring slots directly.
            for ((flag, &prev), &curr) in flags[1..count]
                .iter_mut()
                .zip(&values.values[..count - 1])
                .zip(&values.values[1..count])
            {
                *flag |= !eq(prev, curr);
            }
            return;
        }
        for (row, flag) in flags.iter_mut().enumerate().take(count).skip(1) {
            *flag |= distinct(values.get(row - 1), values.get(row), eq);
        }
    }
    fn strings(column: &Vector, count: usize, previous: Option<(&Vector, usize)>, flags: &mut [bool]) {
        let values = column.unified::<StringT>();
        let bytes =
            |vector: &Vector, value: Option<StringT>| value.map(|value| vector.string_value(&value).to_vec());
        if let Some((vector, row)) = previous {
            let before = bytes(vector, vector.unified::<StringT>().get(row));
            flags[0] |= distinct(
                before.as_deref(),
                bytes(column, values.get(0)).as_deref(),
                |a, b| a == b,
            );
        }
        for (row, flag) in flags.iter_mut().enumerate().take(count).skip(1) {
            let (a, b) = (values.get(row - 1), values.get(row));
            *flag |= distinct(a.as_ref(), b.as_ref(), |a, b| {
                a.len() == b.len() && column.string_value(a) == column.string_value(b)
            });
        }
    }
    if count == 0 {
        return Ok(());
    }
    if column.kind() == VectorKind::Sequence {
        return mark_column(&column.flattened(count), count, previous, flags);
    }
    match column.logical_type().physical_type() {
        PhysicalType::Bool => fixed::<bool>(column, count, previous, flags, |a, b| a == b),
        PhysicalType::Int8 => fixed::<i8>(column, count, previous, flags, |a, b| a == b),
        PhysicalType::Int16 => fixed::<i16>(column, count, previous, flags, |a, b| a == b),
        PhysicalType::Int32 => fixed::<i32>(column, count, previous, flags, |a, b| a == b),
        PhysicalType::Int64 => fixed::<i64>(column, count, previous, flags, |a, b| a == b),
        PhysicalType::Int128 => fixed::<i128>(column, count, previous, flags, |a, b| a == b),
        PhysicalType::UInt8 => fixed::<u8>(column, count, previous, flags, |a, b| a == b),
        PhysicalType::UInt16 => fixed::<u16>(column, count, previous, flags, |a, b| a == b),
        PhysicalType::UInt32 => fixed::<u32>(column, count, previous, flags, |a, b| a == b),
        PhysicalType::UInt64 => fixed::<u64>(column, count, previous, flags, |a, b| a == b),
        PhysicalType::UInt128 => fixed::<u128>(column, count, previous, flags, |a, b| a == b),
        PhysicalType::Float => fixed::<f32>(column, count, previous, flags, SqlOrd::sql_eq),
        PhysicalType::Double => fixed::<f64>(column, count, previous, flags, SqlOrd::sql_eq),
        PhysicalType::Interval => fixed::<rustdb_types::Interval>(
            column,
            count,
            previous,
            flags,
            rustdb_types::Interval::normalized_eq,
        ),
        PhysicalType::Varchar => strings(column, count, previous, flags),
        _ => {
            // Nested types: compare value by value with DuckDB's nested semantics.
            if let Some((vector, row)) = previous {
                flags[0] |= rustdb_row::values_distinct(vector, row, column, 0)?;
            }
            for (row, flag) in flags.iter_mut().enumerate().take(count).skip(1) {
                *flag |= rustdb_row::values_distinct(column, row - 1, column, row)?;
            }
        }
    }
    Ok(())
}

/// Sets `flags[row]` when the key of row `row` is distinct from the key of the row before
/// it (DuckDB's `WindowDeltaScanner` with `DistinctFrom` on the key prefix). `chunks[c]`
/// holds the key columns of chunk `c`, which has `lens[c]` rows; rows are numbered across
/// chunks. `flags[0]` is left as is.
pub(crate) fn delta_flags(chunks: &[Vec<&Vector>], lens: &[usize], flags: &mut [bool]) -> Result<()> {
    let mut offset = 0;
    let mut previous: Option<(&[&Vector], usize)> = None;
    for (columns, &count) in chunks.iter().zip(lens) {
        if count == 0 {
            continue;
        }
        let rows = &mut flags[offset..offset + count];
        for (key, column) in columns.iter().enumerate() {
            let before = previous.map(|(columns, row)| (columns[key], row));
            mark_column(column, count, before, rows)?;
        }
        previous = Some((columns.as_slice(), count - 1));
        offset += count;
    }
    Ok(())
}

/// The materialised collection columns of one hash group.
#[derive(Debug)]
pub(crate) struct WindowCollection {
    count: usize,
    columns: Vec<Vector>,
    /// Whether every appended value of a column was valid.
    all_valid: Vec<bool>,
    /// NULL masks of the columns that requested one and have NULLs.
    validities: Vec<Option<WindowMask>>,
}

impl WindowCollection {
    /// An empty collection for `count` rows of `types`.
    pub(crate) fn new(types: &[LogicalType], count: usize) -> Self {
        Self {
            count,
            columns: types.iter().map(|ty| Vector::flat(ty.clone(), count)).collect(),
            all_valid: vec![true; types.len()],
            validities: vec![None; types.len()],
        }
    }

    /// Rows in the collection.
    pub(crate) const fn size(&self) -> usize {
        self.count
    }

    /// Writes the chunk's columns at rows `input_idx..` (DuckDB's `WindowBuilder::Sink`).
    pub(crate) fn append(&mut self, chunk: &DataChunk, input_idx: usize) -> Result<()> {
        let count = chunk.len();
        if input_idx + count > self.count {
            return Err(Error::internal("window collection overflow"));
        }
        for ((source, target), all_valid) in chunk
            .columns()
            .iter()
            .zip(&mut self.columns)
            .zip(&mut self.all_valid)
        {
            copy_into(source, count, target, input_idx)?;
            if *all_valid && has_nulls(source, count) {
                *all_valid = false;
            }
        }
        Ok(())
    }

    /// Builds the NULL masks of the requested columns (DuckDB's `WindowCollection::Combine`).
    pub(crate) fn combine(&mut self, validity_columns: &[usize]) {
        for &column in validity_columns {
            if column < self.columns.len() && !self.all_valid[column] && self.validities[column].is_none() {
                self.validities[column] = Some(WindowMask::from_validity(
                    self.columns[column].validity(),
                    self.count,
                ));
            }
        }
    }

    /// Column `index`.
    pub(crate) fn column(&self, index: usize) -> &Vector {
        &self.columns[index]
    }

    /// Whether every value of column `index` is valid.
    pub(crate) fn all_valid(&self, index: usize) -> bool {
        self.all_valid[index]
    }

    /// The NULL mask of column `index`, if it was requested and has NULLs.
    pub(crate) fn validity(&self, index: usize) -> Option<&WindowMask> {
        self.validities.get(index).and_then(Option::as_ref)
    }
}
