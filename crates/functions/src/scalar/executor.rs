//! Row loops for scalar function kernels (DuckDB's `UnaryExecutor`, `BinaryExecutor`,
//! `TernaryExecutor` and `GenericExecutor`).
//!
//! The kernels see typed values: fixed-width types as themselves and strings as byte slices
//! ([`Str`]). Constant inputs give a constant result, NULL inputs give NULL (the
//! `NullHandling::Default` contract), and a kernel can return NULL for a row by returning
//! `None` or `false`.

use std::borrow::Cow;

use rustdb_common::Result;
use rustdb_types::{DataChunk, NativeType, Sel, StringHeap, StringT, Unified, Vector, VectorKind};

/// How a kernel argument of some physical type is presented to the kernel.
pub trait ScalarArg: 'static {
    /// The native type stored in the vector.
    type Native: NativeType;
    /// What the kernel receives for one row.
    type Item<'a>: Copy;

    fn item<'a>(vector: &'a Vector, value: &'a Self::Native) -> Self::Item<'a>;
}

impl<T: NativeType> ScalarArg for T {
    type Native = T;
    type Item<'a> = T;

    #[inline]
    fn item<'a>(_: &'a Vector, value: &'a T) -> T {
        *value
    }
}

/// A VARCHAR/BLOB/BIT argument, presented as its bytes.
#[derive(Debug)]
pub struct Str;

impl ScalarArg for Str {
    type Native = StringT;
    type Item<'a> = &'a [u8];

    #[inline]
    fn item<'a>(vector: &'a Vector, value: &'a StringT) -> &'a [u8] {
        vector.string_value(value)
    }
}

/// `vector` with sequences expanded, so that unified access works.
pub fn addressable(vector: &Vector, count: usize) -> Cow<'_, Vector> {
    if vector.kind() == VectorKind::Sequence {
        Cow::Owned(vector.flattened(count))
    } else {
        Cow::Borrowed(vector)
    }
}

/// The bytes of row `row` of a (non-sequence) VARCHAR/BLOB/BIT vector, `None` if NULL.
pub fn str_at(vector: &Vector, row: usize) -> Option<&[u8]> {
    Input::<Str>::new(vector).get(row)
}

/// An argument prepared for row access.
struct Input<'a, A: ScalarArg> {
    vector: &'a Vector,
    unified: Unified<'a, A::Native>,
}

impl<'a, A: ScalarArg> Input<'a, A> {
    fn new(vector: &'a Vector) -> Self {
        Self {
            vector,
            unified: vector.unified::<A::Native>(),
        }
    }

    #[inline]
    fn get(&self, row: usize) -> Option<A::Item<'a>> {
        let slot = self.unified.slot(row);
        self.unified
            .validity
            .is_valid(slot)
            .then(|| A::item(self.vector, &self.unified.values[slot]))
    }

    /// Whether this is a flat input without NULLs.
    fn is_flat_all_valid(&self) -> bool {
        matches!(self.unified.sel, Sel::Identity) && self.unified.validity.is_all_valid()
    }
}

/// A kernel result type: a fixed-width value or bytes written into a string heap.
pub trait ScalarOutput {
    /// What the kernel produces for one row.
    type Value;

    /// Writes `rows` results produced by `value(row)` (`None` = NULL) into `result`.
    fn write(
        result: &mut Vector,
        rows: usize,
        value: impl FnMut(usize, &mut Self::Value) -> Result<bool>,
    ) -> Result<()>;
}

impl<T: NativeType> ScalarOutput for T {
    type Value = T;

    fn write(
        result: &mut Vector,
        rows: usize,
        mut value: impl FnMut(usize, &mut T) -> Result<bool>,
    ) -> Result<()> {
        result.reset_flat(rows);
        let mut nulls = Vec::new();
        {
            let data = &mut result.values_mut::<T>()[..rows];
            for (row, out) in data.iter_mut().enumerate() {
                if !value(row, out)? {
                    nulls.push(row);
                }
            }
        }
        if !nulls.is_empty() {
            let validity = result.validity_mut();
            for row in nulls {
                validity.set_invalid(row, rows);
            }
        }
        Ok(())
    }
}

/// DuckDB's `string_t::MAX_STRING_SIZE`.
const MAX_STRING_SIZE: u64 = u32::MAX as u64;

/// The error for a string result longer than DuckDB supports (`StringHeap::EmptyString`).
pub fn string_too_large(size: u64) -> rustdb_common::Error {
    rustdb_common::Error::out_of_range(format!(
        "Cannot create a string of size: '{size}', the maximum supported string size is: '{MAX_STRING_SIZE}'"
    ))
}

/// A string result: the kernel appends the row's bytes to the given buffer.
#[derive(Debug)]
pub struct StrOut;

impl ScalarOutput for StrOut {
    type Value = Vec<u8>;

    fn write(
        result: &mut Vector,
        rows: usize,
        mut value: impl FnMut(usize, &mut Vec<u8>) -> Result<bool>,
    ) -> Result<()> {
        result.reset_flat(rows);
        let mut heap = StringHeap::default();
        let mut buffer = Vec::new();
        let mut nulls = Vec::new();
        {
            let data = &mut result.values_mut::<StringT>()[..rows];
            for (row, out) in data.iter_mut().enumerate() {
                buffer.clear();
                if value(row, &mut buffer)? {
                    if buffer.len() as u64 > MAX_STRING_SIZE {
                        return Err(string_too_large(buffer.len() as u64));
                    }
                    *out = heap.add(&buffer);
                } else {
                    nulls.push(row);
                }
            }
        }
        result.add_heap(heap);
        if !nulls.is_empty() {
            let validity = result.validity_mut();
            for row in nulls {
                validity.set_invalid(row, rows);
            }
        }
        Ok(())
    }
}

/// Number of result rows and whether the result is constant.
fn result_shape(inputs: &[&Vector], count: usize) -> (usize, bool) {
    let constant = inputs.iter().all(|input| input.is_constant());
    (if constant { 1 } else { count }, constant)
}

fn finish(result: &mut Vector, constant: bool) {
    if constant {
        result.make_constant();
    }
}

/// Applies `f` to every row of a one-argument call; NULL inputs give NULL. `f` writes the
/// result into its second argument and returns `false` for NULL.
pub fn unary<A: ScalarArg, O: ScalarOutput>(
    args: &DataChunk,
    result: &mut Vector,
    mut f: impl FnMut(A::Item<'_>, &mut O::Value) -> Result<bool>,
) -> Result<()> {
    let count = args.len();
    let a = addressable(args.column(0), count);
    let (rows, constant) = result_shape(&[&a], count);
    let a = Input::<A>::new(&a);
    if a.is_flat_all_valid() {
        O::write(result, rows, |row, out| {
            f(A::item(a.vector, &a.unified.values[row]), out)
        })?;
    } else {
        O::write(result, rows, |row, out| match a.get(row) {
            Some(a) => f(a, out),
            None => Ok(false),
        })?;
    }
    finish(result, constant);
    Ok(())
}

/// Like [`unary`] for two arguments.
pub fn binary<A: ScalarArg, B: ScalarArg, O: ScalarOutput>(
    args: &DataChunk,
    result: &mut Vector,
    mut f: impl FnMut(A::Item<'_>, B::Item<'_>, &mut O::Value) -> Result<bool>,
) -> Result<()> {
    let count = args.len();
    let a = addressable(args.column(0), count);
    let b = addressable(args.column(1), count);
    let (rows, constant) = result_shape(&[&a, &b], count);
    let a = Input::<A>::new(&a);
    let b = Input::<B>::new(&b);
    if a.unified.validity.is_all_valid() && b.unified.validity.is_all_valid() {
        // No NULLs: no per-row validity checks, and flat or constant inputs index their
        // values directly.
        let (av, bv) = (a.unified.values, b.unified.values);
        match (a.unified.sel, b.unified.sel) {
            (Sel::Identity, Sel::Identity) => O::write(result, rows, |row, out| {
                f(A::item(a.vector, &av[row]), B::item(b.vector, &bv[row]), out)
            })?,
            (Sel::Identity, Sel::Constant) => {
                let b = B::item(b.vector, &bv[0]);
                O::write(result, rows, |row, out| f(A::item(a.vector, &av[row]), b, out))?;
            }
            (Sel::Constant, Sel::Identity) => {
                let a = A::item(a.vector, &av[0]);
                O::write(result, rows, |row, out| f(a, B::item(b.vector, &bv[row]), out))?;
            }
            (sa, sb) => O::write(result, rows, |row, out| {
                f(
                    A::item(a.vector, &av[sa.slot(row)]),
                    B::item(b.vector, &bv[sb.slot(row)]),
                    out,
                )
            })?,
        }
    } else {
        O::write(result, rows, |row, out| match (a.get(row), b.get(row)) {
            (Some(a), Some(b)) => f(a, b, out),
            _ => Ok(false),
        })?;
    }
    finish(result, constant);
    Ok(())
}

/// Like [`unary`] for three arguments.
pub fn ternary<A: ScalarArg, B: ScalarArg, C: ScalarArg, O: ScalarOutput>(
    args: &DataChunk,
    result: &mut Vector,
    mut f: impl FnMut(A::Item<'_>, B::Item<'_>, C::Item<'_>, &mut O::Value) -> Result<bool>,
) -> Result<()> {
    let count = args.len();
    let a = addressable(args.column(0), count);
    let b = addressable(args.column(1), count);
    let c = addressable(args.column(2), count);
    let (rows, constant) = result_shape(&[&a, &b, &c], count);
    let a = Input::<A>::new(&a);
    let b = Input::<B>::new(&b);
    let c = Input::<C>::new(&c);
    O::write(result, rows, |row, out| {
        match (a.get(row), b.get(row), c.get(row)) {
            (Some(a), Some(b), Some(c)) => f(a, b, c, out),
            _ => Ok(false),
        }
    })?;
    finish(result, constant);
    Ok(())
}

/// Applies `f` to every row of a call with any number of arguments of one kind; rows with a
/// NULL argument are NULL unless `nulls` is set, in which case `f` sees them as `None`.
pub fn variadic<A: ScalarArg, O: ScalarOutput>(
    args: &DataChunk,
    result: &mut Vector,
    nulls: bool,
    mut f: impl FnMut(&[Option<A::Item<'_>>], &mut O::Value) -> Result<bool>,
) -> Result<()> {
    let count = args.len();
    let columns: Vec<Cow<'_, Vector>> = args
        .columns()
        .iter()
        .map(|column| addressable(column, count))
        .collect();
    let refs: Vec<&Vector> = columns.iter().map(AsRef::as_ref).collect();
    let (rows, constant) = result_shape(&refs, count);
    let inputs: Vec<Input<'_, A>> = refs.iter().map(|column| Input::new(column)).collect();
    let mut values = Vec::with_capacity(inputs.len());
    O::write(result, rows, |row, out| {
        values.clear();
        values.extend(inputs.iter().map(|input| input.get(row)));
        if !nulls && values.iter().any(Option::is_none) {
            return Ok(false);
        }
        f(&values, out)
    })?;
    finish(result, constant);
    Ok(())
}
