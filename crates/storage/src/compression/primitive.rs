//! Fixed-width values as they are stored in compressed segments, and helpers shared by
//! the codecs: bounds-checked loads (DuckDB's `Load<T>` with the corruption checks the
//! codecs perform) and writing decoded values into result vectors.

use std::any::Any;
use std::fmt;
use std::ops::Range;
use std::sync::Arc;

use rustdb_common::{Error, ErrorKind, Result};
use rustdb_types::{StringT, ValidityMask, Vector};

/// A value with a little-endian on-disk representation of `SIZE` bytes (the `T` of
/// DuckDB's templated codecs).
pub(crate) trait Primitive: Copy + Default + PartialEq + Send + Sync + fmt::Debug + 'static {
    const SIZE: usize;
    /// Reads the value from the first `SIZE` bytes of `bytes`.
    fn load(bytes: &[u8]) -> Self;
    /// Writes the value to the first `SIZE` bytes of `out`.
    fn store(self, out: &mut [u8]);
}

macro_rules! primitive {
    ($($ty:ty),* $(,)?) => {$(
        impl Primitive for $ty {
            const SIZE: usize = size_of::<$ty>();

            #[inline]
            fn load(bytes: &[u8]) -> Self {
                let mut raw = [0u8; size_of::<$ty>()];
                raw.copy_from_slice(&bytes[..size_of::<$ty>()]);
                <$ty>::from_le_bytes(raw)
            }

            #[inline]
            fn store(self, out: &mut [u8]) {
                out[..size_of::<$ty>()].copy_from_slice(&self.to_le_bytes());
            }
        }
    )*};
}

primitive!(i8, i16, i32, i64, i128, u8, u16, u32, u64, u128, f32, f64);

/// DuckDB's `IOException` for a corrupted segment.
pub(crate) fn corrupt(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::Io, message.into())
}

/// `Load<T>(data + offset)`, failing on reads past the end of the segment's block.
#[inline]
pub(crate) fn load<T: Primitive>(data: &[u8], offset: usize) -> Result<T> {
    data.get(offset..offset.saturating_add(T::SIZE))
        .filter(|bytes| bytes.len() == T::SIZE)
        .map(T::load)
        .ok_or_else(|| {
            corrupt(format!(
                "read of {} bytes at offset {offset} is out of the segment",
                T::SIZE
            ))
        })
}

/// `data[offset..offset + len]`, failing on reads past the end of the segment's block.
#[inline]
pub(crate) fn slice(data: &[u8], offset: usize, len: usize) -> Result<&[u8]> {
    data.get(offset..offset.saturating_add(len))
        .filter(|bytes| bytes.len() == len)
        .ok_or_else(|| {
            corrupt(format!(
                "read of {len} bytes at offset {offset} is out of the segment"
            ))
        })
}

/// Rounds `value` up to a multiple of 8 (`AlignValue`).
#[inline]
pub(crate) const fn align8(value: usize) -> usize {
    value.next_multiple_of(8)
}

/// The data bytes of rows `offset..offset + count` of a flat result vector holding values of
/// `T::SIZE` bytes.
fn result_bytes<T: Primitive>(result: &mut Vector, offset: usize, count: usize) -> Result<&mut [u8]> {
    let start = offset * T::SIZE;
    result
        .data_bytes_mut()
        .get_mut(start..start + count * T::SIZE)
        .ok_or_else(|| Error::internal("scan target vector too small"))
}

/// Writes `values` to rows `offset..` of a flat result vector.
#[inline]
pub(crate) fn write_values<T: Primitive>(result: &mut Vector, offset: usize, values: &[T]) -> Result<()> {
    let target = result_bytes::<T>(result, offset, values.len())?;
    for (chunk, value) in target.chunks_exact_mut(T::SIZE).zip(values) {
        value.store(chunk);
    }
    Ok(())
}

/// Writes `value` to rows `offset..offset + count` of a flat result vector.
#[inline]
pub(crate) fn fill_values<T: Primitive>(
    result: &mut Vector,
    offset: usize,
    count: usize,
    value: T,
) -> Result<()> {
    let target = result_bytes::<T>(result, offset, count)?;
    for chunk in target.chunks_exact_mut(T::SIZE) {
        value.store(chunk);
    }
    Ok(())
}

/// Marks rows of a result vector as NULL wherever `is_valid(row)` is false, for rows
/// `offset..offset + count` (`row` counts from 0 at `offset`).
pub(crate) fn set_invalid_where(
    result: &mut Vector,
    offset: usize,
    count: usize,
    is_valid: impl Fn(usize) -> bool,
) {
    let mut words: Option<Vec<u64>> = None;
    let capacity = result.capacity().max(offset + count);
    for row in 0..count {
        if is_valid(row) {
            continue;
        }
        let words = words.get_or_insert_with(|| {
            let mut words = result
                .validity()
                .words()
                .map_or_else(|| vec![u64::MAX; capacity.div_ceil(64)], <[u64]>::to_vec);
            words.resize(capacity.div_ceil(64), u64::MAX);
            words
        });
        let target = offset + row;
        words[target / 64] &= !(1u64 << (target % 64));
    }
    if let Some(words) = words {
        result.set_validity(ValidityMask::from_words(words));
    }
}

/// Reads bit `index` of a little-endian bitmap of 64-bit words (bit set = 1).
#[inline]
pub(crate) fn bit(words: &[u64], index: usize) -> bool {
    words
        .get(index / 64)
        .is_some_and(|word| word >> (index % 64) & 1 == 1)
}

/// Writes strings to rows `offset..` of a flat VARCHAR result vector without copying them:
/// each value points into `owner`'s bytes at one of `ranges`, and `owner` is kept alive by
/// the vector.
pub(crate) fn write_string_ranges<O>(
    result: &mut Vector,
    offset: usize,
    owner: &Arc<O>,
    ranges: impl IntoIterator<Item = Range<usize>>,
) -> Result<()>
where
    O: AsRef<[u8]> + Send + Sync + 'static,
{
    let bytes = (**owner).as_ref();
    let mut values = Vec::new();
    for range in ranges {
        let string = bytes
            .get(range)
            .ok_or_else(|| corrupt("string out of range of its segment - corrupt database file"))?;
        // SAFETY: the bytes belong to `owner`, which is immutable behind its `Arc` and is
        // kept alive by `result` below for as long as the vector (and its copies) exist.
        values.push(unsafe { StringT::from_raw(string) });
    }
    result
        .values_mut::<StringT>()
        .get_mut(offset..offset + values.len())
        .ok_or_else(|| Error::internal("scan target vector too small"))?
        .copy_from_slice(&values);
    let keep_alive: Arc<dyn Any + Send + Sync> = Arc::clone(owner) as Arc<dyn Any + Send + Sync>;
    result.add_keep_alive(keep_alive);
    Ok(())
}
