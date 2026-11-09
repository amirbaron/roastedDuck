//! Native C++ AST/type/value/map binary round trips and byte rendering.
use roastedduck_ast::SelectStatement;
use roastedduck_common::{ByteString, CaseInsensitiveMap};
use roastedduck_serde::binary::{BinaryDeserializer, BinarySerializer};
use roastedduck_serde::{Deserialize, Deserializer, SerializationOptions, Serialize, Serializer};
use roastedduck_types::{LogicalType, Value};
use std::io::{self, BufRead};

fn decode(hex: &str) -> Vec<u8> {
    hex.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let digit = |b| match b {
                b'0'..=b'9' => b - b'0',
                b'a'..=b'f' => b - b'a' + 10,
                _ => unreachable!(),
            };
            digit(pair[0]) * 16 + digit(pair[1])
        })
        .collect()
}
fn check<T: Deserialize + Serialize>(
    encoded: &[u8],
    expected: &[u8],
    render: impl FnOnce(&T) -> ByteString,
) -> Result<(), String> {
    let mut reader = BinaryDeserializer::new(encoded);
    reader.begin().map_err(|e| e.to_string())?;
    let value: T = (&mut reader as &mut dyn Deserializer)
        .read_property(100, "payload")
        .map_err(|e| e.to_string())?;
    reader.end().map_err(|e| e.to_string())?;
    let mut writer = BinarySerializer::new(SerializationOptions::default());
    writer.begin();
    (&mut writer as &mut dyn Serializer)
        .write_property(100, "payload", &value)
        .map_err(|e| e.to_string())?;
    writer.end();
    if writer.bytes() != encoded {
        return Err("binary round trip differs".into());
    }
    let actual = render(&value);
    if actual.as_bytes() != expected {
        let hex = |bytes: &[u8]| {
            bytes
                .iter()
                .take(1024)
                .flat_map(|byte| {
                    let digits = b"0123456789abcdef";
                    [
                        char::from(digits[usize::from(byte >> 4)]),
                        char::from(digits[usize::from(byte & 15)]),
                    ]
                })
                .collect::<String>()
        };
        return Err(format!(
            "native byte rendering differs: expected={} actual={}",
            hex(expected),
            hex(actual.as_bytes())
        ));
    }
    Ok(())
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (mut passed, mut failed, mut cpp_rejected) = (0, 0, 0);
    for (index, line) in io::stdin().lock().lines().enumerate() {
        let line = line?;
        let fields: Vec<_> = line.split('\t').collect();
        assert_eq!(fields.len(), 4);
        if fields[2] == "ERROR" {
            cpp_rejected += 1;
            continue;
        }
        let encoded = decode(fields[2]);
        let expected = decode(fields[3]);
        let result = match fields[0] {
            "query" => check::<SelectStatement>(&encoded, &expected, SelectStatement::to_sql_bytes),
            "type" => check::<LogicalType>(&encoded, &expected, LogicalType::to_bytes),
            "value" => check::<Value>(&encoded, &expected, Value::to_sql_string),
            "map" => {
                let name = ByteString::from(decode(fields[1]));
                check::<CaseInsensitiveMap<u64, ByteString>>(&encoded, &expected, |map| {
                    assert_eq!(map.get(name.as_bytes()), Some(&43));
                    assert_eq!(map.get(name.to_ascii_lowercase()), Some(&43));
                    assert_eq!(map.iter().next().map(|(key, _)| key), Some(&name));
                    map.len().to_string().into()
                })
            }
            _ => return Err("unknown oracle payload".into()),
        };
        match result {
            Ok(()) => passed += 1,
            Err(error) => {
                failed += 1;
                eprintln!("case {index}, {}: {error}", fields[0]);
            }
        }
    }
    println!("passed={passed} failed={failed} cpp_rejected={cpp_rejected}");
    if failed > 0 {
        return Err("native name comparison failed".into());
    }
    Ok(())
}
