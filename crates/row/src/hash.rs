//! DuckDB's hash function (`common/types/hash.cpp`, `VectorOperations::Hash` and
//! `CombineHash`), bit-for-bit: hashes decide partitions, and SQL `hash()` exposes them.

use rustdb_common::{Error, Result};
use rustdb_types::{LogicalType, PhysicalType, Vector, VectorKind};

use crate::compare::Interval;
use crate::format::{FormatKind, VectorFormat};

/// Hash of NULL.
pub const NULL_HASH: u64 = 0xbf58_476d_1ce4_e5b9;
const MULTIPLIER: u64 = 0xd6e8_feb8_6659_fd93;

/// Avalanche mix of a 64-bit value (`MurmurHash64`).
#[inline]
pub const fn murmur_hash64(mut x: u64) -> u64 {
    x ^= x >> 32;
    x = x.wrapping_mul(MULTIPLIER);
    x ^= x >> 32;
    x = x.wrapping_mul(MULTIPLIER);
    x ^ (x >> 32)
}

/// Combines a running hash with the next one (`CombineHashScalar`).
#[inline]
pub const fn combine_hash(mut a: u64, b: u64) -> u64 {
    a ^= a >> 32;
    a = a.wrapping_mul(MULTIPLIER);
    a ^ b
}

/// Hash of a byte string (`Hash(const char *, size_t)`, also used for VARCHAR/BLOB).
pub fn hash_bytes(bytes: &[u8]) -> u64 {
    let mut h = 0xe17a_1465 ^ (bytes.len() as u64).wrapping_mul(0xc6a4_a793_5bd1_e995);
    let (chunks, remainder) = bytes.as_chunks::<8>();
    for &block in chunks {
        h ^= u64::from_le_bytes(block);
        h = h.wrapping_mul(MULTIPLIER);
    }
    if !remainder.is_empty() {
        let mut block = [0u8; 8];
        block[..remainder.len()].copy_from_slice(remainder);
        h ^= u64::from_le_bytes(block);
        h = h.wrapping_mul(MULTIPLIER);
    }
    murmur_hash64(h)
}

/// Hash of a fixed-width value of physical type `physical` stored in `bytes`.
pub fn hash_fixed(physical: PhysicalType, bytes: &[u8]) -> u64 {
    macro_rules! read {
        ($ty:ty) => {{
            let mut buffer = [0u8; size_of::<$ty>()];
            buffer.copy_from_slice(&bytes[..size_of::<$ty>()]);
            <$ty>::from_ne_bytes(buffer)
        }};
    }
    match physical {
        // Small integers are hashed as their (sign-extended) 32-bit value.
        PhysicalType::Bool | PhysicalType::Int8 => murmur_hash64(u64::from(i32::from(read!(i8)) as u32)),
        PhysicalType::UInt8 => murmur_hash64(u64::from(read!(u8))),
        PhysicalType::Int16 => murmur_hash64(u64::from(i32::from(read!(i16)) as u32)),
        PhysicalType::UInt16 => murmur_hash64(u64::from(read!(u16))),
        PhysicalType::Int32 => murmur_hash64(u64::from(read!(i32) as u32)),
        PhysicalType::UInt32 => murmur_hash64(u64::from(read!(u32))),
        PhysicalType::Int64 => murmur_hash64(read!(i64) as u64),
        PhysicalType::UInt64 => murmur_hash64(read!(u64)),
        PhysicalType::Int128 => {
            let value = read!(i128);
            murmur_hash64(value as u64) ^ murmur_hash64((value >> 64) as u64)
        }
        PhysicalType::UInt128 => {
            let value = read!(u128);
            murmur_hash64(value as u64) ^ murmur_hash64((value >> 64) as u64)
        }
        PhysicalType::Float => {
            // -0.0 hashes like 0.0 and every NaN like the canonical quiet NaN.
            let value = read!(f32);
            let bits = if value == 0.0 {
                0
            } else if value.is_nan() {
                0x7fc0_0000
            } else {
                value.to_bits()
            };
            murmur_hash64(u64::from(bits))
        }
        PhysicalType::Double => {
            let value = read!(f64);
            let bits = if value == 0.0 {
                0
            } else if value.is_nan() {
                0x7ff8_0000_0000_0000
            } else {
                value.to_bits()
            };
            murmur_hash64(bits)
        }
        PhysicalType::Interval => {
            let (months, days, micros) = Interval::from_bytes(bytes).normalize();
            murmur_hash64(days as u64) ^ murmur_hash64(months as u64) ^ murmur_hash64(micros as u64)
        }
        _ => hash_bytes(bytes),
    }
}

/// Hash of row `row` of a formatted vector (`HashOp`), recursing into nested types.
fn hash_row(format: &VectorFormat<'_>, row: usize) -> u64 {
    let slot = format.slot(row);
    match format.kind {
        FormatKind::Fixed(width) => {
            if format.slot_is_valid(slot) {
                hash_fixed(
                    format.vector.logical_type().physical_type(),
                    format.slot_bytes(slot, width),
                )
            } else {
                NULL_HASH
            }
        }
        FormatKind::Varchar => {
            if format.slot_is_valid(slot) {
                hash_bytes(format.string_bytes(slot))
            } else {
                NULL_HASH
            }
        }
        // Structs hash their fields only; the struct's own validity is ignored.
        FormatKind::Struct => {
            let mut children = format.children.iter();
            let first = children.next().map_or(NULL_HASH, |child| hash_row(child, row));
            children.fold(first, |hash, child| combine_hash(hash, hash_row(child, row)))
        }
        FormatKind::List => {
            if !format.slot_is_valid(slot) {
                return NULL_HASH;
            }
            let (offset, length) = format.list_entry(slot);
            if length == 0 {
                return NULL_HASH;
            }
            let child = &format.children[0];
            let first = hash_row(child, offset);
            (offset + 1..offset + length).fold(first, |hash, element| {
                combine_hash(hash, hash_row(child, element))
            })
        }
        FormatKind::Array(_) => {
            if !format.slot_is_valid(slot) {
                return NULL_HASH;
            }
            let (offset, length) = format.list_entry(slot);
            let child = &format.children[0];
            (offset..offset + length).fold(0, |hash, element| combine_hash(hash, hash_row(child, element)))
        }
    }
}

fn check_result(result: &Vector) -> Result<()> {
    if result.logical_type().physical_type() == PhysicalType::UInt64 {
        Ok(())
    } else {
        Err(Error::internal(format!(
            "hash result vector has type {}",
            result.logical_type()
        )))
    }
}

/// Hashes the first `count` rows of `input` into `result` (`VectorOperations::Hash`).
/// A constant input yields a constant result.
pub fn hash_vector(input: &Vector, count: usize, result: &mut Vector) -> Result<()> {
    check_result(result)?;
    let format = VectorFormat::new(input, count)?;
    if input.kind() == VectorKind::Constant {
        *result = Vector::flat(result.logical_type().clone(), 1);
        result.values_mut::<u64>()[0] = hash_row(&format, 0);
        result.make_constant();
        return Ok(());
    }
    *result = Vector::flat(result.logical_type().clone(), count.max(1));
    let hashes = result.values_mut::<u64>();
    if let (FormatKind::Fixed(8), true, true) = (format.kind, format.sel.is_identity(), format.all_valid())
        && matches!(
            input.logical_type().physical_type(),
            PhysicalType::Int64 | PhysicalType::UInt64
        )
    {
        // Tight loop for the most common key type.
        for (row, hash) in hashes[..count].iter_mut().enumerate() {
            let mut block = [0u8; 8];
            block.copy_from_slice(format.slot_bytes(row, 8));
            *hash = murmur_hash64(u64::from_ne_bytes(block));
        }
        return Ok(());
    }
    for (row, hash) in hashes[..count].iter_mut().enumerate() {
        *hash = hash_row(&format, row);
    }
    Ok(())
}

/// Combines the hashes of `input` into `hashes` (`VectorOperations::CombineHash`).
pub fn combine_hash_vector(hashes: &mut Vector, input: &Vector, count: usize) -> Result<()> {
    check_result(hashes)?;
    let format = VectorFormat::new(input, count)?;
    if hashes.kind() == VectorKind::Constant && input.kind() == VectorKind::Constant {
        let current = hashes.unified::<u64>().values[0];
        let mut constant = Vector::flat(hashes.logical_type().clone(), 1);
        constant.values_mut::<u64>()[0] = combine_hash(current, hash_row(&format, 0));
        constant.make_constant();
        *hashes = constant;
        return Ok(());
    }
    let current: Vec<u64> = {
        let unified = hashes.unified::<u64>();
        (0..count).map(|row| unified.values[unified.slot(row)]).collect()
    };
    let mut combined = Vector::flat(hashes.logical_type().clone(), count.max(1));
    for (row, (hash, current)) in combined.values_mut::<u64>()[..count]
        .iter_mut()
        .zip(current)
        .enumerate()
    {
        *hash = combine_hash(current, hash_row(&format, row));
    }
    *hashes = combined;
    Ok(())
}

/// Hashes several columns into one hash per row (first column hashed, the rest combined),
/// as DuckDB does for multi-column keys.
pub fn hash_columns(columns: &[&Vector], count: usize) -> Result<Vector> {
    let mut hashes = Vector::flat(LogicalType::UBIGINT, count.max(1));
    let Some((first, rest)) = columns.split_first() else {
        return Ok(hashes);
    };
    hash_vector(first, count, &mut hashes)?;
    for column in rest {
        combine_hash_vector(&mut hashes, column, count)?;
    }
    Ok(hashes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustdb_types::{ListEntry, LogicalTypeId, TypeInfo, Value};

    fn hash_one(vector: &Vector) -> u64 {
        let mut result = Vector::flat(LogicalType::UBIGINT, 1);
        hash_vector(vector, 1, &mut result).unwrap();
        result.unified::<u64>().get(0).unwrap()
    }

    fn single(value: &Value) -> Vector {
        let mut vector = Vector::flat(value.logical_type().clone(), 1);
        vector.set_value(0, value).unwrap();
        vector
    }

    /// Expected values from `SELECT hash(...)` in DuckDB 1.5.6.
    #[test]
    fn matches_duckdb_hash() {
        assert_eq!(hash_one(&single(&Value::tinyint(1))), 4_717_996_019_076_358_352);
        assert_eq!(hash_one(&single(&Value::tinyint(-1))), 4_739_667_815_145_166_545);
        assert_eq!(
            hash_one(&single(&Value::varchar("hello"))),
            8_628_213_525_773_890_682
        );
        assert_eq!(
            hash_one(&single(&Value::varchar("a long string that is not inlined"))),
            15_284_163_451_695_641_710
        );
        assert_eq!(
            hash_one(&single(&Value::null(LogicalType::INTEGER))),
            13_787_848_793_156_543_929
        );
        assert_eq!(hash_one(&single(&Value::double(1.5))), 1_706_605_666_616_485_939);

        let mut interval = Vector::flat(LogicalType::INTERVAL, 1);
        interval.data_bytes_mut()[4..8].copy_from_slice(&1i32.to_ne_bytes());
        assert_eq!(hash_one(&interval), 4_717_996_019_076_358_352);

        let mut list = Vector::flat(LogicalType::list(LogicalType::INTEGER), 1);
        let child = list.list_child_mut().unwrap();
        child.resize(2);
        child.values_mut::<i32>()[..2].copy_from_slice(&[1, 2]);
        list.set_list_size(2);
        list.values_mut::<ListEntry>()[0] = ListEntry { offset: 0, length: 2 };
        assert_eq!(hash_one(&list), 6_530_802_887_144_669_425);

        let struct_type = LogicalType::with_info(
            LogicalTypeId::Struct,
            TypeInfo::Struct(rustdb_types::StructTypeInfo {
                child_types: vec![
                    ("a".into(), LogicalType::INTEGER),
                    ("b".into(), LogicalType::VARCHAR),
                ],
            }),
        );
        let mut row = Vector::flat(struct_type, 1);
        let children = row.struct_children_mut().unwrap();
        children[0].set_value(0, &Value::integer(1)).unwrap();
        children[1].set_value(0, &Value::varchar("x")).unwrap();
        assert_eq!(hash_one(&row), 14_110_061_903_442_261_593);
    }

    #[test]
    fn constant_inputs_hash_to_constants() {
        let constant = Vector::constant(&Value::bigint(42)).unwrap();
        let mut result = Vector::flat(LogicalType::UBIGINT, 1);
        hash_vector(&constant, 100, &mut result).unwrap();
        assert_eq!(result.kind(), VectorKind::Constant);
        let flat = single(&Value::bigint(42));
        assert_eq!(result.unified::<u64>().get(57), Some(hash_one(&flat)));
        let other = Vector::constant(&Value::varchar("x")).unwrap();
        combine_hash_vector(&mut result, &other, 100).unwrap();
        assert_eq!(result.kind(), VectorKind::Constant);
    }
}
