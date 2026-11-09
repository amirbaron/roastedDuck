//! Exact native function/window/collation/LOAD metadata and SQL against C++ 1.5.6.
use roastedduck_ast::{
    CollateExpression, ExpressionType, FunctionExpression, LoadInfo, LoadType, OrderModifier, ParseInfo,
    ParsedExpression, ParsedExpressionKind, WindowBoundary, WindowExcludeMode, WindowExpression,
};
use roastedduck_common::{ByteString, Error, Result, byte_format};
use roastedduck_serde::binary::BinarySerializer;
use roastedduck_serde::{SerializationOptions, Serialize, Serializer};
use roastedduck_types::{LogicalType, LogicalTypeId, StringTypeInfo, TypeInfo, Value};
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
fn wire<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    let mut writer = BinarySerializer::new(SerializationOptions::default());
    writer.begin();
    (&mut writer as &mut dyn Serializer).write_property(100, "payload", value)?;
    writer.end();
    Ok(writer.bytes().to_vec())
}
fn outcome(mode: &str, name: ByteString) -> Result<(Vec<u8>, ByteString)> {
    if mode == "string-type" {
        let ty = LogicalType::with_info(
            LogicalTypeId::Varchar,
            TypeInfo::String(StringTypeInfo { collation: name }),
        );
        return Ok((wire(&ty)?, ty.to_bytes()));
    }
    if mode.starts_with("load-") {
        let mut load = LoadInfo {
            filename: "ext".into(),
            repository: "repo".into(),
            version: "v1".into(),
            load_type: LoadType::Install,
            repo_is_alias: false,
        };
        match mode {
            "load-file" => load.filename = name,
            "load-repository" => load.repository = name,
            "load-version" => load.version = name,
            _ => return Err(Error::invalid_input("unknown load mode")),
        }
        let sql = load.to_sql_bytes();
        let info = Box::new(ParseInfo::Load(Box::new(load)));
        return Ok((wire(&info)?, sql));
    }
    let expr = if mode.starts_with("window-") {
        let mut window = WindowExpression {
            function_name: "fn".into(),
            schema: ByteString::new(),
            catalog: ByteString::new(),
            children: Vec::new(),
            partitions: Vec::new(),
            orders: Vec::new(),
            start: WindowBoundary::UnboundedPreceding,
            end: WindowBoundary::CurrentRowRange,
            start_expr: None,
            end_expr: None,
            offset_expr: None,
            default_expr: None,
            ignore_nulls: false,
            filter_expr: None,
            exclude_clause: WindowExcludeMode::NoOther,
            distinct: false,
            arg_orders: Vec::new(),
        };
        match mode {
            "window-name" => window.function_name = name,
            "window-catalog" => window.catalog = name,
            "window-schema" => window.schema = name,
            _ => return Err(Error::invalid_input("unknown window mode")),
        }
        ParsedExpression::new(
            ExpressionType::WindowAggregate,
            ParsedExpressionKind::Window(Box::new(window)),
        )
    } else if mode == "collate" {
        ParsedExpression::new(
            ExpressionType::Collate,
            ParsedExpressionKind::Collate(CollateExpression {
                collation: name,
                child: Box::new(ParsedExpression::constant(Value::integer(1))),
            }),
        )
    } else {
        let mut function = FunctionExpression {
            function_name: "fn".into(),
            schema: ByteString::new(),
            catalog: ByteString::new(),
            children: vec![ParsedExpression::constant(Value::integer(1))],
            filter: None,
            order_bys: OrderModifier { orders: Vec::new() },
            distinct: false,
            is_operator: false,
            export_state: false,
        };
        match mode {
            "function-name" => function.function_name = name,
            "function-catalog" => function.catalog = name,
            "function-schema" => function.schema = name,
            "postfix" => {
                function.function_name = name;
                function.function_name.push_bytes("__postfix__postfix");
                function.is_operator = true;
            }
            _ => return Err(Error::invalid_input("unknown function mode")),
        }
        ParsedExpression::new(
            ExpressionType::Function,
            ParsedExpressionKind::Function(Box::new(function)),
        )
    };
    let expr = Box::new(expr);
    Ok((wire(&expr)?, byte_format!("{expr}")))
}
pub fn compare(line: &str) -> Result<()> {
    let f: Vec<_> = line.split('\t').collect();
    if f.len() != 5 {
        return Err(Error::invalid_input("invalid function oracle record"));
    }
    let result = outcome(f[0], decode(f[1])?.into());
    match result {
        Ok((bytes, sql)) if f[3] == "ok" && bytes == decode(f[2])? && sql.as_bytes() == decode(f[4])? => {
            Ok(())
        }
        Err(error) if f[3] == "error" && error.raw_message_bytes() == decode(f[4])? => Ok(()),
        result => Err(Error::internal(format!(
            "function/window mismatch {} name {}: {result:?}",
            f[0], f[1]
        ))),
    }
}
fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut count = 0;
    for line in io::stdin().lock().lines() {
        compare(&line?)?;
        count += 1;
    }
    println!("{count} C++ function/window/collation/LOAD comparisons passed");
    Ok(())
}
