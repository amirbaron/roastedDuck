mod serialization;
mod sql;

use std::cmp::Ordering;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use rustdb_common::{Error, Result};

use crate::format::{self, NestedQuoting};
use crate::{
    Date, Interval, LogicalType, LogicalTypeId, PhysicalType, StringT, Time, TimeNs, TimeTz, Timestamp, Uuid,
    Vector,
};

/// The storage of a non-NULL value, keyed by physical representation.
#[derive(Clone, Debug)]
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
    Interval(Interval),
    /// VARCHAR, BLOB, BIT, ... bytes.
    Bytes(Arc<[u8]>),
    /// The elements of a LIST, MAP or ARRAY, or the fields of a STRUCT or UNION.
    Nested(Arc<[Value]>),
    /// A value of type `TYPE`.
    Type(LogicalType),
}

impl Payload {
    /// Floats compare by bit pattern so that equality is reflexive (`NaN == NaN`), which
    /// makes values usable as map keys; SQL comparison semantics live in the executor.
    fn eq_bits(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::F32(a), Self::F32(b)) => a.to_bits() == b.to_bits(),
            (Self::F64(a), Self::F64(b)) => a.to_bits() == b.to_bits(),
            (Self::Null, Self::Null) => true,
            (Self::Bool(a), Self::Bool(b)) => a == b,
            (Self::I8(a), Self::I8(b)) => a == b,
            (Self::I16(a), Self::I16(b)) => a == b,
            (Self::I32(a), Self::I32(b)) => a == b,
            (Self::I64(a), Self::I64(b)) => a == b,
            (Self::I128(a), Self::I128(b)) => a == b,
            (Self::U8(a), Self::U8(b)) => a == b,
            (Self::U16(a), Self::U16(b)) => a == b,
            (Self::U32(a), Self::U32(b)) => a == b,
            (Self::U64(a), Self::U64(b)) => a == b,
            (Self::U128(a), Self::U128(b)) => a == b,
            (Self::Interval(a), Self::Interval(b)) => a == b,
            (Self::Bytes(a), Self::Bytes(b)) => a == b,
            (Self::Nested(a), Self::Nested(b)) => a == b,
            (Self::Type(a), Self::Type(b)) => a == b,
            _ => false,
        }
    }
}

impl PartialEq for Payload {
    fn eq(&self, other: &Self) -> bool {
        self.eq_bits(other)
    }
}

impl Eq for Payload {}

impl Hash for Payload {
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::mem::discriminant(self).hash(state);
        match self {
            Self::Null => {}
            Self::Bool(v) => v.hash(state),
            Self::I8(v) => v.hash(state),
            Self::I16(v) => v.hash(state),
            Self::I32(v) => v.hash(state),
            Self::I64(v) => v.hash(state),
            Self::I128(v) => v.hash(state),
            Self::U8(v) => v.hash(state),
            Self::U16(v) => v.hash(state),
            Self::U32(v) => v.hash(state),
            Self::U64(v) => v.hash(state),
            Self::U128(v) => v.hash(state),
            Self::F32(v) => v.to_bits().hash(state),
            Self::F64(v) => v.to_bits().hash(state),
            Self::Interval(v) => v.hash(state),
            Self::Bytes(v) => v.hash(state),
            Self::Nested(v) => v.hash(state),
            Self::Type(v) => v.hash(state),
        }
    }
}

/// A single typed SQL value (DuckDB's `Value`).
///
/// Equality and hashing are structural (type and payload, floats by bit pattern), not
/// SQL comparison: see [`Value::compare_not_distinct`] and the cast layer's comparisons.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Value {
    ty: LogicalType,
    payload: Payload,
}

impl Default for Value {
    /// A NULL of type `"NULL"`, like DuckDB's default-constructed `Value`.
    fn default() -> Self {
        Self::null(LogicalType::SQLNULL)
    }
}

macro_rules! scalar_constructors {
    ($($(#[$doc:meta])* $name:ident($arg:ty) => $ty:expr, $variant:ident;)*) => {$(
        $(#[$doc])*
        pub fn $name(value: $arg) -> Self {
            Self { ty: $ty, payload: Payload::$variant(value.into()) }
        }
    )*};
}

macro_rules! payload_accessors {
    ($($(#[$doc:meta])* $name:ident -> $ty:ty, $variant:ident;)*) => {$(
        $(#[$doc])*
        pub fn $name(&self) -> Option<$ty> {
            match self.payload {
                Payload::$variant(v) => Some(v),
                _ => None,
            }
        }
    )*};
}

impl Value {
    /// A NULL of type `ty`.
    pub fn null(ty: LogicalType) -> Self {
        Self {
            ty,
            payload: Payload::Null,
        }
    }

    scalar_constructors! {
        boolean(bool) => LogicalType::BOOLEAN, Bool;
        tinyint(i8) => LogicalType::TINYINT, I8;
        smallint(i16) => LogicalType::SMALLINT, I16;
        integer(i32) => LogicalType::INTEGER, I32;
        bigint(i64) => LogicalType::BIGINT, I64;
        hugeint(i128) => LogicalType::HUGEINT, I128;
        utinyint(u8) => LogicalType::UTINYINT, U8;
        usmallint(u16) => LogicalType::USMALLINT, U16;
        uinteger(u32) => LogicalType::UINTEGER, U32;
        ubigint(u64) => LogicalType::UBIGINT, U64;
        uhugeint(u128) => LogicalType::UHUGEINT, U128;
        float(f32) => LogicalType::FLOAT, F32;
        double(f64) => LogicalType::DOUBLE, F64;
        interval(Interval) => LogicalType::INTERVAL, Interval;
        /// `TIMESTAMP_S`: seconds since the epoch.
        timestamp_s(i64) => LogicalType::TIMESTAMP_S, I64;
        /// `TIMESTAMP_MS`: milliseconds since the epoch.
        timestamp_ms(i64) => LogicalType::TIMESTAMP_MS, I64;
        /// `TIMESTAMP_NS`: nanoseconds since the epoch.
        timestamp_ns(i64) => LogicalType::TIMESTAMP_NS, I64;
    }

    pub fn date(value: Date) -> Self {
        Self {
            ty: LogicalType::DATE,
            payload: Payload::I32(value.0),
        }
    }

    pub fn time(value: Time) -> Self {
        Self {
            ty: LogicalType::TIME,
            payload: Payload::I64(value.0),
        }
    }

    pub fn time_ns(value: TimeNs) -> Self {
        Self {
            ty: LogicalType::TIME_NS,
            payload: Payload::I64(value.0),
        }
    }

    pub fn time_tz(value: TimeTz) -> Self {
        Self {
            ty: LogicalType::TIME_TZ,
            payload: Payload::I64(value.0 as i64),
        }
    }

    pub fn timestamp(value: Timestamp) -> Self {
        Self {
            ty: LogicalType::TIMESTAMP,
            payload: Payload::I64(value.0),
        }
    }

    /// `TIMESTAMP WITH TIME ZONE` (microseconds since the epoch, UTC).
    pub fn timestamp_tz(value: Timestamp) -> Self {
        Self {
            ty: LogicalType::TIMESTAMP_TZ,
            payload: Payload::I64(value.0),
        }
    }

    pub fn uuid(value: Uuid) -> Self {
        Self {
            ty: LogicalType::UUID,
            payload: Payload::I128(value.0),
        }
    }

    pub fn varchar(value: impl AsRef<str>) -> Self {
        Self::bytes(LogicalType::VARCHAR, value.as_ref().as_bytes())
    }

    pub fn blob(value: &[u8]) -> Self {
        Self::bytes(LogicalType::BLOB, value)
    }

    /// A value of a string-like type (VARCHAR, BLOB, BIT, ...) from its raw bytes.
    pub fn bytes(ty: LogicalType, value: &[u8]) -> Self {
        Self {
            ty,
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

    /// The ENUM value at dictionary position `index` of `ty`.
    pub fn enum_value(ty: LogicalType, index: u32) -> Self {
        let payload = match ty.physical_type() {
            PhysicalType::UInt8 => Payload::U8(index as u8),
            PhysicalType::UInt16 => Payload::U16(index as u16),
            _ => Payload::U32(index),
        };
        Self { ty, payload }
    }

    /// A LIST of `child_type` holding `values` (DuckDB's `Value::LIST`).
    pub fn list(child_type: LogicalType, values: Vec<Self>) -> Self {
        Self::from_children(LogicalType::list(child_type), values)
    }

    /// An ARRAY of `child_type` values (its size is the number of values).
    pub fn array(child_type: LogicalType, values: Vec<Self>) -> Self {
        let size = values.len() as u32;
        Self::from_children(LogicalType::array(child_type, size), values)
    }

    /// A LIST/MAP/ARRAY/STRUCT/UNION value of type `ty` from its elements or fields (MAP
    /// elements are `STRUCT(key, value)` values, UNION fields start with the tag); the
    /// children must match `ty`.
    pub fn from_children(ty: LogicalType, children: Vec<Self>) -> Self {
        Self {
            ty,
            payload: Payload::Nested(children.into()),
        }
    }

    /// A value of type `TYPE` (DuckDB's `Value::TYPE`).
    pub fn type_value(ty: LogicalType) -> Self {
        Self {
            ty: LogicalType::simple(LogicalTypeId::Type),
            payload: Payload::Type(ty),
        }
    }

    /// A STRUCT whose type is derived from its named fields.
    pub fn struct_value(fields: Vec<(String, Value)>) -> Self {
        let ty = LogicalType::struct_type(
            fields
                .iter()
                .map(|(name, value)| (name.clone(), value.ty.clone()))
                .collect(),
        );
        Self::from_children(ty, fields.into_iter().map(|(_, value)| value).collect())
    }

    /// A MAP value from key/value pairs.
    pub fn map(key_type: LogicalType, value_type: LogicalType, entries: Vec<(Value, Value)>) -> Self {
        let ty = LogicalType::map(key_type, value_type);
        let entry_type = ty.child_type().cloned().unwrap_or(LogicalType::INVALID);
        let items = entries
            .into_iter()
            .map(|(key, value)| Self::from_children(entry_type.clone(), vec![key, value]))
            .collect();
        Self::from_children(ty, items)
    }

    /// A UNION value of type `ty` whose member `tag` holds `value`.
    pub fn union_value(ty: LogicalType, tag: u8, value: Value) -> Self {
        let members = ty.union_members().unwrap_or_default();
        let mut value = Some(value);
        let mut fields = Vec::with_capacity(members.len() + 1);
        fields.push(Self::utinyint(tag));
        for (index, (_, member_type)) in members.iter().enumerate() {
            let member = (index == usize::from(tag)).then(|| value.take()).flatten();
            fields.push(member.unwrap_or_else(|| Self::null(member_type.clone())));
        }
        Self::from_children(ty, fields)
    }

    pub fn logical_type(&self) -> &LogicalType {
        &self.ty
    }

    pub fn is_null(&self) -> bool {
        matches!(self.payload, Payload::Null)
    }

    /// The same value seen as type `ty`, which must share the physical representation
    /// (DuckDB's `Value::Reinterpret`).
    #[must_use]
    pub fn reinterpret(mut self, ty: LogicalType) -> Self {
        self.ty = ty;
        self
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

    pub fn as_f64(&self) -> Option<f64> {
        match self.payload {
            Payload::F32(v) => Some(v.into()),
            Payload::F64(v) => Some(v),
            _ => None,
        }
    }

    payload_accessors! {
        as_bool -> bool, Bool;
        as_i8 -> i8, I8;
        as_i16 -> i16, I16;
        as_i32 -> i32, I32;
        as_i64 -> i64, I64;
        as_u8 -> u8, U8;
        as_u16 -> u16, U16;
        as_u32 -> u32, U32;
        as_u64 -> u64, U64;
        as_u128 -> u128, U128;
        as_f32 -> f32, F32;
        as_interval -> Interval, Interval;
    }

    /// A DATE value.
    pub fn as_date(&self) -> Option<Date> {
        (self.ty.id() == LogicalTypeId::Date).then_some(Date(self.as_i32()?))
    }

    /// A TIME value.
    pub fn as_time(&self) -> Option<Time> {
        (self.ty.id() == LogicalTypeId::Time).then_some(Time(self.as_i64()?))
    }

    /// A `TIMESTAMP` or `TIMESTAMP WITH TIME ZONE` value.
    pub fn as_timestamp(&self) -> Option<Timestamp> {
        matches!(
            self.ty.id(),
            LogicalTypeId::Timestamp | LogicalTypeId::TimestampTz
        )
        .then_some(Timestamp(self.as_i64()?))
    }

    /// A UUID value.
    pub fn as_uuid(&self) -> Option<Uuid> {
        match (self.ty.id(), &self.payload) {
            (LogicalTypeId::Uuid, Payload::I128(v)) => Some(Uuid(*v)),
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

    /// The text of a VARCHAR value, if it is valid UTF-8.
    pub fn as_str(&self) -> Option<&str> {
        std::str::from_utf8(self.as_bytes()?).ok()
    }

    /// The elements of a LIST/MAP/ARRAY value (MAP elements are `STRUCT(key, value)`
    /// values) or the fields of a STRUCT/UNION value (UNION fields start with the tag).
    pub fn children(&self) -> Option<&[Self]> {
        match &self.payload {
            Payload::Nested(children) => Some(children),
            _ => None,
        }
    }

    /// The type held by a `TYPE` value.
    pub fn as_type(&self) -> Option<&LogicalType> {
        match &self.payload {
            Payload::Type(ty) => Some(ty),
            _ => None,
        }
    }

    /// The tag and the active member value of a UNION.
    pub fn union_tag_and_value(&self) -> Option<(u8, &Self)> {
        let fields = self.children()?;
        let tag = fields.first()?.as_u8()?;
        Some((tag, fields.get(usize::from(tag) + 1)?))
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

    /// Reads slot `slot` of a flat/constant vector of a non-nested type.
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
            PhysicalType::Interval => Payload::Interval(vector.values::<Interval>()[slot]),
            PhysicalType::Varchar => {
                let value = &vector.values::<StringT>()[slot];
                Payload::Bytes(Arc::from(vector.string_value(value)))
            }
            PhysicalType::List
            | PhysicalType::Struct
            | PhysicalType::Array
            | PhysicalType::Unknown
            | PhysicalType::Bit
            | PhysicalType::Invalid => Payload::Null,
        };
        Self { ty, payload }
    }

    /// Writes this non-nested value into slot `slot` of a flat vector.
    pub(crate) fn write_to_vector(&self, vector: &mut Vector, slot: usize) -> Result<()> {
        if vector.logical_type().physical_type() != self.ty.physical_type() {
            return Err(Error::internal(format!(
                "Cannot write value of type {} into vector of type {}",
                self.ty,
                vector.logical_type()
            )));
        }
        match &self.payload {
            Payload::Null => {}
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
            Payload::Interval(v) => vector.values_mut::<Interval>()[slot] = *v,
            Payload::Bytes(bytes) => {
                let string: StringT = vector.add_string(bytes);
                vector.values_mut::<StringT>()[slot] = string;
            }
            Payload::Nested(_) => {
                return Err(Error::internal("nested values are written by Vector::set_value"));
            }
            Payload::Type(_) => {
                return Err(Error::not_implemented(format!(
                    "Writing {} values into vectors",
                    self.ty
                )));
            }
        }
        Ok(())
    }

    /// DuckDB's `Value::ToString()`: the value cast to VARCHAR (`"NULL"` for NULL).
    pub fn to_varchar(&self) -> Result<String> {
        if self.is_null() {
            return Ok("NULL".to_owned());
        }
        let mut out = Vec::new();
        self.write_text(&mut out)?;
        Ok(String::from_utf8(out).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned()))
    }

    /// Appends the VARCHAR text of a non-NULL value.
    fn write_text(&self, out: &mut Vec<u8>) -> Result<()> {
        use LogicalTypeId as L;
        match (&self.payload, self.ty.id()) {
            (Payload::Bool(v), _) => out.extend_from_slice(if *v { b"true" } else { b"false" }),
            (_, L::Decimal) => {
                let (width, scale) = self.ty.decimal_properties().unwrap_or((18, 3));
                format::write_decimal(self.as_i128().unwrap_or_default(), width, scale, out);
            }
            (_, L::Enum) => {
                let index = self.as_i128().unwrap_or_default();
                let text = usize::try_from(index)
                    .ok()
                    .and_then(|index| self.ty.enum_values()?.get(index));
                out.extend_from_slice(text.map_or("", String::as_str).as_bytes());
            }
            (Payload::I8(v), L::TinyInt) => format::write_i64((*v).into(), out),
            (Payload::I16(v), L::SmallInt) => format::write_i64((*v).into(), out),
            (Payload::I32(v), L::Integer) => format::write_i64((*v).into(), out),
            (Payload::I64(v), L::BigInt) => format::write_i64(*v, out),
            (Payload::I128(v), L::HugeInt) => format::write_i128(*v, out),
            (Payload::U8(v), L::UTinyInt) => format::write_u64((*v).into(), out),
            (Payload::U16(v), L::USmallInt) => format::write_u64((*v).into(), out),
            (Payload::U32(v), L::UInteger) => format::write_u64((*v).into(), out),
            (Payload::U64(v), L::UBigInt) => format::write_u64(*v, out),
            (Payload::U128(v), L::UHugeInt) => format::write_u128(*v, out),
            (Payload::F32(v), L::Float) => format::write_f32(*v, out),
            (Payload::F64(v), L::Double) => format::write_f64(*v, out),
            (Payload::Bytes(bytes), L::Varchar | L::Char) => out.extend_from_slice(bytes),
            (Payload::I32(v), L::Date) => Date(*v).write_to(out),
            (Payload::I64(v), L::Time) => Time(*v).write_to(out),
            (Payload::I64(v), L::TimeNs) => TimeNs(*v).write_to(out),
            (Payload::I64(v), L::TimeTz) => TimeTz(*v as u64).write_to(out),
            (Payload::I64(v), L::Timestamp) => Timestamp(*v).write_to(out)?,
            (Payload::I64(v), L::TimestampTz) => Timestamp(*v).write_tz_to(out)?,
            (Payload::I64(v), L::TimestampNs) => Timestamp::write_ns_to(*v, out)?,
            (Payload::I64(v), L::TimestampMs) => crate::timestamp::write_ms_to(*v, out)?,
            (Payload::I64(v), L::TimestampSec) => crate::timestamp::write_sec_to(*v, out)?,
            (Payload::Interval(v), L::Interval) => v.write_to(out),
            (Payload::Bytes(bytes), L::Blob) => crate::blob::write_text(bytes, out),
            (Payload::Bytes(bytes), L::Bit) => crate::bit::write_text(bytes, out),
            (Payload::Bytes(bytes), L::Geometry) => crate::geometry::to_text(bytes, out)?,
            (Payload::Bytes(bytes), L::Bignum) => {
                crate::bignum::write_text(bytes, out).map_err(Error::invalid_input)?;
            }
            (Payload::I128(v), L::Uuid) => Uuid(*v).write_to(out),
            (Payload::Type(ty), L::Type) => out.extend_from_slice(ty.to_string().as_bytes()),
            (Payload::Nested(items), L::List | L::Array) => {
                let quoting = Self::child_quoting(self.list_element_type());
                let raw_array = self.ty.id() == L::Array;
                out.push(b'[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.extend_from_slice(b", ");
                    }
                    // ARRAY elements are never quoted (DuckDB's `ArrayToVarcharCast`).
                    item.write_element(if raw_array { None } else { quoting }, out)?;
                }
                out.push(b']');
            }
            (Payload::Nested(entries), L::Map) => self.write_map(entries, out)?,
            (Payload::Nested(fields), L::Struct) => self.write_struct(fields, out)?,
            (Payload::Nested(_), L::Union) => match self.union_tag_and_value() {
                Some((_, member)) if !member.is_null() => member.write_text(out)?,
                _ => out.extend_from_slice(b"NULL"),
            },
            _ => {
                return Err(Error::not_implemented(format!(
                    "Unimplemented type for cast ({} -> VARCHAR)",
                    self.ty
                )));
            }
        }
        Ok(())
    }

    fn list_element_type(&self) -> Option<&LogicalType> {
        self.ty.child_type()
    }

    /// How children of type `child` are embedded: nested children are written verbatim.
    fn child_quoting(child: Option<&LogicalType>) -> Option<NestedQuoting> {
        match child {
            Some(child) if child.is_nested() => None,
            _ => Some(NestedQuoting::Value),
        }
    }

    /// Appends this value as an element of a nested value's text (`NULL` for NULL).
    fn write_element(&self, quoting: Option<NestedQuoting>, out: &mut Vec<u8>) -> Result<()> {
        if self.is_null() {
            out.extend_from_slice(b"NULL");
            return Ok(());
        }
        match quoting {
            None => self.write_text(out),
            Some(quoting) => {
                let mut text = Vec::new();
                self.write_text(&mut text)?;
                format::write_nested_string(&text, quoting, out);
                Ok(())
            }
        }
    }

    fn write_struct(&self, fields: &[Value], out: &mut Vec<u8>) -> Result<()> {
        let names = self.ty.struct_children().unwrap_or_default();
        let unnamed = self.ty.is_unnamed_struct();
        out.push(if unnamed { b'(' } else { b'{' });
        for (i, (field, (name, field_type))) in fields.iter().zip(names).enumerate() {
            if i > 0 {
                out.extend_from_slice(b", ");
            }
            if !unnamed {
                format::write_nested_string(name.as_bytes(), NestedQuoting::Key, out);
                out.extend_from_slice(b": ");
            }
            field.write_element(Self::child_quoting(Some(field_type)), out)?;
        }
        out.push(if unnamed { b')' } else { b'}' });
        Ok(())
    }

    fn write_map(&self, entries: &[Value], out: &mut Vec<u8>) -> Result<()> {
        let (key_type, value_type) = self
            .ty
            .map_key_value()
            .ok_or_else(|| Error::internal("MAP type without key/value types"))?;
        let (key_quoting, value_quoting) = (
            Self::child_quoting(Some(key_type)),
            Self::child_quoting(Some(value_type)),
        );
        out.push(b'{');
        for (i, entry) in entries.iter().enumerate() {
            if i > 0 {
                out.extend_from_slice(b", ");
            }
            match entry.children() {
                Some([key, value]) if !entry.is_null() => {
                    if key.is_null() {
                        out.extend_from_slice(b"invalid");
                        continue;
                    }
                    key.write_element(key_quoting, out)?;
                    out.push(b'=');
                    value.write_element(value_quoting, out)?;
                }
                _ => out.extend_from_slice(b"NULL"),
            }
        }
        out.push(b'}');
        Ok(())
    }

    /// DuckDB's ordering of two values of the same physical type, as used by
    /// `ValueOperations` (`NotDistinctFrom` for equality, `DistinctGreaterThan` for order):
    /// NULLs sort after every other value, NaN is the largest float and equal to itself,
    /// `-0.0` equals `0.0`, intervals compare normalised, strings bytewise and nested
    /// values lexicographically (a shorter list that is a prefix sorts first).
    ///
    /// Returns `None` if the two values are stored differently (different physical
    /// types); callers unify the types first.
    pub fn compare_not_distinct(&self, other: &Self) -> Option<Ordering> {
        fn floats<F: PartialOrd + Copy>(left: F, right: F, left_nan: bool, right_nan: bool) -> Ordering {
            match (left_nan, right_nan) {
                (true, true) => Ordering::Equal,
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                (false, false) => left.partial_cmp(&right).unwrap_or(Ordering::Equal),
            }
        }
        fn sequences(left: &[Value], right: &[Value]) -> Option<Ordering> {
            for (l, r) in left.iter().zip(right) {
                match l.compare_not_distinct(r)? {
                    Ordering::Equal => {}
                    unequal => return Some(unequal),
                }
            }
            Some(left.len().cmp(&right.len()))
        }
        Some(match (&self.payload, &other.payload) {
            (Payload::Null, Payload::Null) => Ordering::Equal,
            (Payload::Null, _) => Ordering::Greater,
            (_, Payload::Null) => Ordering::Less,
            (Payload::Bool(l), Payload::Bool(r)) => l.cmp(r),
            (Payload::I8(l), Payload::I8(r)) => l.cmp(r),
            (Payload::I16(l), Payload::I16(r)) => l.cmp(r),
            (Payload::I32(l), Payload::I32(r)) => l.cmp(r),
            (Payload::I64(l), Payload::I64(r)) => l.cmp(r),
            (Payload::I128(l), Payload::I128(r)) => l.cmp(r),
            (Payload::U8(l), Payload::U8(r)) => l.cmp(r),
            (Payload::U16(l), Payload::U16(r)) => l.cmp(r),
            (Payload::U32(l), Payload::U32(r)) => l.cmp(r),
            (Payload::U64(l), Payload::U64(r)) => l.cmp(r),
            (Payload::U128(l), Payload::U128(r)) => l.cmp(r),
            (Payload::F32(l), Payload::F32(r)) => floats(*l, *r, l.is_nan(), r.is_nan()),
            (Payload::F64(l), Payload::F64(r)) => floats(*l, *r, l.is_nan(), r.is_nan()),
            (Payload::Interval(l), Payload::Interval(r)) => l.cmp(r),
            (Payload::Bytes(l), Payload::Bytes(r)) => l.cmp(r),
            (Payload::Nested(l), Payload::Nested(r)) => sequences(l, r)?,
            (Payload::Type(l), Payload::Type(r)) => l.to_string().cmp(&r.to_string()),
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn nested_values_format_like_duckdb() {
        let strings = Value::list(
            LogicalType::VARCHAR,
            ["a b", "x,y", "", "null", " s"]
                .into_iter()
                .map(Value::varchar)
                .chain([Value::null(LogicalType::VARCHAR)])
                .collect(),
        );
        assert_eq!(
            strings.to_varchar().unwrap(),
            "[a b, 'x,y', '', 'null', ' s', NULL]"
        );
        let array = Value::array(
            LogicalType::VARCHAR,
            vec![Value::varchar("x,y"), Value::varchar("")],
        );
        assert_eq!(array.to_varchar().unwrap(), "[x,y, ]");
        let row = Value::struct_value(vec![
            ("k y".to_owned(), Value::varchar("v,w")),
            ("b".to_owned(), Value::null(LogicalType::INTEGER)),
        ]);
        assert_eq!(row.to_varchar().unwrap(), "{'k y': 'v,w', 'b': NULL}");
        let unnamed = Value::from_children(
            LogicalType::struct_type(vec![
                (String::new(), LogicalType::INTEGER),
                (String::new(), LogicalType::VARCHAR),
            ]),
            vec![Value::integer(1), Value::varchar("a b")],
        );
        assert_eq!(unnamed.to_varchar().unwrap(), "(1, a b)");
        let map = Value::map(
            LogicalType::VARCHAR,
            LogicalType::VARCHAR,
            vec![
                (Value::varchar("a,b"), Value::varchar("c d")),
                (Value::varchar("n"), Value::null(LogicalType::VARCHAR)),
            ],
        );
        assert_eq!(map.to_varchar().unwrap(), "{'a,b'=c d, n=NULL}");
        let union_type = LogicalType::union_type(vec![("t".to_owned(), LogicalType::VARCHAR)]);
        let union = Value::union_value(union_type, 0, Value::varchar("x,y"));
        assert_eq!(union.to_varchar().unwrap(), "x,y");
        let nested = Value::list(
            LogicalType::list(LogicalType::INTEGER),
            vec![
                Value::list(LogicalType::INTEGER, vec![Value::integer(1), Value::integer(2)]),
                Value::list(LogicalType::INTEGER, vec![Value::integer(3)]),
            ],
        );
        assert_eq!(nested.to_varchar().unwrap(), "[[1, 2], [3]]");
        let enum_type = LogicalType::enum_type(vec!["a".to_owned(), "b".to_owned()]);
        assert_eq!(Value::enum_value(enum_type, 1).to_varchar().unwrap(), "b");
    }
}
