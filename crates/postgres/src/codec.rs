//! PostgreSQL binary values and DuckDB 1.5.6 extension type mapping.
//!
//! Unknown types must be cast by PostgreSQL, never interpreted as UTF-8 binary data.
mod file;
mod numeric;
mod write;
pub use write::encode_parameter;

use roastedduck_common::{Error, ErrorKind, Result};
use roastedduck_types::{Date, Interval, LogicalType, Time, TimeTz, Timestamp, Uuid, Value};
use tokio_postgres::types::{Kind, Type};

const DATE_EPOCH: i32 = 10_957;
const TIMESTAMP_EPOCH: i64 = 946_684_800_000_000;

#[derive(Clone, Copy, Debug)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "Independent upstream type settings and catalog resolution capability"
)]
pub struct Options {
    pub array_as_varchar: bool,
    pub numeric_as_varchar: bool,
    pub numeric_nan_as_null: bool,
    /// Legacy `information_schema` scans do not resolve catalog-defined types.
    pub resolve_custom_types: bool,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            array_as_varchar: false,
            numeric_as_varchar: false,
            numeric_nan_as_null: true,
            resolve_custom_types: false,
        }
    }
}

#[derive(Clone, Debug)]
enum Encoding {
    Unsupported,
    NativeGeometry,
    Bool,
    I16,
    I32,
    I64,
    U32,
    F32,
    F64,
    Bytes,
    FixedChar,
    Jsonb,
    Date,
    Time,
    TimeTz,
    Timestamp,
    TimestampTz,
    Interval,
    Uuid,
    Numeric,
    Enum,
    Point,
    Geometry(Geometry),
    Array { element: Box<Codec>, dimensions: usize },
    Composite(Vec<Codec>),
    CastToVarchar,
}

#[derive(Clone, Copy, Debug)]
enum Geometry {
    Fixed(usize),
    Path,
    Polygon,
}

/// A column's logical type and its PostgreSQL binary representation.
#[derive(Clone, Debug)]
pub struct Codec {
    pub logical_type: LogicalType,
    pub oid: u32,
    encoding: Encoding,
    options: Options,
    file_format: bool,
}
impl Codec {
    /// `postgres_query` uses the pinned OID whitelist, unlike a catalog table scan.
    pub fn for_query(ty: &Type, modifier: i32, mut options: Options) -> Result<Self> {
        options.resolve_custom_types = false;
        let supported = match ty.kind() {
            Kind::Array(child) => matches!(
                child.name(),
                "bool"
                    | "char"
                    | "bpchar"
                    | "int8"
                    | "int2"
                    | "int4"
                    | "float4"
                    | "float8"
                    | "text"
                    | "varchar"
                    | "json"
                    | "jsonb"
                    | "numeric"
                    | "uuid"
                    | "date"
                    | "time"
                    | "timestamp"
                    | "timestamptz"
                    | "interval"
                    | "timetz"
                    | "bit"
            ),
            _ => matches!(
                ty.name(),
                "bool"
                    | "int2"
                    | "int4"
                    | "int8"
                    | "float4"
                    | "float8"
                    | "char"
                    | "bpchar"
                    | "text"
                    | "varchar"
                    | "json"
                    | "bytea"
                    | "date"
                    | "time"
                    | "timestamp"
                    | "interval"
                    | "timetz"
                    | "timestamptz"
                    | "bit"
                    | "uuid"
                    | "numeric"
                    | "jsonb"
            ),
        } && Type::from_oid(ty.oid()).is_some();
        if supported {
            Self::new(ty, modifier, 0, options)
        } else {
            Ok(Self {
                logical_type: LogicalType::VARCHAR,
                oid: ty.oid(),
                encoding: Encoding::CastToVarchar,
                options,
                file_format: false,
            })
        }
    }

    pub fn new(ty: &Type, modifier: i32, dimensions: usize, options: Options) -> Result<Self> {
        use Encoding as E;
        use LogicalType as L;
        let (logical_type, encoding) = if let Kind::Array(element) = ty.kind() {
            let element = Self::new(element, modifier, 0, options)?;
            if options.array_as_varchar {
                (L::VARCHAR, E::CastToVarchar)
            } else {
                let dimensions = dimensions.max(1);
                // PostgreSQL MAXDIM is six. Bound recursive allocations even for bad metadata.
                if dimensions > 6 {
                    return Err(invalid("Invalid PostgreSQL array dimension count"));
                }
                let mut logical_type = element.logical_type.clone();
                for _ in 0..dimensions {
                    logical_type = L::list(logical_type);
                }
                (
                    logical_type,
                    E::Array {
                        element: Box::new(element),
                        dimensions,
                    },
                )
            }
        } else {
            match ty.name() {
                "bool" => (L::BOOLEAN, E::Bool),
                "int2" => (L::SMALLINT, E::I16),
                "int4" => (L::INTEGER, E::I32),
                "int8" => (L::BIGINT, E::I64),
                "oid" => (L::UINTEGER, E::U32),
                "float4" => (L::FLOAT, E::F32),
                "float8" => (L::DOUBLE, E::F64),
                "char" | "bpchar" => (L::VARCHAR, E::FixedChar),
                "varchar" | "text" | "json" => (L::VARCHAR, E::Bytes),
                "jsonb" => (L::VARCHAR, E::Jsonb),
                "bytea" => (L::BLOB, E::Bytes),
                "date" => (L::DATE, E::Date),
                "time" => (L::TIME, E::Time),
                "timetz" => (L::TIME_TZ, E::TimeTz),
                "timestamp" => (L::TIMESTAMP, E::Timestamp),
                "timestamptz" => (L::TIMESTAMP_TZ, E::TimestampTz),
                "interval" => (L::INTERVAL, E::Interval),
                "uuid" => (L::UUID, E::Uuid),
                "numeric" => {
                    let modifier = i64::from(modifier);
                    let width = ((modifier - 4) >> 16) & 0xffff;
                    let scale = (((modifier - 4) & 0x7ff) ^ 1024) - 1024;
                    if modifier == -1 || scale < 0 || width > 38 {
                        if options.numeric_as_varchar {
                            (L::VARCHAR, E::CastToVarchar)
                        } else {
                            (L::DOUBLE, E::Numeric)
                        }
                    } else {
                        if width == 0 || scale > width {
                            return Err(invalid(format!(
                                "DECIMAL type with width {width} and scale {scale} is not supported"
                            )));
                        }
                        (L::decimal(width as u8, scale as u8), E::Numeric)
                    }
                }
                "point" => (
                    L::struct_type(vec![("x".into(), L::DOUBLE), ("y".into(), L::DOUBLE)]),
                    E::Point,
                ),
                "line" | "circle" => (L::list(L::DOUBLE), E::Geometry(Geometry::Fixed(3))),
                "lseg" | "box" => (L::list(L::DOUBLE), E::Geometry(Geometry::Fixed(4))),
                "path" => (L::list(L::DOUBLE), E::Geometry(Geometry::Path)),
                "polygon" => (L::list(L::DOUBLE), E::Geometry(Geometry::Polygon)),
                _ if options.resolve_custom_types => match ty.kind() {
                    Kind::Enum(labels) => (L::enum_type(labels.clone()), E::Enum),
                    Kind::Composite(fields) => {
                        let children = fields
                            .iter()
                            .map(|f| Self::new(f.type_(), -1, 0, options))
                            .collect::<Result<Vec<_>>>()?;
                        (
                            L::struct_type(
                                fields
                                    .iter()
                                    .zip(&children)
                                    .map(|(f, c)| (f.name().into(), c.logical_type.clone()))
                                    .collect(),
                            ),
                            E::Composite(children),
                        )
                    }
                    _ => (L::VARCHAR, E::CastToVarchar),
                },
                _ => (L::VARCHAR, E::CastToVarchar),
            }
        };
        Ok(Self {
            logical_type,
            oid: ty.oid(),
            encoding,
            options,
            file_format: false,
        })
    }

    pub fn needs_text_cast(&self) -> bool {
        matches!(self.encoding, Encoding::CastToVarchar)
    }
    pub fn requires_text_copy(&self) -> bool {
        use Encoding as E;
        let supported_oid = |ty: &LogicalType| match ty.id() {
            roastedduck_types::LogicalTypeId::Boolean => Some(16),
            roastedduck_types::LogicalTypeId::SmallInt => Some(21),
            roastedduck_types::LogicalTypeId::Integer => Some(23),
            roastedduck_types::LogicalTypeId::BigInt => Some(20),
            roastedduck_types::LogicalTypeId::Float => Some(700),
            roastedduck_types::LogicalTypeId::Double => Some(701),
            roastedduck_types::LogicalTypeId::Varchar => Some(1043),
            roastedduck_types::LogicalTypeId::Blob => Some(17),
            roastedduck_types::LogicalTypeId::Date => Some(1082),
            roastedduck_types::LogicalTypeId::Time => Some(1083),
            roastedduck_types::LogicalTypeId::Timestamp => Some(1114),
            roastedduck_types::LogicalTypeId::TimestampTz => Some(1184),
            roastedduck_types::LogicalTypeId::TimeTz => Some(1266),
            roastedduck_types::LogicalTypeId::Interval => Some(1186),
            roastedduck_types::LogicalTypeId::Uuid => Some(2950),
            _ => None,
        };
        match &self.encoding {
            E::CastToVarchar | E::FixedChar | E::Jsonb | E::Point | E::Geometry(_) => true,
            E::Numeric => self.logical_type.decimal_properties().is_none(),
            E::Array { element, .. } => {
                supported_oid(&element.logical_type) != Some(element.oid) || element.requires_text_copy()
            }
            E::Composite(children) => children
                .iter()
                .any(|c| supported_oid(&c.logical_type).is_none() || c.requires_text_copy()),
            _ => false,
        }
    }

    /// Server projection corresponding to the pinned scan's casts. Arrays of
    /// unsupported elements remain LIST(VARCHAR), unless the whole-array setting
    /// explicitly requests VARCHAR.
    pub fn projection(&self, expression: &str) -> Result<String> {
        if self.needs_text_cast() {
            return Ok(format!("{expression}::varchar"));
        }
        if let Encoding::Array { element, dimensions } = &self.encoding {
            if *dimensions == 1 && element.needs_text_cast() {
                return Ok(format!("{expression}::varchar[]"));
            }
        } else if self.contains_text_cast() {
            return Err(Error::new(
                ErrorKind::NotImplemented,
                format!(
                    "cast to varchar not implemented for composite column {expression} (type {})",
                    self.logical_type
                ),
            ));
        }
        Ok(expression.to_owned())
    }

    fn contains_text_cast(&self) -> bool {
        match &self.encoding {
            Encoding::CastToVarchar => true,
            Encoding::Composite(children) => children.iter().any(Self::contains_text_cast),
            Encoding::Array { element, .. } => element.contains_text_cast(),
            _ => false,
        }
    }

    pub fn decode(&self, bytes: Option<&[u8]>) -> Result<Value> {
        use Encoding as E;
        let Some(bytes) = bytes else {
            return Ok(Value::null(self.logical_type.clone()));
        };
        if self.file_format
            && bytes.is_empty()
            && self.logical_type.id() == roastedduck_types::LogicalTypeId::List
        {
            return Ok(Value::from_children(self.logical_type.clone(), Vec::new()));
        }
        let mut reader = Reader(bytes);
        let value = match &self.encoding {
            E::Unsupported => return Err(Error::internal(format!("Unsupported Type {}", self.logical_type))),
            E::NativeGeometry => {
                let geometry = roastedduck_types::geometry::from_binary(reader.rest(), true)
                    .ok()
                    .flatten()
                    .ok_or_else(|| invalid("Failed to parse Postgres geometry data"))?;
                Value::bytes(self.logical_type.clone(), &geometry)
            }
            E::Bool => Value::boolean(reader.u8()? != 0),
            E::I16 => Value::smallint(reader.i16()?),
            E::I32 => Value::integer(reader.i32()?),
            E::I64 => Value::bigint(reader.i64()?),
            E::U32 => Value::uinteger(reader.u32()?),
            E::F32 => Value::float(f32::from_bits(reader.u32()?)),
            E::F64 => Value::double(reader.f64()?),
            E::Bytes | E::CastToVarchar => Value::bytes(self.logical_type.clone(), reader.rest()),
            E::FixedChar => {
                let b = reader.rest();
                Value::bytes(self.logical_type.clone(), b.trim_ascii_end_spaces())
            }
            E::Jsonb => {
                let version = reader.u8()?;
                if version != 1 {
                    return Err(Error::new(
                        ErrorKind::NotImplemented,
                        format!("JSONB version number mismatch, expected 1, got {version}"),
                    ));
                }
                Value::bytes(self.logical_type.clone(), reader.rest())
            }
            E::Date => Value::date(match reader.i32()? {
                i32::MAX => Date::INFINITY,
                i32::MIN => Date::NEG_INFINITY,
                n => Date(n.wrapping_add(DATE_EPOCH)),
            }),
            E::Time => Value::time(Time(reader.i64()?)),
            E::TimeTz => {
                let time = Time(reader.i64()?);
                let offset = reader
                    .i32()?
                    .checked_neg()
                    .ok_or_else(|| invalid("Invalid PostgreSQL time zone"))?;
                Value::time_tz(TimeTz::new(time, offset))
            }
            E::Timestamp | E::TimestampTz => {
                let timestamp = match reader.i64()? {
                    i64::MAX => Timestamp::INFINITY,
                    i64::MIN => Timestamp::NEG_INFINITY,
                    n => Timestamp(n.wrapping_add(TIMESTAMP_EPOCH)),
                };
                if matches!(self.encoding, E::TimestampTz) {
                    Value::timestamp_tz(timestamp)
                } else {
                    Value::timestamp(timestamp)
                }
            }
            E::Interval => {
                let micros = reader.i64()?;
                let days = reader.i32()?;
                let months = reader.i32()?;
                Value::interval(Interval { months, days, micros })
            }
            E::Uuid => Value::uuid(Uuid::from_u128(u128::from_be_bytes(reader.array()?))),
            E::Numeric => numeric::decode(&mut reader, &self.logical_type, self.options.numeric_nan_as_null)?,
            E::Enum => {
                let label = reader.rest();
                let index = self
                    .logical_type
                    .enum_values()
                    .and_then(|v| v.iter().position(|v| v.as_bytes() == label))
                    .ok_or_else(|| {
                        Error::new(
                            if self.file_format {
                                ErrorKind::Io
                            } else {
                                ErrorKind::InvalidInput
                            },
                            format!("Could not map ENUM value {}", String::from_utf8_lossy(label)),
                        )
                    })?;
                Value::enum_value(self.logical_type.clone(), index as u32)
            }
            E::Point => Value::from_children(
                self.logical_type.clone(),
                vec![Value::double(reader.f64()?), Value::double(reader.f64()?)],
            ),
            E::Geometry(kind) => {
                let count = match kind {
                    Geometry::Fixed(n) => *n,
                    Geometry::Path => {
                        reader.u8()?;
                        reader.u32()? as usize * 2
                    }
                    Geometry::Polygon => reader.u32()? as usize * 2,
                };
                if count > reader.0.len() / 8 {
                    return Err(invalid("Truncated PostgreSQL geometric value"));
                }
                Value::list(
                    LogicalType::DOUBLE,
                    (0..count)
                        .map(|_| reader.f64().map(Value::double))
                        .collect::<Result<_>>()?,
                )
            }
            E::Array { element, dimensions } => {
                let actual = reader.u32()? as usize;
                reader.u32()?;
                reader.u32()?;
                if actual == 0 {
                    Value::from_children(self.logical_type.clone(), Vec::new())
                } else {
                    if actual != *dimensions {
                        return Err(invalid(format!(
                            "Expected an array with {dimensions} dimensions, but this array has {actual} dimensions. The array stored in Postgres does not match the schema. Postgres does not enforce that arrays match the provided schema but DuckDB requires this.\nSet pg_array_as_varchar=true to read the array as a varchar instead."
                        )));
                    }
                    let mut shape = Vec::with_capacity(actual);
                    let mut size = 1usize;
                    for _ in 0..actual {
                        let n = reader.u32()? as usize;
                        reader.u32()?;
                        if n == 0 && !self.file_format {
                            return Err(invalid("Invalid zero-sized PostgreSQL array dimension"));
                        }
                        size = size
                            .checked_mul(n)
                            .ok_or_else(|| invalid("PostgreSQL array size overflow"))?;
                        shape.push(n);
                    }
                    if size > reader.0.len() / 4 {
                        return Err(invalid("Truncated PostgreSQL array"));
                    }
                    decode_array(&mut reader, &self.logical_type, &shape, element)?
                }
            }
            E::Composite(children) => {
                let count = reader.u32()? as usize;
                if count != children.len() {
                    return Err(invalid(format!(
                        "Mismatch in entry count: expected {} but got {count}",
                        children.len()
                    )));
                }
                let mut values = Vec::with_capacity(count);
                for child in children {
                    reader.u32()?;
                    values.push(child.decode(reader.field()?)?);
                }
                Value::from_children(self.logical_type.clone(), values)
            }
        };
        if !reader.0.is_empty() {
            return Err(invalid("Trailing bytes in PostgreSQL binary value"));
        }
        Ok(value)
    }
}

fn decode_array(
    reader: &mut Reader<'_>,
    ty: &LogicalType,
    shape: &[usize],
    element: &Codec,
) -> Result<Value> {
    let child = ty
        .child_type()
        .ok_or_else(|| Error::internal("PostgreSQL array type missing child"))?;
    let mut values = Vec::with_capacity(shape[0]);
    for _ in 0..shape[0] {
        values.push(if shape.len() == 1 {
            element.decode(reader.field()?)?
        } else {
            decode_array(reader, child, &shape[1..], element)?
        });
    }
    Ok(Value::from_children(ty.clone(), values))
}

fn invalid(message: impl Into<String>) -> Error {
    Error::invalid_input(message.into())
}

// Unlike trim_ascii_end, bpchar discards spaces only (not tabs or line breaks).
trait TrimSpaces {
    fn trim_ascii_end_spaces(&self) -> &Self;
}
impl TrimSpaces for [u8] {
    fn trim_ascii_end_spaces(&self) -> &Self {
        &self[..self.iter().rposition(|b| *b != b' ').map_or(0, |i| i + 1)]
    }
}

struct Reader<'a>(&'a [u8]);
impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let (v, rest) = self
            .0
            .split_at_checked(n)
            .ok_or_else(|| invalid("Truncated PostgreSQL binary value"))?;
        self.0 = rest;
        Ok(v)
    }
    fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        self.take(N)?
            .try_into()
            .map_err(|_| invalid("Truncated PostgreSQL binary value"))
    }
    fn rest(&mut self) -> &'a [u8] {
        std::mem::take(&mut self.0)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.array::<1>()?[0])
    }
    fn i16(&mut self) -> Result<i16> {
        Ok(i16::from_be_bytes(self.array()?))
    }
    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_be_bytes(self.array()?))
    }
    fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_be_bytes(self.array()?))
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(self.array()?))
    }
    fn i64(&mut self) -> Result<i64> {
        Ok(i64::from_be_bytes(self.array()?))
    }
    fn f64(&mut self) -> Result<f64> {
        Ok(f64::from_be_bytes(self.array()?))
    }
    fn field(&mut self) -> Result<Option<&'a [u8]>> {
        match self.i32()? {
            -1 => Ok(None),
            n if n >= 0 => self.take(n as usize).map(Some),
            _ => Err(invalid("Invalid PostgreSQL binary field length")),
        }
    }
}
