//! Extreme values of types (DuckDB's `Value::MinimumValue`, `Value::MaximumValue`,
//! `Value::Infinity` and `Value::NegativeInfinity`).

use rustdb_common::{Error, Result};
use rustdb_types::{Date, LogicalType, LogicalTypeId, Time, TimeNs, TimeTz, Timestamp, Uuid, Value};

const MICROS_PER_DAY: i64 = 86_400_000_000;
const NANOS_PER_DAY: i64 = MICROS_PER_DAY * 1000;

fn invalid(type_name: &LogicalType, what: &str) -> Error {
    Error::new(
        rustdb_common::ErrorKind::InvalidType,
        format!("Invalid type [{type_name}]: {what}"),
    )
}

/// `10^width - 1`, the largest unscaled DECIMAL value of a width.
fn decimal_max(width: u8) -> i128 {
    10i128.pow(u32::from(width)) - 1
}

/// DuckDB's `Value::MaximumValue`.
pub(crate) fn maximum_value(ty: &LogicalType) -> Result<Value> {
    use LogicalTypeId as L;
    Ok(match ty.id() {
        L::Boolean => Value::boolean(true),
        L::TinyInt => Value::tinyint(i8::MAX),
        L::SmallInt => Value::smallint(i16::MAX),
        L::Integer | L::SqlNull => Value::integer(i32::MAX),
        L::BigInt => Value::bigint(i64::MAX),
        L::HugeInt => Value::hugeint(i128::MAX),
        L::UHugeInt => Value::uhugeint(u128::MAX),
        L::Uuid => Value::uuid(Uuid(i128::MAX)),
        L::UTinyInt => Value::utinyint(u8::MAX),
        L::USmallInt => Value::usmallint(u16::MAX),
        L::UInteger => Value::uinteger(u32::MAX),
        L::UBigInt => Value::ubigint(u64::MAX),
        L::Date => Value::date(Date::MAX),
        L::Time => Value::time(Time(MICROS_PER_DAY)),
        L::TimeNs => Value::time_ns(TimeNs(NANOS_PER_DAY)),
        L::Timestamp => Value::timestamp(Timestamp(i64::MAX - 1)),
        L::TimestampSec => Value::timestamp_s((i64::MAX - 1) / 1_000_000),
        L::TimestampMs => Value::timestamp_ms((i64::MAX - 1) / 1_000),
        L::TimestampNs => Value::timestamp_ns(i64::MAX - 1),
        L::TimestampTz => Value::timestamp_tz(Timestamp(i64::MAX - 1)),
        L::TimeTz => Value::time_tz(TimeTz::new(Time(MICROS_PER_DAY), TimeTz::MIN_OFFSET)),
        L::Float => Value::float(f32::MAX),
        L::Double => Value::double(f64::MAX),
        L::Decimal => {
            let (width, scale) = ty.decimal_properties().unwrap_or((18, 3));
            Value::decimal(decimal_max(width), width, scale)
        }
        L::Enum => {
            let size = ty.enum_values().map_or(0, <[String]>::len);
            Value::enum_value(ty.clone(), size.saturating_sub(1) as u32)
        }
        L::Bignum => Value::bytes(LogicalType::BIGNUM, &double_max_bignum(false)),
        _ => return Err(invalid(ty, "MaximumValue requires numeric type")),
    })
}

/// `±DBL_MAX` as a stored BIGNUM, DuckDB's default BIGNUM extremes: a 3-byte header
/// (the byte count with bit 23 set) and the big-endian magnitude, all bytes inverted for
/// negative values.
pub(crate) fn double_max_bignum(negative: bool) -> Vec<u8> {
    // DBL_MAX = 0xFFFFFFFFFFFFF8 * 2^968: 7 significant bytes and 121 zero bytes.
    let mut bytes = vec![0x80, 0x00, 0x80, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xF8];
    bytes.resize(3 + 128, 0);
    if negative {
        for byte in &mut bytes {
            *byte = !*byte;
        }
    }
    bytes
}

/// The smallest TIMESTAMP, 290309-12-22 (BC) 00:00:00 (DuckDB's `Timestamp::MIN_*`).
fn minimum_timestamp() -> Result<Timestamp> {
    Date::from_ymd(-290_308, 12, 22)
        .and_then(Timestamp::from_date)
        .ok_or_else(|| Error::internal("minimum timestamp"))
}

/// DuckDB's `Value::MinimumValue`.
pub(crate) fn minimum_value(ty: &LogicalType) -> Result<Value> {
    use LogicalTypeId as L;
    Ok(match ty.id() {
        L::Boolean => Value::boolean(false),
        L::TinyInt => Value::tinyint(i8::MIN),
        L::SmallInt => Value::smallint(i16::MIN),
        L::Integer | L::SqlNull => Value::integer(i32::MIN),
        L::BigInt => Value::bigint(i64::MIN),
        L::HugeInt => Value::hugeint(i128::MIN),
        L::UHugeInt => Value::uhugeint(0),
        L::Uuid => Value::uuid(Uuid(i128::MIN)),
        L::UTinyInt => Value::utinyint(0),
        L::USmallInt => Value::usmallint(0),
        L::UInteger => Value::uinteger(0),
        L::UBigInt => Value::ubigint(0),
        L::Date => Value::date(Date::MIN),
        L::Time => Value::time(Time(0)),
        L::TimeNs => Value::time_ns(TimeNs(0)),
        L::Timestamp => Value::timestamp(minimum_timestamp()?),
        L::TimestampSec => Value::timestamp_s(minimum_timestamp()?.0 / 1_000_000),
        L::TimestampMs => Value::timestamp_ms(minimum_timestamp()?.0 / 1_000),
        L::TimestampNs => Value::timestamp_ns(i64::MIN / NANOS_PER_DAY * NANOS_PER_DAY),
        L::TimeTz => Value::time_tz(TimeTz::new(Time(0), TimeTz::MAX_OFFSET)),
        L::TimestampTz => Value::timestamp_tz(minimum_timestamp()?),
        L::Float => Value::float(f32::MIN),
        L::Double => Value::double(f64::MIN),
        L::Decimal => {
            let (width, scale) = ty.decimal_properties().unwrap_or((18, 3));
            Value::decimal(-decimal_max(width), width, scale)
        }
        L::Enum => Value::enum_value(ty.clone(), 0),
        L::Bignum => Value::bytes(LogicalType::BIGNUM, &double_max_bignum(true)),
        _ => return Err(invalid(ty, "MinimumValue requires numeric type")),
    })
}

/// DuckDB's `Value::Infinity`.
pub(crate) fn infinity(ty: &LogicalType) -> Result<Value> {
    use LogicalTypeId as L;
    Ok(match ty.id() {
        L::Date => Value::date(Date::INFINITY),
        L::Timestamp => Value::timestamp(Timestamp::INFINITY),
        L::TimestampSec => Value::timestamp_s(Timestamp::INFINITY.0),
        L::TimestampMs => Value::timestamp_ms(Timestamp::INFINITY.0),
        L::TimestampNs => Value::timestamp_ns(Timestamp::INFINITY.0),
        L::TimestampTz => Value::timestamp_tz(Timestamp::INFINITY),
        L::Float => Value::float(f32::INFINITY),
        L::Double => Value::double(f64::INFINITY),
        _ => return Err(invalid(ty, "Infinity requires numeric type")),
    })
}
