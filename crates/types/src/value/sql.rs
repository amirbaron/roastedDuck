//! Values as SQL literals.

use crate::{LogicalTypeId, Value};
use rustdb_common::{ByteString, Result, byte_format as format};

impl Value {
    /// The legacy infallible SQL formatter. Native callers that must propagate
    /// DuckDB's rendering exceptions use `try_to_sql_string`.
    pub fn to_sql_string(&self) -> ByteString {
        self.try_to_sql_string().unwrap_or_default()
    }

    /// DuckDB's `Value::ToSQLString`, including exceptions from VARCHAR conversion.
    pub fn try_to_sql_string(&self) -> Result<ByteString> {
        if self.is_null() {
            return Ok("NULL".into());
        }
        let text = || self.to_varchar_bytes();
        let ty = self.logical_type();
        Ok(match ty.id() {
            LogicalTypeId::Uuid
            | LogicalTypeId::Date
            | LogicalTypeId::Time
            | LogicalTypeId::TimeNs
            | LogicalTypeId::Timestamp
            | LogicalTypeId::TimeTz
            | LogicalTypeId::TimestampTz
            | LogicalTypeId::TimestampSec
            | LogicalTypeId::TimestampMs
            | LogicalTypeId::TimestampNs
            | LogicalTypeId::Interval
            | LogicalTypeId::Blob => format!("'{}'::{ty}", text()?),
            LogicalTypeId::Varchar | LogicalTypeId::Enum => {
                let text = text()?;
                if text == "\0" {
                    return Ok("chr(0)".into());
                }
                format!("'{}'", text.replace_byte(b'\'', b"''"))
            }
            LogicalTypeId::Struct => {
                let children = self.children().unwrap_or_default();
                let names = ty.struct_children().unwrap_or_default();
                let unnamed = ty.is_unnamed_struct();
                let fields = children
                    .iter()
                    .zip(names)
                    .map(|(child, (name, _))| {
                        let child = child.try_to_sql_string()?;
                        Ok(if unnamed {
                            child
                        } else {
                            format!("'{name}': {child}")
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                if unnamed {
                    format!("({})", ByteString::join(&fields, ", "))
                } else {
                    format!("{{{}}}", ByteString::join(&fields, ", "))
                }
            }
            LogicalTypeId::Float | LogicalTypeId::Double => {
                let float = self.as_f64().unwrap_or_default();
                if float.is_finite() {
                    text()?
                } else if ty.id() == LogicalTypeId::Double && !float.is_nan() {
                    ByteString::from(if float < 0.0 { "-1e1000" } else { "1e1000" })
                } else {
                    format!("'{}'::{ty}", text()?)
                }
            }
            LogicalTypeId::List | LogicalTypeId::Array => {
                let items = self
                    .children()
                    .unwrap_or_default()
                    .iter()
                    .map(Self::try_to_sql_string)
                    .collect::<Result<Vec<_>>>()?;
                format!("[{}]", ByteString::join(&items, ", "))
            }
            LogicalTypeId::Map => {
                let items = self
                    .children()
                    .unwrap_or_default()
                    .iter()
                    .map(|entry| {
                        let pair = entry.children().unwrap_or_default();
                        let render = |i: usize| {
                            pair.get(i)
                                .map_or_else(|| Ok(ByteString::new()), Self::try_to_sql_string)
                        };
                        Ok(format!("{}: {}", render(0)?, render(1)?))
                    })
                    .collect::<Result<Vec<_>>>()?;
                format!("MAP {{{}}}::{ty}", ByteString::join(&items, ", "))
            }
            LogicalTypeId::Union => {
                let (tag, member) = self
                    .union_tag_and_value()
                    .ok_or_else(|| rustdb_common::Error::internal("union value without a valid tag"))?;
                let (name, _) = ty
                    .union_members()
                    .and_then(|members| members.get(usize::from(tag)))
                    .ok_or_else(|| rustdb_common::Error::internal("union tag out of range"))?;
                format!("union_value({name} := {})", member.try_to_sql_string()?)
            }
            LogicalTypeId::Variant => format!("VARIANT({})", text()?),
            _ => text()?,
        })
    }
}
