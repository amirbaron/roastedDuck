//! Frozen C++ pivot/name codecs and value rendering outcomes, including exceptions.
use roastedduck_ast::SelectStatement;
use roastedduck_common::{ByteString, Error, Result};
use roastedduck_function::cast::ValueCast;
use roastedduck_serde::binary::{BinaryDeserializer, BinarySerializer};
use roastedduck_serde::{Deserializer, SerializationOptions, Serializer};
use roastedduck_types::{LogicalType, Value};
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
fn outcome(result: Result<ByteString>) -> String {
    let (prefix, bytes) = match result {
        Ok(bytes) => ("OK:", bytes.into_bytes()),
        Err(error) => ("ERR:", error.to_bytes()),
    };
    let mut out = prefix.to_owned();
    for byte in bytes {
        let digits = b"0123456789abcdef";
        out.push(char::from(digits[usize::from(byte >> 4)]));
        out.push(char::from(digits[usize::from(byte & 15)]));
    }
    out
}
pub fn compare(line: &str) -> Result<()> {
    let fields: Vec<_> = line.split('\t').collect();
    if matches!(fields[0], "pivot" | "unpivot") {
        let wire = decode(fields[2])?;
        let mut reader = BinaryDeserializer::new(&wire);
        reader.begin()?;
        let value: SelectStatement = (&mut reader as &mut dyn Deserializer).read_property(100, "payload")?;
        reader.end()?;
        let mut writer = BinarySerializer::new(SerializationOptions::default());
        writer.begin();
        (&mut writer as &mut dyn Serializer).write_property(100, "payload", &value)?;
        writer.end();
        assert_eq!(writer.bytes(), wire, "pivot AST round trip");
        assert_eq!(value.to_sql_bytes().as_bytes(), decode(fields[3])?, "pivot SQL");
    } else {
        let name = ByteString::from(decode(fields[1])?);
        let row = Value::struct_value(vec![(name.clone(), Value::integer(42))]);
        let type_value = Value::type_value(row.logical_type().clone());
        let value = match fields[0] {
            "0" => Ok(row.clone()),
            "1" => Ok(Value::list(row.logical_type().clone(), vec![row.clone()])),
            "2" => Ok(type_value.clone()),
            "3" => Ok(Value::list(type_value.logical_type().clone(), vec![type_value])),
            "4" => Ok(Value::union_value(
                LogicalType::union_type(vec![(name, LogicalType::INTEGER)]),
                0,
                Value::integer(42),
            )),
            "5" => row.default_cast_as(&LogicalType::variant()),
            "6" => row.default_cast_as(&LogicalType::VARCHAR),
            _ => return Err(Error::invalid_input("unknown native value oracle kind")),
        };
        let plain = outcome(value.clone().and_then(|value| value.to_varchar_bytes()));
        let sql = outcome(value.and_then(|value| value.try_to_sql_string()));
        if plain != fields[2] || sql != fields[3] {
            return Err(Error::invalid_input(format!(
                "native value outcome mismatch: mode={} name={} plain={} sql={}",
                fields[0],
                &fields[1][..fields[1].len().min(80)],
                plain == fields[2],
                sql == fields[3]
            )));
        }
    }
    Ok(())
}
