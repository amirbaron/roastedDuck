//! Independent C++ bound-parameter metadata and native text.
use roastedduck_common::{Error, Result};
use roastedduck_planner::expression::ExpressionKind;
use roastedduck_planner::prepared::BoundParameterMap;
use roastedduck_serde::{SerializationOptions, Serializer, binary::BinarySerializer};
fn unhex(text: &str) -> Result<Vec<u8>> {
    let (pairs, rest) = text.as_bytes().as_chunks::<2>();
    if !rest.is_empty() {
        return Err(Error::invalid_input("odd oracle hex"));
    }
    pairs
        .iter()
        .map(|pair| {
            let digit = |b| match b {
                b'0'..=b'9' => Ok(b - b'0'),
                b'a'..=b'f' => Ok(b - b'a' + 10),
                _ => Err(Error::invalid_input("invalid oracle hex")),
            };
            Ok(digit(pair[0])? * 16 + digit(pair[1])?)
        })
        .collect()
}
pub fn compare(line: &str) -> Result<()> {
    let fields: Vec<_> = line.split('\t').collect();
    if fields.len() != 3 {
        return Err(Error::invalid_input("invalid parameter oracle record"));
    }
    let name = unhex(fields[0])?;
    let mut parameters = BoundParameterMap::default();
    let expression = parameters.bind_parameter_expression(&name, &name);
    let mut writer = BinarySerializer::new(SerializationOptions::default());
    writer.begin();
    (&mut writer as &mut dyn Serializer).write_property(100, "payload", &expression)?;
    writer.end();
    if writer.bytes() != unhex(fields[1])? {
        return Err(Error::internal("bound parameter wire differs"));
    }
    if expression.to_bytes().as_bytes() != unhex(fields[2])? {
        return Err(Error::internal("bound parameter text differs"));
    }
    let repeated = parameters.bind_parameter_expression(name.to_ascii_uppercase(), b"other");
    let (ExpressionKind::Parameter(first), ExpressionKind::Parameter(second)) =
        (&expression.kind, &repeated.kind)
    else {
        return Err(Error::internal("missing bound parameters"));
    };
    if !std::sync::Arc::ptr_eq(&first.parameter_data, &second.parameter_data) {
        return Err(Error::internal("ASCII-insensitive parameter uses different data"));
    }
    Ok(())
}
