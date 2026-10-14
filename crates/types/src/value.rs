use std::fmt::Write as _;
use std::sync::Arc;

use rustdb_common::{Error, Result};

use crate::{LogicalType, LogicalTypeId, PhysicalType, StringT, Vector};

/// The storage of a non-NULL value, keyed by physical representation.
#[derive(Clone, Debug, PartialEq)]
enum Payload {
    Null,
    Bool(bool),
    I8(i8),
    I16(i16),
    I32(i32),
    I64(i64),
    I128(i128),
    U8(u8),
    U16(u16),
    U32(u32),
    U64(u64),
    U128(u128),
    F32(f32),
    F64(f64),
    Bytes(Arc<[u8]>),
}

/// A single typed SQL value (DuckDB's `Value`).
#[derive(Clone, Debug, PartialEq)]
pub struct Value {
    ty: LogicalType,
    payload: Payload,
}

impl Value {
    pub fn null(ty: LogicalType) -> Self {
        Self {
            ty,
            payload: Payload::Null,
        }
    }

    pub fn boolean(value: bool) -> Self {
        Self {
            ty: LogicalType::BOOLEAN,
            payload: Payload::Bool(value),
        }
    }

    pub fn tinyint(value: i8) -> Self {
        Self {
            ty: LogicalType::TINYINT,
            payload: Payload::I8(value),
        }
    }

    pub fn smallint(value: i16) -> Self {
        Self {
            ty: LogicalType::SMALLINT,
            payload: Payload::I16(value),
        }
    }

    pub fn integer(value: i32) -> Self {
        Self {
            ty: LogicalType::INTEGER,
            payload: Payload::I32(value),
        }
    }

    pub fn bigint(value: i64) -> Self {
        Self {
            ty: LogicalType::BIGINT,
            payload: Payload::I64(value),
        }
    }

    pub fn hugeint(value: i128) -> Self {
        Self {
            ty: LogicalType::HUGEINT,
            payload: Payload::I128(value),
        }
    }

    pub fn utinyint(value: u8) -> Self {
        Self {
            ty: LogicalType::UTINYINT,
            payload: Payload::U8(value),
        }
    }

    pub fn usmallint(value: u16) -> Self {
        Self {
            ty: LogicalType::USMALLINT,
            payload: Payload::U16(value),
        }
    }

    pub fn uinteger(value: u32) -> Self {
        Self {
            ty: LogicalType::UINTEGER,
            payload: Payload::U32(value),
        }
    }

    pub fn ubigint(value: u64) -> Self {
        Self {
            ty: LogicalType::UBIGINT,
            payload: Payload::U64(value),
        }
    }

    pub fn uhugeint(value: u128) -> Self {
        Self {
            ty: LogicalType::UHUGEINT,
            payload: Payload::U128(value),
        }
    }

    pub fn float(value: f32) -> Self {
        Self {
            ty: LogicalType::FLOAT,
            payload: Payload::F32(value),
        }
    }

    pub fn double(value: f64) -> Self {
        Self {
            ty: LogicalType::DOUBLE,
            payload: Payload::F64(value),
        }
    }

    pub fn varchar(value: impl AsRef<str>) -> Self {
        Self {
            ty: LogicalType::VARCHAR,
            payload: Payload::Bytes(Arc::from(value.as_ref().as_bytes())),
        }
    }

    pub fn blob(value: &[u8]) -> Self {
        Self {
            ty: LogicalType::BLOB,
            payload: Payload::Bytes(Arc::from(value)),
        }
    }

    /// A DECIMAL(width, scale) holding `unscaled / 10^scale`.
    pub fn decimal(unscaled: i128, width: u8, scale: u8) -> Self {
        let ty = LogicalType::decimal(width, scale);
        let payload = match ty.physical_type() {
            PhysicalType::Int16 => Payload::I16(unscaled as i16),
            PhysicalType::Int32 => Payload::I32(unscaled as i32),
            PhysicalType::Int64 => Payload::I64(unscaled as i64),
            _ => Payload::I128(unscaled),
        };
        Self { ty, payload }
    }

    pub fn logical_type(&self) -> &LogicalType {
        &self.ty
    }

    pub fn is_null(&self) -> bool {
        matches!(self.payload, Payload::Null)
    }

    /// The value as a signed 128-bit integer, for any integral or decimal (unscaled) value.
    pub fn as_i128(&self) -> Option<i128> {
        match self.payload {
            Payload::I8(v) => Some(v.into()),
            Payload::I16(v) => Some(v.into()),
            Payload::I32(v) => Some(v.into()),
            Payload::I64(v) => Some(v.into()),
            Payload::I128(v) => Some(v),
            Payload::U8(v) => Some(v.into()),
            Payload::U16(v) => Some(v.into()),
            Payload::U32(v) => Some(v.into()),
            Payload::U64(v) => Some(v.into()),
            Payload::U128(v) => i128::try_from(v).ok(),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self.payload {
            Payload::Bool(v) => Some(v),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self.payload {
            Payload::F32(v) => Some(v.into()),
            Payload::F64(v) => Some(v),
            _ => None,
        }
    }

    /// String/blob bytes.
    pub fn as_bytes(&self) -> Option<&[u8]> {
        match &self.payload {
            Payload::Bytes(bytes) => Some(bytes),
            _ => None,
        }
    }

    /// A value of integral type `ty` holding `value` (wrapping like a C cast), as produced
    /// by sequence vectors.
    pub(crate) fn from_i64_as(ty: &LogicalType, value: i64) -> Self {
        let payload = match ty.physical_type() {
            PhysicalType::Int8 => Payload::I8(value as i8),
            PhysicalType::Int16 => Payload::I16(value as i16),
            PhysicalType::Int32 => Payload::I32(value as i32),
            PhysicalType::UInt8 => Payload::U8(value as u8),
            PhysicalType::UInt16 => Payload::U16(value as u16),
            PhysicalType::UInt32 => Payload::U32(value as u32),
            PhysicalType::UInt64 => Payload::U64(value as u64),
            PhysicalType::Int128 => Payload::I128(value.into()),
            PhysicalType::UInt128 => Payload::U128(value as u128),
            _ => Payload::I64(value),
        };
        Self {
            ty: ty.clone(),
            payload,
        }
    }

    pub(crate) fn from_vector_slot(vector: &Vector, slot: usize) -> Self {
        let ty = vector.logical_type().clone();
        let payload = match ty.physical_type() {
            PhysicalType::Bool => Payload::Bool(vector.values::<bool>()[slot]),
            PhysicalType::Int8 => Payload::I8(vector.values::<i8>()[slot]),
            PhysicalType::Int16 => Payload::I16(vector.values::<i16>()[slot]),
            PhysicalType::Int32 => Payload::I32(vector.values::<i32>()[slot]),
            PhysicalType::Int64 => Payload::I64(vector.values::<i64>()[slot]),
            PhysicalType::Int128 => Payload::I128(vector.values::<i128>()[slot]),
            PhysicalType::UInt8 => Payload::U8(vector.values::<u8>()[slot]),
            PhysicalType::UInt16 => Payload::U16(vector.values::<u16>()[slot]),
            PhysicalType::UInt32 => Payload::U32(vector.values::<u32>()[slot]),
            PhysicalType::UInt64 => Payload::U64(vector.values::<u64>()[slot]),
            PhysicalType::UInt128 => Payload::U128(vector.values::<u128>()[slot]),
            PhysicalType::Float => Payload::F32(vector.values::<f32>()[slot]),
            PhysicalType::Double => Payload::F64(vector.values::<f64>()[slot]),
            PhysicalType::Varchar => {
                let value = &vector.values::<StringT>()[slot];
                Payload::Bytes(Arc::from(vector.string_value(value)))
            }
            other => unreachable!("vectors of physical type {other:?} are not constructed yet"),
        };
        Self { ty, payload }
    }

    pub(crate) fn write_to_vector(&self, vector: &mut Vector, slot: usize) -> Result<()> {
        if vector.logical_type().physical_type() != self.ty.physical_type() {
            return Err(Error::internal(format!(
                "Cannot write value of type {} into vector of type {}",
                self.ty,
                vector.logical_type()
            )));
        }
        match &self.payload {
            Payload::Null => unreachable!("NULL is handled by Vector::set_value"),
            Payload::Bool(v) => vector.values_mut::<bool>()[slot] = *v,
            Payload::I8(v) => vector.values_mut::<i8>()[slot] = *v,
            Payload::I16(v) => vector.values_mut::<i16>()[slot] = *v,
            Payload::I32(v) => vector.values_mut::<i32>()[slot] = *v,
            Payload::I64(v) => vector.values_mut::<i64>()[slot] = *v,
            Payload::I128(v) => vector.values_mut::<i128>()[slot] = *v,
            Payload::U8(v) => vector.values_mut::<u8>()[slot] = *v,
            Payload::U16(v) => vector.values_mut::<u16>()[slot] = *v,
            Payload::U32(v) => vector.values_mut::<u32>()[slot] = *v,
            Payload::U64(v) => vector.values_mut::<u64>()[slot] = *v,
            Payload::U128(v) => vector.values_mut::<u128>()[slot] = *v,
            Payload::F32(v) => vector.values_mut::<f32>()[slot] = *v,
            Payload::F64(v) => vector.values_mut::<f64>()[slot] = *v,
            Payload::Bytes(bytes) => {
                let string: StringT = vector.add_string(bytes);
                vector.values_mut::<StringT>()[slot] = string;
            }
        }
        Ok(())
    }

    /// DuckDB's `Value::ToString()`: the value cast to VARCHAR (`"NULL"` for NULL).
    pub fn to_varchar(&self) -> Result<String> {
        use LogicalTypeId as L;
        if self.is_null() {
            return Ok("NULL".to_owned());
        }
        let text = match (&self.payload, self.ty.id()) {
            (Payload::Bool(v), _) => if *v { "true" } else { "false" }.to_owned(),
            (_, L::Decimal) => {
                let (_, scale) = self.ty.decimal_properties().unwrap_or((18, 3));
                format_decimal(self.as_i128().unwrap_or_default(), scale)
            }
            (Payload::I8(v), L::TinyInt) => v.to_string(),
            (Payload::I16(v), L::SmallInt) => v.to_string(),
            (Payload::I32(v), L::Integer) => v.to_string(),
            (Payload::I64(v), L::BigInt) => v.to_string(),
            (Payload::I128(v), L::HugeInt) => v.to_string(),
            (Payload::U8(v), L::UTinyInt) => v.to_string(),
            (Payload::U16(v), L::USmallInt) => v.to_string(),
            (Payload::U32(v), L::UInteger) => v.to_string(),
            (Payload::U64(v), L::UBigInt) => v.to_string(),
            (Payload::U128(v), L::UHugeInt) => v.to_string(),
            (Payload::F32(v), L::Float) => format_float(*v),
            (Payload::F64(v), L::Double) => format_double(*v),
            (Payload::Bytes(bytes), L::Varchar | L::Char) => String::from_utf8_lossy(bytes).into_owned(),
            _ => {
                return Err(Error::not_implemented(format!(
                    "Unimplemented type for cast ({} -> VARCHAR)",
                    self.ty
                )));
            }
        };
        Ok(text)
    }
}

/// Formats an unscaled decimal (DuckDB's `DecimalToString`).
fn format_decimal(unscaled: i128, scale: u8) -> String {
    let negative = unscaled < 0;
    let magnitude = unscaled.unsigned_abs();
    let mut out = String::new();
    if negative {
        out.push('-');
    }
    if scale == 0 {
        let _ = write!(out, "{magnitude}");
        return out;
    }
    let divisor = 10u128.pow(u32::from(scale));
    let _ = write!(
        out,
        "{}.{:0width$}",
        magnitude / divisor,
        magnitude % divisor,
        width = scale as usize
    );
    out
}

/// Formats an `f64` exactly like `duckdb_fmt::format("{}", v)` (fmt 6.1.2).
pub(crate) fn format_double(value: f64) -> String {
    if value.is_nan() {
        return "nan".to_owned();
    }
    if value.is_infinite() {
        return if value < 0.0 { "-inf" } else { "inf" }.to_owned();
    }
    prettify_general(&format!("{value:e}"))
}

/// Formats an `f32` exactly like `duckdb_fmt::format("{}", v)` (fmt 6.1.2).
pub(crate) fn format_float(value: f32) -> String {
    if value.is_nan() {
        return "nan".to_owned();
    }
    if value.is_infinite() {
        return if value < 0.0 { "-inf" } else { "inf" }.to_owned();
    }
    prettify_general(&format!("{value:e}"))
}

/// Re-renders Rust's shortest round-trip `{:e}` output (`-d.ddde[-]x`) using fmt's
/// `general` presentation with `trailing_zeros` set, as fmt does for an empty format spec.
fn prettify_general(scientific: &str) -> String {
    let (negative, body) = match scientific.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, scientific),
    };
    let (mantissa, exponent) = body.split_once('e').unwrap_or((body, "0"));
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let num_digits = digits.len() as i32;

    let mut out = String::with_capacity(24);
    if negative {
        out.push('-');
    }
    if !(-4..16).contains(&exponent) {
        out.push_str(&digits[..1]);
        if num_digits > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        out.push('e');
        out.push(if exponent < 0 { '-' } else { '+' });
        let _ = write!(out, "{:02}", exponent.unsigned_abs());
        return out;
    }
    let full_exp = exponent + 1;
    if num_digits <= full_exp {
        out.push_str(&digits);
        out.extend(std::iter::repeat_n('0', (full_exp - num_digits) as usize));
        out.push_str(".0");
    } else if full_exp > 0 {
        out.push_str(&digits[..full_exp as usize]);
        out.push('.');
        out.push_str(&digits[full_exp as usize..]);
    } else {
        out.push_str("0.");
        out.extend(std::iter::repeat_n('0', (-full_exp) as usize));
        out.push_str(&digits);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doubles_format_like_fmt() {
        let cases = [
            (1.0, "1.0"),
            (0.1, "0.1"),
            (-0.0, "-0.0"),
            (0.0, "0.0"),
            (1e20, "1e+20"),
            (1e15, "1000000000000000.0"),
            (1e16, "1e+16"),
            (123.456, "123.456"),
            (1.0 / 3.0, "0.3333333333333333"),
            (0.0001, "0.0001"),
            (0.00001, "1e-05"),
            (1.5e-300, "1.5e-300"),
            (f64::NAN, "nan"),
            (f64::NEG_INFINITY, "-inf"),
        ];
        for (input, expected) in cases {
            assert_eq!(format_double(input), expected, "formatting {input:e}");
        }
        assert_eq!(format_float(0.1), "0.1");
    }

    #[test]
    fn decimals_format() {
        assert_eq!(format_decimal(15, 1), "1.5");
        assert_eq!(format_decimal(-5, 1), "-0.5");
        assert_eq!(format_decimal(-5, 0), "-5");
        assert_eq!(format_decimal(1234, 3), "1.234");
    }

    #[test]
    fn strings_round_trip_through_vectors() {
        let long = Value::varchar("a string longer than twelve bytes");
        let mut vector = Vector::flat(LogicalType::VARCHAR, 4);
        vector.set_value(1, &long).unwrap();
        vector.set_value(2, &Value::varchar("short")).unwrap();
        vector.set_value(3, &Value::null(LogicalType::VARCHAR)).unwrap();
        let copy = vector.clone();
        drop(vector);
        assert_eq!(copy.get_value(1), long);
        assert_eq!(copy.get_value(2).to_varchar().unwrap(), "short");
        assert!(copy.get_value(3).is_null());
    }
}
