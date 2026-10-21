//! Casts from BLOB, BIT and UUID (DuckDB's `blob_cast.cpp`, `bit_cast.cpp` and
//! `uuid_casts.cpp`).

use rustdb_common::Error;
use rustdb_types::bit::{self, BitNumeric};
use rustdb_types::{LogicalType, LogicalTypeId, NativeType, PhysicalType, Uuid, blob};

use super::BoundCastInfo;
use super::executor::{RowError, try_map_strings, try_map_strings_to_string, values_to_string};
use super::string::could_not_convert;

/// BIT -> number. A bit string that is too long for the target raises an error even
/// under `TRY_CAST`, like DuckDB.
fn bit_to_numeric<T: BitNumeric + NativeType>() -> BoundCastInfo {
    BoundCastInfo::new(|source, result, count, params| {
        try_map_strings::<T>(source, result, count, params, |bits| {
            bit::to_numeric::<T>(bits).map_err(|message| RowError::Fatal(Error::conversion(message)))
        })
    })
}

/// DuckDB's `BitCastSwitch`.
fn from_bit(target: &LogicalType) -> Option<BoundCastInfo> {
    use LogicalTypeId as L;
    Some(match target.id() {
        L::Boolean => bit_to_numeric::<bool>(),
        L::TinyInt => bit_to_numeric::<i8>(),
        L::SmallInt => bit_to_numeric::<i16>(),
        L::Integer => bit_to_numeric::<i32>(),
        L::BigInt => bit_to_numeric::<i64>(),
        L::HugeInt => bit_to_numeric::<i128>(),
        L::UTinyInt => bit_to_numeric::<u8>(),
        L::USmallInt => bit_to_numeric::<u16>(),
        L::UInteger => bit_to_numeric::<u32>(),
        L::UBigInt => bit_to_numeric::<u64>(),
        L::UHugeInt => bit_to_numeric::<u128>(),
        L::Float => bit_to_numeric::<f32>(),
        L::Double => bit_to_numeric::<f64>(),
        L::Blob => BoundCastInfo::new(|source, result, count, params| {
            try_map_strings_to_string(source, result, count, params, |bits, out| {
                bit::to_blob(bits, out);
                Ok(())
            })
        }),
        L::Varchar => BoundCastInfo::new(|source, result, count, params| {
            try_map_strings_to_string(source, result, count, params, |bits, out| {
                bit::write_text(bits, out);
                Ok(())
            })
        }),
        _ => return None,
    })
}

/// DuckDB's `BlobCastSwitch`.
fn from_blob(target: &LogicalType) -> Option<BoundCastInfo> {
    Some(match target.id() {
        LogicalTypeId::Varchar => BoundCastInfo::new(|source, result, count, params| {
            try_map_strings_to_string(source, result, count, params, |bytes, out| {
                blob::write_text(bytes, out);
                Ok(())
            })
        }),
        LogicalTypeId::Uuid => BoundCastInfo::new(|source, result, count, params| {
            try_map_strings::<Uuid>(source, result, count, params, |bytes| {
                Uuid::try_from_blob(bytes).ok_or_else(|| could_not_convert(bytes, PhysicalType::Int128))
            })
        }),
        // An empty BLOB raises an error even under `TRY_CAST`.
        LogicalTypeId::Bit => BoundCastInfo::new(|source, result, count, params| {
            try_map_strings_to_string(source, result, count, params, |bytes, out| {
                bit::from_blob(bytes, out).map_err(|message| RowError::Fatal(Error::conversion(message)))
            })
        }),
        // An exported aggregate state read back from its bytes.
        LogicalTypeId::AggregateState => BoundCastInfo::reinterpret(),
        _ => return None,
    })
}

/// DuckDB's `UUIDCastSwitch`.
fn from_uuid(target: &LogicalType) -> Option<BoundCastInfo> {
    Some(match target.id() {
        LogicalTypeId::Varchar => BoundCastInfo::new(|source, result, count, params| {
            values_to_string::<Uuid>(source, result, count, params, Uuid::write_to)
        }),
        LogicalTypeId::Blob => BoundCastInfo::new(|source, result, count, params| {
            values_to_string::<Uuid>(source, result, count, params, |uuid, out| {
                out.extend_from_slice(&uuid.to_blob());
            })
        }),
        _ => return None,
    })
}

/// Casts whose source is BLOB, BIT or UUID.
pub(super) fn cast_switch(source: &LogicalType, target: &LogicalType) -> Option<BoundCastInfo> {
    match source.id() {
        LogicalTypeId::Blob => from_blob(target),
        LogicalTypeId::Bit => from_bit(target),
        LogicalTypeId::Uuid => from_uuid(target),
        _ => None,
    }
}
