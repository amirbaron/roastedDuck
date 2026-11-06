//! Casts from and to ENUM (DuckDB's `enum_casts.cpp` and `StringEnumCast`).

use std::collections::HashMap;
use std::sync::Arc;

use rustdb_common::{ByteString, Error, Result};
use rustdb_types::{LogicalType, LogicalTypeId, NativeType, PhysicalType, Vector};

use super::executor::{RowError, RowResult, addressable, cast_rows, try_map_strings};
use super::numeric::out_of_range_message;
use super::string::could_not_convert;
use super::{BoundCastInfo, CastFunctionSet};

/// The unsigned integer types ENUM values are stored as.
trait EnumIndex: NativeType + Copy + super::numeric::CastNumber {
    fn index(self) -> usize;
    fn from_index(index: usize) -> Self;
}

macro_rules! enum_index {
    ($($ty:ty),*) => {$(
        impl EnumIndex for $ty {
            #[inline]
            fn index(self) -> usize {
                self as usize
            }
            #[inline]
            fn from_index(index: usize) -> Self {
                index as $ty
            }
        }
    )*};
}
enum_index!(u8, u16, u32);

/// Calls `$body` with `$index` bound to the storage type of the ENUM `$ty`.
macro_rules! with_index {
    ($ty:expr, $index:ident => $body:expr) => {
        match $ty.physical_type() {
            PhysicalType::UInt8 => {
                type $index = u8;
                Some($body)
            }
            PhysicalType::UInt16 => {
                type $index = u16;
                Some($body)
            }
            PhysicalType::UInt32 => {
                type $index = u32;
                Some($body)
            }
            _ => None,
        }
    };
}

/// The dictionary position of every ENUM string (DuckDB's `EnumType::GetPos`).
fn positions(ty: &LogicalType) -> Arc<HashMap<Vec<u8>, usize>> {
    let values = ty.enum_values().unwrap_or_default();
    Arc::new(
        values
            .iter()
            .enumerate()
            .map(|(index, value)| (value.as_bytes().to_vec(), index))
            .collect(),
    )
}

/// VARCHAR -> ENUM: the dictionary position of the string.
pub(super) fn string_to_enum(target: &LogicalType) -> Option<BoundCastInfo> {
    let positions = positions(target);
    with_index!(target, Index => BoundCastInfo::new(move |source, result, count, params| {
        try_map_strings::<Index>(source, result, count, params, |text| {
            positions
                .get(text)
                .map(|&index| Index::from_index(index))
                .ok_or_else(|| could_not_convert(text, Index::PHYSICAL))
        })
    }))
}

fn enum_text(values: &[ByteString], index: usize) -> RowResult<&[u8]> {
    values
        .get(index)
        .map(ByteString::as_bytes)
        .ok_or_else(|| RowError::Fatal(Error::internal(format!("ENUM index {index} out of range"))))
}

/// ENUM -> VARCHAR: the dictionary string.
fn enum_to_varchar(source_type: &LogicalType) -> Option<BoundCastInfo> {
    let values: Arc<[ByteString]> = source_type.enum_values()?.into();
    with_index!(source_type, Index => BoundCastInfo::new(move |source, result, count, params| {
        let source = addressable(source, count);
        let indexes = source.values::<Index>();
        super::executor::cast_rows_to_string(&source, result, count, params, |slot, out| {
            out.extend_from_slice(enum_text(&values, indexes[slot].index())?);
            Ok(())
        })
    }))
}

/// ENUM -> ENUM: re-looks up every string in the target dictionary. Missing strings are an
/// error under `CAST` and NULL under `TRY_CAST`.
fn enum_to_enum(source_type: &LogicalType, target: &LogicalType) -> Option<BoundCastInfo> {
    let values: Arc<[ByteString]> = source_type.enum_values()?.into();
    let positions = positions(target);
    with_index!(source_type, Source => {
        with_index!(target, Target => {
            let values = Arc::clone(&values);
            let positions = Arc::clone(&positions);
            BoundCastInfo::new(move |source, result, count, params| {
                let source = addressable(source, count);
                let indexes = source.values::<Source>();
                // DuckDB's TRY_CAST silently yields NULL here, without recording an error.
                let try_cast = params.is_try_cast();
                cast_rows::<Target>(&source, result, count, params, |slot| {
                    let index = indexes[slot];
                    let text = enum_text(&values, index.index())?;
                    positions
                        .get(text)
                        .map(|&position| Target::from_index(position))
                        .ok_or_else(|| {
                            if try_cast {
                                RowError::Null
                            } else {
                                RowError::Invalid(out_of_range_message::<Source, Target>(index))
                            }
                        })
                })
            })
        })?
    })
}

/// ENUM -> anything else: through VARCHAR.
fn enum_to_any(
    set: &CastFunctionSet,
    source_type: &LogicalType,
    target: &LogicalType,
) -> Result<BoundCastInfo> {
    let to_varchar = set.get_cast_function(source_type, &LogicalType::VARCHAR)?;
    let from_varchar = set.get_cast_function(&LogicalType::VARCHAR, target)?;
    Ok(BoundCastInfo::new(move |source, result, count, params| {
        let mut text = Vector::flat(LogicalType::VARCHAR, count);
        to_varchar.execute(source, &mut text, count, params)?;
        from_varchar.execute(&text, result, count, params)?;
        Ok(true)
    }))
}

/// DuckDB's `EnumCastSwitch`: casts from ENUM.
pub(super) fn cast_switch(
    set: &CastFunctionSet,
    source: &LogicalType,
    target: &LogicalType,
) -> Result<Option<BoundCastInfo>> {
    match target.id() {
        LogicalTypeId::Enum => Ok(enum_to_enum(source, target)),
        LogicalTypeId::Varchar if target == &LogicalType::VARCHAR => Ok(enum_to_varchar(source)),
        _ => enum_to_any(set, source, target).map(Some),
    }
}
