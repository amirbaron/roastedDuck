//! The row loops shared by every scalar cast kernel (DuckDB's `UnaryExecutor` +
//! `VectorTryCastData` / `HandleVectorCastError`).
//!
//! A kernel supplies a per-row conversion that reads the source by *slot*; the loop takes
//! care of the source representation (constant inputs give constant results, flat inputs
//! without NULLs run a tight loop), NULL propagation and the CAST vs `TRY_CAST` handling of
//! rows that fail to convert.

use rustdb_common::{Error, Result};
use rustdb_types::{NativeType, Sel, StringHeap, StringT, Vector, VectorKind};

use super::CastParameters;

/// Why a single row failed to convert.
#[derive(Debug, PartialEq)]
pub enum RowError {
    /// The row cannot be converted: `CAST` raises this message, `TRY_CAST` yields NULL.
    Invalid(String),
    /// An error raised even under `TRY_CAST` (DuckDB throws these directly).
    Fatal(Error),
    /// The row becomes NULL without counting as a failed conversion.
    Null,
    /// Like [`RowError::Invalid`], but `CAST` raises this exact error (DuckDB throws some
    /// conversion failures with other error kinds).
    Error(Error),
}

impl From<Error> for RowError {
    fn from(error: Error) -> Self {
        Self::Fatal(error)
    }
}

/// The result of converting one row.
pub type RowResult<T> = std::result::Result<T, RowError>;

/// `source` with sequences expanded, so that slot-based access works.
pub fn addressable(source: &Vector, count: usize) -> std::borrow::Cow<'_, Vector> {
    if source.kind() == VectorKind::Sequence {
        std::borrow::Cow::Owned(source.flattened(count))
    } else {
        std::borrow::Cow::Borrowed(source)
    }
}

/// Converts every non-NULL row of `source` with `convert(slot)` into a fresh `result`.
///
/// Returns `Ok(false)` if some rows failed under `TRY_CAST` (they are NULL), and an error
/// for the first failing row under `CAST`.
pub fn cast_rows<O: NativeType>(
    source: &Vector,
    result: &mut Vector,
    count: usize,
    params: &mut CastParameters,
    mut convert: impl FnMut(usize) -> RowResult<O>,
) -> Result<bool> {
    let constant = source.is_constant();
    let rows = if constant { 1 } else { count };
    result.reset_flat(rows);
    let sel = source.sel();
    let validity = source.validity();
    let mut null_rows = Vec::new();
    let mut all_converted = true;
    {
        let out = &mut result.values_mut::<O>()[..rows];
        let mut handle = |row: usize, error: RowError, null_rows: &mut Vec<usize>| -> Result<()> {
            match error {
                RowError::Invalid(message) => {
                    params.assign_error(message)?;
                    all_converted = false;
                    null_rows.push(row);
                    Ok(())
                }
                RowError::Fatal(error) => Err(error),
                RowError::Null => {
                    null_rows.push(row);
                    Ok(())
                }
                RowError::Error(error) => {
                    if !params.is_try_cast() {
                        return Err(error);
                    }
                    params.assign_error_bytes(error.raw_message_bytes().to_vec())?;
                    all_converted = false;
                    null_rows.push(row);
                    Ok(())
                }
            }
        };
        if validity.is_all_valid() && matches!(sel, Sel::Identity) {
            for (row, out) in out.iter_mut().enumerate() {
                match convert(row) {
                    Ok(value) => *out = value,
                    Err(error) => handle(row, error, &mut null_rows)?,
                }
            }
        } else {
            for (row, out) in out.iter_mut().enumerate() {
                let slot = sel.slot(row);
                if !validity.is_valid(slot) {
                    null_rows.push(row);
                    continue;
                }
                match convert(slot) {
                    Ok(value) => *out = value,
                    Err(error) => handle(row, error, &mut null_rows)?,
                }
            }
        }
    }
    if !null_rows.is_empty() {
        let mask = result.validity_mut();
        for row in null_rows {
            mask.set_invalid(row, rows);
        }
    }
    if constant {
        result.make_constant();
    }
    Ok(all_converted)
}

/// Like [`cast_rows`] for VARCHAR-like results: `convert(slot, out)` appends the text of a
/// row to a reused scratch buffer.
pub fn cast_rows_to_string(
    source: &Vector,
    result: &mut Vector,
    count: usize,
    params: &mut CastParameters,
    mut convert: impl FnMut(usize, &mut Vec<u8>) -> RowResult<()>,
) -> Result<bool> {
    let mut heap = StringHeap::default();
    let mut scratch = Vec::with_capacity(32);
    let all_converted = cast_rows::<StringT>(source, result, count, params, |slot| {
        scratch.clear();
        convert(slot, &mut scratch)?;
        Ok(heap.add(&scratch))
    })?;
    result.add_heap(heap);
    Ok(all_converted)
}

/// Converts the values of a fixed-width vector with a fallible function.
pub fn try_map_values<I: NativeType, O: NativeType>(
    source: &Vector,
    result: &mut Vector,
    count: usize,
    params: &mut CastParameters,
    convert: impl Fn(I) -> RowResult<O>,
) -> Result<bool> {
    let source = addressable(source, count);
    let values = source.values::<I>();
    cast_rows(&source, result, count, params, |slot| convert(values[slot]))
}

/// Renders the values of a fixed-width vector as text.
pub fn values_to_string<I: NativeType>(
    source: &Vector,
    result: &mut Vector,
    count: usize,
    params: &mut CastParameters,
    write: impl Fn(I, &mut Vec<u8>),
) -> Result<bool> {
    let source = addressable(source, count);
    let values = source.values::<I>();
    cast_rows_to_string(&source, result, count, params, |slot, out| {
        write(values[slot], out);
        Ok(())
    })
}

/// Renders the values of a fixed-width vector as text with a fallible function.
pub fn try_values_to_string<I: NativeType>(
    source: &Vector,
    result: &mut Vector,
    count: usize,
    params: &mut CastParameters,
    write: impl Fn(I, &mut Vec<u8>) -> RowResult<()>,
) -> Result<bool> {
    let source = addressable(source, count);
    let values = source.values::<I>();
    cast_rows_to_string(&source, result, count, params, |slot, out| {
        write(values[slot], out)
    })
}

/// Converts the strings of a VARCHAR/BLOB vector with a fallible function.
pub fn try_map_strings<O: NativeType>(
    source: &Vector,
    result: &mut Vector,
    count: usize,
    params: &mut CastParameters,
    convert: impl Fn(&[u8]) -> RowResult<O>,
) -> Result<bool> {
    let strings = source.values::<StringT>();
    cast_rows(source, result, count, params, |slot| {
        convert(source.string_value(&strings[slot]))
    })
}

/// Converts the strings of a VARCHAR/BLOB vector into new strings.
pub fn try_map_strings_to_string(
    source: &Vector,
    result: &mut Vector,
    count: usize,
    params: &mut CastParameters,
    convert: impl Fn(&[u8], &mut Vec<u8>) -> RowResult<()>,
) -> Result<bool> {
    let strings = source.values::<StringT>();
    cast_rows_to_string(source, result, count, params, |slot, out| {
        convert(source.string_value(&strings[slot]), out)
    })
}

/// Builds a flat VARCHAR vector row by row (child vectors of nested casts).
pub(super) struct StringColumn {
    heap: StringHeap,
    values: Vec<StringT>,
    valid: Vec<bool>,
}

impl StringColumn {
    /// A column of `rows` NULLs, to be filled with [`StringColumn::set`].
    pub(super) fn nulls(rows: usize) -> Self {
        Self {
            heap: StringHeap::default(),
            values: vec![StringT::default(); rows],
            valid: vec![false; rows],
        }
    }

    /// An empty column, to be filled with [`StringColumn::push`].
    pub(super) fn new() -> Self {
        Self::nulls(0)
    }

    pub(super) fn len(&self) -> usize {
        self.values.len()
    }

    /// Appends a value (`None` is NULL).
    pub(super) fn push(&mut self, value: Option<&[u8]>) {
        self.values
            .push(value.map_or_else(StringT::default, |bytes| self.heap.add(bytes)));
        self.valid.push(value.is_some());
    }

    /// Overwrites row `row` (`None` is NULL).
    pub(super) fn set(&mut self, row: usize, value: Option<&[u8]>) {
        if let (Some(slot), Some(valid)) = (self.values.get_mut(row), self.valid.get_mut(row)) {
            *slot = value.map_or_else(StringT::default, |bytes| self.heap.add(bytes));
            *valid = value.is_some();
        }
    }

    /// The finished flat VARCHAR vector.
    pub(super) fn finish(self) -> Vector {
        let rows = self.values.len();
        let mut vector = Vector::flat(rustdb_types::LogicalType::VARCHAR, rows);
        vector.values_mut::<StringT>()[..rows].copy_from_slice(&self.values);
        if self.valid.iter().any(|valid| !valid) {
            let mask = vector.validity_mut();
            for (row, _) in self.valid.iter().enumerate().filter(|(_, valid)| !**valid) {
                mask.set_invalid(row, rows);
            }
        }
        vector.add_heap(self.heap);
        vector
    }
}
