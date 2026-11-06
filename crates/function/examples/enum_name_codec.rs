//! Compare native ENUM dictionaries, values and rendering with frozen C++ outcomes.
use rustdb_common::{ByteString, Error, Result};
use rustdb_function::cast::ValueCast;
use rustdb_serde::binary::{BinaryDeserializer, BinarySerializer};
use rustdb_serde::{Deserialize, Deserializer, SerializationOptions, Serialize, Serializer};
use rustdb_types::{LogicalType, Value};
use std::io::{self, BufRead};
fn decode(hex: &str) -> Result<Vec<u8>> {
    fn digit(byte: u8) -> Result<u8> {
        match byte {
            b'0'..=b'9' => Ok(byte - b'0'),
            b'a'..=b'f' => Ok(byte - b'a' + 10),
            _ => Err(Error::invalid_input("invalid hex")),
        }
    }
    let (pairs, rest) = hex.as_bytes().as_chunks::<2>();
    if !rest.is_empty() {
        return Err(Error::invalid_input("odd hex length"));
    }
    pairs
        .iter()
        .map(|p| Ok((digit(p[0])? << 4) | digit(p[1])?))
        .collect()
}
fn roundtrip<T: Serialize + Deserialize>(value: &T, encoded: &[u8]) -> Result<T> {
    let mut writer = BinarySerializer::new(SerializationOptions::default());
    writer.begin();
    (&mut writer as &mut dyn Serializer).write_property(100, "payload", value)?;
    writer.end();
    if writer.bytes() != encoded {
        return Err(Error::internal("native ENUM wire differs from C++"));
    }
    let mut reader = BinaryDeserializer::new(encoded);
    reader.begin()?;
    let restored = (&mut reader as &mut dyn Deserializer).read_property(100, "payload")?;
    reader.end()?;
    Ok(restored)
}
pub fn compare(line: &str) -> Result<()> {
    let fields: Vec<_> = line.split('\t').collect();
    if fields.len() != 5 {
        return Err(Error::invalid_input("invalid ENUM oracle record"));
    }
    let name = ByteString::from(decode(fields[1])?);
    let mut tail = name.clone();
    tail.push_bytes("_tail");
    let ty = LogicalType::enum_type_bytes(vec![name, tail]);
    let wire = decode(fields[2])?;
    let result = if matches!(fields[0], "type" | "typeof") {
        let restored: LogicalType = roundtrip(&ty, &wire)?;
        if restored != ty {
            return Err(Error::internal("ENUM type round trip differs"));
        }
        if fields[0] == "type" {
            Ok(restored.to_bytes())
        } else {
            Value::try_varchar(&restored.to_bytes()).and_then(|value| value.to_varchar_bytes())
        }
    } else {
        let value = Value::enum_value(ty, 0);
        let restored: Value = roundtrip(&value, &wire)?;
        if restored != value {
            return Err(Error::internal("ENUM value round trip differs"));
        }
        match fields[0] {
            "plain" => restored.to_varchar_bytes(),
            "sql" => restored.try_to_sql_string(),
            "cast" => restored
                .default_cast_as(&LogicalType::VARCHAR)
                .and_then(|value| value.to_varchar_bytes()),
            _ => return Err(Error::invalid_input("unknown ENUM oracle operation")),
        }
    };
    let (status, actual): (&str, ByteString) = match result {
        Ok(bytes) => ("ok", bytes),
        Err(error) => ("error", error.raw_message_bytes().into()),
    };
    if status != fields[3] || actual.as_bytes() != decode(fields[4])? {
        return Err(Error::internal(rustdb_common::byte_format!(
            "ENUM {} outcome differs for hex name {}",
            fields[0],
            fields[1]
        )));
    }
    Ok(())
}
fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut count = 0;
    for line in io::stdin().lock().lines() {
        compare(&line?)?;
        count += 1;
    }
    println!("{count} C++ ENUM comparisons passed");
    Ok(())
}
