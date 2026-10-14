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
pub(super) enum RowError {
    /// The row cannot be converted: `CAST` raises this message, `TRY_CAST` yields NULL.
    Invalid(String),
    /// An error raised even under `TRY_CAST` (DuckDB throws these directly).
    Fatal(Error),
}

impl From<Error> for RowError {
    fn from(error: Error) -> Self {
        Self::Fatal(error)
    }
}

/// The result of converting one row.
pub(super) type RowResult<T> = std::result::Result<T, RowError>;

/// `source` with sequences expanded, so that slot-based access works.
pub(super) fn addressable(source: &Vector, count: usize) -> std::borrow::Cow<'_, Vector> {
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
pub(super) fn cast_rows<O: NativeType>(
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
pub(super) fn cast_rows_to_string(
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
pub(super) fn try_map_values<I: NativeType, O: NativeType>(
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
pub(super) fn values_to_string<I: NativeType>(
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

/// Converts the strings of a VARCHAR/BLOB vector with a fallible function.
pub(super) fn try_map_strings<O: NativeType>(
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
pub(super) fn try_map_strings_to_string(
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
