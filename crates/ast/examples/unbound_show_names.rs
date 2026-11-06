//! Unbound-type and SHOW metadata against the frozen C++ serializer and SQL renderer.
use rustdb_ast::{ShowRef, ShowType, TableRef, TableRefKind, logical_type_to_string};
use rustdb_common::{ByteString, Error, Result};
use rustdb_serde::binary::BinarySerializer;
use rustdb_serde::{SerializationCompatibility, SerializationOptions, Serialize, Serializer};
use rustdb_types::{
    LogicalType, LogicalTypeId, TypeInfo, TypeParameter, TypeParameterValue, UnboundTypeInfo, Value,
};
use std::io::{self, BufRead};
fn decode(hex: &str) -> Result<Vec<u8>> {
    let (pairs, rest) = hex.as_bytes().as_chunks::<2>();
    if !rest.is_empty() {
        return Err(Error::invalid_input("odd hex length"));
    }
    pairs
        .iter()
        .map(
            |p| match (char::from(p[0]).to_digit(16), char::from(p[1]).to_digit(16)) {
                (Some(a), Some(b)) => Ok((a * 16 + b) as u8),
                _ => Err(Error::invalid_input("invalid hex")),
            },
        )
        .collect()
}
fn wire<T: Serialize>(value: &T, latest: bool) -> Result<Vec<u8>> {
    let options = if latest {
        SerializationOptions {
            compatibility: SerializationCompatibility::latest(),
            ..SerializationOptions::default()
        }
    } else {
        SerializationOptions::default()
    };
    let mut writer = BinarySerializer::new(options);
    writer.begin();
    (&mut writer as &mut dyn Serializer).write_property(100, "payload", value)?;
    writer.end();
    Ok(writer.bytes().to_vec())
}
fn outcome(mode: &str, name: ByteString, latest: bool) -> Result<(Vec<u8>, ByteString)> {
    if mode.starts_with("show-") {
        let mut show = ShowRef {
            table_name: "t".into(),
            catalog_name: "c".into(),
            schema_name: "s".into(),
            query: None,
            show_type: ShowType::ShowFrom,
        };
        match mode {
            "show-table" => show.table_name = name,
            "show-catalog" => show.catalog_name = name,
            "show-schema" => show.schema_name = name,
            _ => return Err(Error::invalid_input("unknown SHOW mode")),
        }
        let table = Box::new(TableRef {
            external_dependency: rustdb_ast::ExternalDependency::default(),
            alias: ByteString::new(),
            sample: None,
            query_location: None,
            kind: TableRefKind::Show(Box::new(show)),
        });
        return Ok((wire(&table, latest)?, rustdb_common::byte_format!("{table}")));
    }
    let mut info = UnboundTypeInfo {
        catalog: ByteString::new(),
        schema: ByteString::new(),
        name: "custom".into(),
        parameters: Vec::new(),
        query_location: None,
    };
    match mode {
        "name" => info.name = name,
        "catalog" => info.catalog = name,
        "schema" => info.schema = name,
        "enum" | "bind-enum" => {
            info.name = "ENUM".into();
            info.parameters.push(TypeParameter {
                name,
                value: TypeParameterValue::Constant(Value::varchar("x")),
                query_location: None,
            });
        }
        "field" | "bind-mix" => {
            info.name = "STRUCT".into();
            info.parameters.push(TypeParameter {
                name,
                value: TypeParameterValue::Type(UnboundTypeInfo {
                    name: "INTEGER".into(),
                    catalog: ByteString::new(),
                    schema: ByteString::new(),
                    parameters: Vec::new(),
                    query_location: None,
                }),
                query_location: None,
            });
            if mode == "bind-mix" {
                info.parameters.push(TypeParameter {
                    name: "b".into(),
                    value: TypeParameterValue::Type(UnboundTypeInfo {
                        name: "VARCHAR".into(),
                        catalog: ByteString::new(),
                        schema: ByteString::new(),
                        parameters: Vec::new(),
                        query_location: None,
                    }),
                    query_location: None,
                });
            }
        }
        _ => return Err(Error::invalid_input("unknown unbound mode")),
    }
    let logical = if mode.starts_with("bind-") {
        info.default_bind()?
    } else {
        let type_render = info.to_sql_bytes()?;
        let logical = LogicalType::with_info(LogicalTypeId::Unbound, TypeInfo::Unbound(info));
        if logical_type_to_string(&logical) != type_render {
            return Err(Error::internal("unbound type renderer differs from AST renderer"));
        }
        logical
    };
    Ok((wire(&logical, latest)?, logical_type_to_string(&logical)))
}
pub fn compare(line: &str) -> Result<()> {
    let fields: Vec<_> = line.split('\t').collect();
    if fields.len() != 5 {
        return Err(Error::invalid_input("invalid unbound/SHOW record"));
    }
    let (version, mode) = fields[0]
        .split_once('/')
        .ok_or_else(|| Error::invalid_input("missing version"))?;
    let result = outcome(mode, decode(fields[1])?.into(), version == "latest");
    match result {
        Ok((bytes, render))
            if fields[3] == "ok"
                && bytes == decode(fields[2])?
                && render.as_bytes() == decode(fields[4])? =>
        {
            Ok(())
        }
        Err(error) if fields[3] == "error" && error.raw_message_bytes() == decode(fields[4])? => Ok(()),
        result => Err(Error::internal(format!(
            "unbound/SHOW outcome differs from C++: {} name {} actual {result:?}",
            fields[0], fields[1]
        ))),
    }
}
fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut count = 0;
    for line in io::stdin().lock().lines() {
        compare(&line?)?;
        count += 1;
    }
    println!("{count} C++ unbound/SHOW comparisons passed");
    Ok(())
}
