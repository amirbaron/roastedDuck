//! The compressed materialization functions (DuckDB's
//! `function/scalar/compressed_materialization`): `__internal_compress_integral_*`,
//! `__internal_decompress_integral_*`, `__internal_compress_string_*` and
//! `__internal_decompress_string`.
//!
//! The optimizer inserts them around materializing operators to shrink integers (by
//! subtracting the column minimum) and short strings (packed into an unsigned integer
//! that orders like the string). Like DuckDB, binding them from SQL is an error.

use rustdb_common::{Error, Result};
use rustdb_function::{FunctionData, ScalarFunction, ScalarFunctionSet};
use rustdb_types::{DataChunk, LogicalType, NativeType, PhysicalType, StringHeap, StringT, Vector};

/// `CMUtils::Bind`.
fn bind_internal(
    _: &mut ScalarFunction,
    _: &rustdb_function::BindInput<'_>,
) -> Result<Option<std::sync::Arc<dyn FunctionData>>> {
    Err(Error::binder(
        "Compressed materialization functions are for internal use only!",
    ))
}

/// An integer type the functions handle, through its two's complement bits.
trait Bits: NativeType + Copy {
    fn to_bits(self) -> u128;
    fn from_bits(bits: u128) -> Self;
}

macro_rules! bits {
    ($($ty:ty),*) => {$(
        impl Bits for $ty {
            #[allow(clippy::cast_sign_loss, clippy::cast_lossless, reason = "two's complement bits")]
            fn to_bits(self) -> u128 {
                self as u128
            }

            #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap, reason = "UnsafeNumericCast")]
            fn from_bits(bits: u128) -> Self {
                bits as $ty
            }
        }
    )*};
}
bits!(i16, i32, i64, i128, u8, u16, u32, u64, u128);

/// The constant `min` argument (`ConstantVector::GetData(args.data[1])[0]`).
fn constant_min<T: NativeType>(args: &DataChunk) -> T {
    let min = &args.columns()[1];
    let data = min.unified::<T>();
    data.values[data.slot(0)]
}

/// `input - min` as the narrower unsigned type (`TemplatedIntegralCompress`, a
/// `UnaryExecutor` over the input with the constant minimum).
#[allow(clippy::unnecessary_wraps, reason = "a `Kernel`")]
fn compress_kernel<I: Bits, R: Bits>(args: &DataChunk, result: &mut Vector) -> Result<()> {
    let min = constant_min::<I>(args).to_bits();
    rustdb_types::kernels::unary::<I, R>(&args.columns()[0], args.len(), result, |input| {
        R::from_bits(input.to_bits().wrapping_sub(min))
    });
    Ok(())
}

/// `min + input` (`TemplatedIntegralDecompress`).
#[allow(clippy::unnecessary_wraps, reason = "a `Kernel`")]
fn decompress_kernel<I: Bits, R: Bits>(args: &DataChunk, result: &mut Vector) -> Result<()> {
    let min = constant_min::<R>(args).to_bits();
    rustdb_types::kernels::unary::<I, R>(&args.columns()[0], args.len(), result, |input| {
        R::from_bits(min.wrapping_add(input.to_bits()))
    });
    Ok(())
}

type Kernel = fn(&DataChunk, &mut Vector) -> Result<()>;

/// The compress kernel from `I` to the unsigned type of `result`.
fn compress_for<I: Bits>(result: PhysicalType) -> Option<Kernel> {
    Some(match result {
        PhysicalType::UInt8 => compress_kernel::<I, u8>,
        PhysicalType::UInt16 => compress_kernel::<I, u16>,
        PhysicalType::UInt32 => compress_kernel::<I, u32>,
        PhysicalType::UInt64 => compress_kernel::<I, u64>,
        _ => return None,
    })
}

fn compress_dispatch(input: PhysicalType, result: PhysicalType) -> Option<Kernel> {
    match input {
        PhysicalType::Int16 => compress_for::<i16>(result),
        PhysicalType::Int32 => compress_for::<i32>(result),
        PhysicalType::Int64 => compress_for::<i64>(result),
        PhysicalType::Int128 => compress_for::<i128>(result),
        PhysicalType::UInt16 => compress_for::<u16>(result),
        PhysicalType::UInt32 => compress_for::<u32>(result),
        PhysicalType::UInt64 => compress_for::<u64>(result),
        PhysicalType::UInt128 => compress_for::<u128>(result),
        _ => None,
    }
}

/// The decompress kernel from the unsigned `I` to `result`.
fn decompress_for<I: Bits>(result: PhysicalType) -> Option<Kernel> {
    Some(match result {
        PhysicalType::Int16 => decompress_kernel::<I, i16>,
        PhysicalType::Int32 => decompress_kernel::<I, i32>,
        PhysicalType::Int64 => decompress_kernel::<I, i64>,
        PhysicalType::Int128 => decompress_kernel::<I, i128>,
        PhysicalType::UInt16 => decompress_kernel::<I, u16>,
        PhysicalType::UInt32 => decompress_kernel::<I, u32>,
        PhysicalType::UInt64 => decompress_kernel::<I, u64>,
        PhysicalType::UInt128 => decompress_kernel::<I, u128>,
        _ => return None,
    })
}

fn decompress_dispatch(input: PhysicalType, result: PhysicalType) -> Option<Kernel> {
    match input {
        PhysicalType::UInt8 => decompress_for::<u8>(result),
        PhysicalType::UInt16 => decompress_for::<u16>(result),
        PhysicalType::UInt32 => decompress_for::<u32>(result),
        PhysicalType::UInt64 => decompress_for::<u64>(result),
        _ => None,
    }
}

fn type_name(ty: &LogicalType) -> String {
    ty.to_string().to_lowercase()
}

/// `__internal_compress_integral_<result>`.
fn integral_compress_set(result: &LogicalType) -> ScalarFunctionSet {
    let name = format!("__internal_compress_integral_{}", type_name(result));
    let mut set = ScalarFunctionSet::new(name.as_str());
    let inputs = [
        LogicalType::TINYINT,
        LogicalType::SMALLINT,
        LogicalType::INTEGER,
        LogicalType::BIGINT,
        LogicalType::HUGEINT,
        LogicalType::UTINYINT,
        LogicalType::USMALLINT,
        LogicalType::UINTEGER,
        LogicalType::UBIGINT,
        LogicalType::UHUGEINT,
    ];
    for input in inputs {
        if result.physical_type().size() >= input.physical_type().size() {
            continue;
        }
        let kernel = compress_dispatch(input.physical_type(), result.physical_type());
        let Some(kernel) = kernel else {
            continue;
        };
        set.add(
            ScalarFunction::new(
                name.as_str(),
                vec![input.clone(), input],
                result.clone(),
                move |args, _, out| kernel(args, out),
            )
            .with_bind(bind_internal),
        );
    }
    set
}

/// `__internal_decompress_integral_<result>`.
fn integral_decompress_set(result: &LogicalType) -> ScalarFunctionSet {
    let name = format!("__internal_decompress_integral_{}", type_name(result));
    let mut set = ScalarFunctionSet::new(name.as_str());
    for input in [
        LogicalType::UTINYINT,
        LogicalType::USMALLINT,
        LogicalType::UINTEGER,
        LogicalType::UBIGINT,
    ] {
        if result.physical_type().size() <= input.physical_type().size() {
            continue;
        }
        let kernel = decompress_dispatch(input.physical_type(), result.physical_type());
        let Some(kernel) = kernel else {
            continue;
        };
        set.add(
            ScalarFunction::new(
                name.as_str(),
                vec![input, result.clone()],
                result.clone(),
                move |args, _, out| kernel(args, out),
            )
            .with_bind(bind_internal),
        );
    }
    set
}

/// `StringCompress`: the string's bytes from the most significant byte down, its length
/// in the least significant byte (`MiniStringCompress` for one byte: length + first byte).
fn compress_string(input: &[u8], size: usize) -> u128 {
    if size == 1 {
        return input.first().map_or(0, |&first| 1 + u128::from(first));
    }
    // The first `size - 1` bytes, most significant first, above the length byte. The
    // strings are short: shifting them in avoids a `memcpy` call per row.
    let prefix = input.len().min(size - 1);
    let packed = input[..prefix]
        .iter()
        .fold(0u128, |packed, &byte| packed << 8 | u128::from(byte));
    // An empty prefix of a 16-byte result would shift by 128 bits.
    let aligned = packed.checked_shl(8 * (size - prefix) as u32).unwrap_or(0);
    aligned | u128::from(input.len() as u8)
}

/// [`compress_string`] of a `string_t`. An inlined string is packed from its 16 bytes
/// without looking at its characters one by one (`TemplatedReverseMemCpy`): byte-swapped,
/// its first byte is the most significant, and shifting it down by the bytes the result
/// lacks leaves the first `size - 1` bytes above an (empty) length byte.
#[inline]
fn compress_string_t(input: &StringT, size: usize) -> u128 {
    if size == 1 || !input.is_inlined() {
        // SAFETY: the kernel runs while the input vector, and so its string heap, lives.
        return compress_string(unsafe { input.as_bytes() }, size);
    }
    (input.inlined_bits().swap_bytes() >> (8 * (16 - size))) | input.len() as u128
}

/// `StringDecompress` (and `MiniStringDecompress`): the string packed into the `size`
/// low bytes of `value`. Shifted to the top and byte-swapped, its first byte is the
/// lowest, so a string that fits inline is taken from the integer as a whole
/// (`UncompressInlineString`); longer ones are copied into `heap`.
#[inline]
fn decompress_string(value: u128, size: usize, heap: &mut StringHeap) -> StringT {
    if size == 1 {
        return if value == 0 {
            StringT::inline(&[])
        } else {
            StringT::inline(&[(value - 1) as u8])
        };
    }
    let len = (value as u8 as usize).min(size - 1);
    let bytes = (value << (8 * (16 - size))).swap_bytes().to_le_bytes();
    heap.add(&bytes[..len])
}

fn string_compress_function(result: &LogicalType) -> ScalarFunctionSet {
    let name = format!("__internal_compress_string_{}", type_name(result));
    let size = result.physical_type().size();
    let physical = result.physical_type();
    let mut set = ScalarFunctionSet::new(name.as_str());
    set.add(
        ScalarFunction::new(
            name.as_str(),
            vec![LogicalType::VARCHAR],
            result.clone(),
            move |args, _, out| {
                macro_rules! run {
                    ($t:ty) => {{
                        rustdb_types::kernels::unary::<StringT, $t>(
                            &args.columns()[0],
                            args.len(),
                            out,
                            |input| <$t as Bits>::from_bits(compress_string_t(&input, size)),
                        );
                        Ok(())
                    }};
                }
                match physical {
                    PhysicalType::UInt8 => run!(u8),
                    PhysicalType::UInt16 => run!(u16),
                    PhysicalType::UInt32 => run!(u32),
                    PhysicalType::UInt64 => run!(u64),
                    PhysicalType::Int128 => run!(i128),
                    _ => run!(u128),
                }
            },
        )
        .with_bind(bind_internal),
    );
    set
}

fn string_decompress_set() -> ScalarFunctionSet {
    let name = "__internal_decompress_string";
    let mut set = ScalarFunctionSet::new(name);
    for input in [
        LogicalType::UTINYINT,
        LogicalType::USMALLINT,
        LogicalType::UINTEGER,
        LogicalType::UBIGINT,
        LogicalType::UHUGEINT,
        LogicalType::HUGEINT,
    ] {
        let size = input.physical_type().size();
        let physical = input.physical_type();
        set.add(
            ScalarFunction::new(name, vec![input], LogicalType::VARCHAR, move |args, _, out| {
                macro_rules! run {
                    ($t:ty) => {{
                        let mut heap = StringHeap::default();
                        rustdb_types::kernels::unary::<$t, StringT>(
                            &args.columns()[0],
                            args.len(),
                            out,
                            |value| decompress_string(value.to_bits(), size, &mut heap),
                        );
                        out.add_heap(heap);
                        Ok(())
                    }};
                }
                match physical {
                    PhysicalType::UInt8 => run!(u8),
                    PhysicalType::UInt16 => run!(u16),
                    PhysicalType::UInt32 => run!(u32),
                    PhysicalType::UInt64 => run!(u64),
                    PhysicalType::Int128 => run!(i128),
                    _ => run!(u128),
                }
            })
            .with_bind(bind_internal),
        );
    }
    set
}

/// Every compressed materialization function set.
pub fn compressed_materialization_functions() -> Vec<ScalarFunctionSet> {
    let mut sets = Vec::new();
    for result in [
        LogicalType::UTINYINT,
        LogicalType::USMALLINT,
        LogicalType::UINTEGER,
        LogicalType::UBIGINT,
    ] {
        sets.push(integral_compress_set(&result));
    }
    for result in [
        LogicalType::UTINYINT,
        LogicalType::USMALLINT,
        LogicalType::UINTEGER,
        LogicalType::UBIGINT,
        LogicalType::HUGEINT,
        LogicalType::UHUGEINT,
    ] {
        sets.push(string_compress_function(&result));
    }
    for result in [
        LogicalType::SMALLINT,
        LogicalType::INTEGER,
        LogicalType::BIGINT,
        LogicalType::HUGEINT,
        LogicalType::USMALLINT,
        LogicalType::UINTEGER,
        LogicalType::UBIGINT,
        LogicalType::UHUGEINT,
    ] {
        sets.push(integral_decompress_set(&result));
    }
    sets.push(string_decompress_set());
    sets
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compress_string_matches_the_byte_by_byte_layout() {
        for size in [2usize, 4, 8, 16] {
            for input in [&b""[..], b"a", b"ab", b"abcdefg", b"abcdefghijklmnopqrstu"] {
                if input.len() >= size {
                    continue;
                }
                let mut bytes = [0u8; 16];
                for (i, &byte) in input.iter().take(size).enumerate() {
                    bytes[size - 1 - i] = byte;
                }
                bytes[0] = input.len() as u8;
                assert_eq!(
                    compress_string(input, size),
                    u128::from_le_bytes(bytes),
                    "{size} {input:?}"
                );
            }
        }
    }

    #[test]
    fn inlined_strings_compress_like_their_bytes() {
        for size in [1usize, 2, 4, 8, 16] {
            for input in [
                &b""[..],
                b"a",
                b"ab",
                b"abcdefg",
                b"abcdefghijkl",
                b"abcdefghijklmno",
            ] {
                if input.len() >= size && size > 1 || size == 1 && input.len() > 1 {
                    continue;
                }
                // SAFETY: `input` outlives the value.
                let value = unsafe { StringT::from_raw(input) };
                assert_eq!(
                    compress_string_t(&value, size),
                    compress_string(input, size),
                    "{size} {input:?}"
                );
            }
        }
    }

    #[test]
    fn strings_round_trip_and_order() {
        let mut heap = StringHeap::default();
        for size in [1usize, 2, 4, 8, 16] {
            let mut previous = None;
            for text in ["", "a", "ab", "abc", "abcdefghijklm", "abcdefghijklmno", "b"] {
                if text.len() >= size && size > 1 || size == 1 && text.len() > 1 {
                    continue;
                }
                let compressed = compress_string(text.as_bytes(), size);
                let out = decompress_string(compressed, size, &mut heap);
                // SAFETY: `heap` holds the bytes of non-inlined results.
                assert_eq!(unsafe { out.as_bytes() }, text.as_bytes());
                if let Some(previous) = previous {
                    assert!(compressed > previous);
                }
                previous = Some(compressed);
            }
        }
    }
}
