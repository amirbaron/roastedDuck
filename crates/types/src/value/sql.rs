//! Values as SQL literals.

use crate::{LogicalTypeId, Value};

impl Value {
    /// The value as SQL text (DuckDB's `Value::ToSQLString`).
    pub fn to_sql_string(&self) -> String {
        let value = self;
        let text = || value.to_varchar().unwrap_or_default();
        if value.is_null() {
            return "NULL".to_owned();
        }
        let ty = value.logical_type();
        match ty.id() {
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
            | LogicalTypeId::Blob => format!("'{}'::{ty}", text()),
            LogicalTypeId::Varchar | LogicalTypeId::Enum => {
                let text = text();
                if text == "\0" {
                    return "chr(0)".to_owned();
                }
                format!("'{}'", text.replace('\'', "''"))
            }
            LogicalTypeId::Struct => {
                let children = value.children().unwrap_or_default();
                let names = ty.struct_children().unwrap_or_default();
                let unnamed = ty.is_unnamed_struct();
                let fields: Vec<String> = children
                    .iter()
                    .zip(names)
                    .map(|(child, (name, _))| {
                        if unnamed {
                            child.to_sql_string()
                        } else {
                            format!("'{name}': {}", child.to_sql_string())
                        }
                    })
                    .collect();
                if unnamed {
                    format!("({})", fields.join(", "))
                } else {
                    format!("{{{}}}", fields.join(", "))
                }
            }
            LogicalTypeId::Float | LogicalTypeId::Double => {
                let float = value.as_f64().unwrap_or_default();
                if float.is_finite() {
                    text()
                } else if ty.id() == LogicalTypeId::Double && !float.is_nan() {
                    if float < 0.0 { "-1e1000" } else { "1e1000" }.to_owned()
                } else {
                    format!("'{}'::{ty}", text())
                }
            }
            LogicalTypeId::List | LogicalTypeId::Array => {
                let children = value.children().unwrap_or_default();
                let items: Vec<String> = children.iter().map(Value::to_sql_string).collect();
                format!("[{}]", items.join(", "))
            }
            LogicalTypeId::Map => {
                let entries = value.children().unwrap_or_default();
                let items: Vec<String> = entries
                    .iter()
                    .map(|entry| {
                        let pair = entry.children().unwrap_or_default();
                        let render = |i: usize| pair.get(i).map(Value::to_sql_string).unwrap_or_default();
                        format!("{}: {}", render(0), render(1))
                    })
                    .collect();
                format!("MAP {{{}}}::{ty}", items.join(", "))
            }
            _ => text(),
        }
    }
}
