//! Frozen C++ CTE, recursive CTE, macro and lambda byte-name codecs.
use rustdb_ast::{CommonTableExpressionMap, MacroFunction, ParsedExpression, QueryNode};
use rustdb_common::{ByteString, Error, Result};
use rustdb_serde::binary::{BinaryDeserializer, BinarySerializer};
use rustdb_serde::{Deserialize, Deserializer, SerializationOptions, Serialize, Serializer};
fn decode(hex: &str) -> Result<Vec<u8>> {
    if !hex.len().is_multiple_of(2) {
        return Err(Error::invalid_input("odd oracle hex length"));
    }
    hex.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(
            |pair| match (char::from(pair[0]).to_digit(16), char::from(pair[1]).to_digit(16)) {
                (Some(a), Some(b)) => Ok((a * 16 + b) as u8),
                _ => Err(Error::invalid_input("invalid oracle hex")),
            },
        )
        .collect()
}
fn check<T: Deserialize + Serialize>(
    encoded: &[u8],
    expected: &[u8],
    render: impl FnOnce(&T) -> Result<ByteString>,
) -> Result<()> {
    let mut reader = BinaryDeserializer::new(encoded);
    reader.begin()?;
    let value: T = (&mut reader as &mut dyn Deserializer).read_property(100, "payload")?;
    reader.end()?;
    let mut writer = BinarySerializer::new(SerializationOptions::default());
    writer.begin();
    (&mut writer as &mut dyn Serializer).write_property(100, "payload", &value)?;
    writer.end();
    assert_eq!(writer.bytes(), encoded, "native AST wire round trip");
    assert_eq!(render(&value)?.as_bytes(), expected, "native AST name/rendering");
    Ok(())
}
pub fn compare(line: &str) -> Result<()> {
    let fields: Vec<_> = line.split('\t').collect();
    let name = ByteString::from(decode(fields[1])?);
    let encoded = decode(fields[2])?;
    let expected = decode(fields[3])?;
    match fields[0] {
        "cte" => check::<CommonTableExpressionMap>(&encoded, &expected, |map| {
            let (key, info) = map
                .map
                .iter()
                .next()
                .ok_or_else(|| Error::invalid_input("missing CTE in native oracle"))?;
            assert_eq!(key, &name);
            assert_eq!(info.aliases, vec![name]);
            Ok(key.clone())
        }),
        "lambda" => check::<ParsedExpression>(&encoded, &expected, |expr| {
            let rustdb_ast::ParsedExpressionKind::LambdaRef(lambda) = &expr.kind else {
                return Err(Error::invalid_input("missing lambda reference in native oracle"));
            };
            Ok(lambda.column_name.clone())
        }),
        "macro" => check::<MacroFunction>(&encoded, &expected, |function| {
            let (key, _) = function
                .default_parameters
                .iter()
                .next()
                .ok_or_else(|| Error::invalid_input("missing default in native oracle"))?;
            assert_eq!(key, &name);
            Ok(key.clone())
        }),
        "recursive" => check::<QueryNode>(&encoded, &expected, |node| Ok(node.to_sql_bytes())),
        _ => Err(Error::invalid_input("unknown native AST oracle kind")),
    }
}
