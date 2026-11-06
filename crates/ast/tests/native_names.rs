//! Golden data from DuckDB 1.5.6 native `BinarySerializer` + native byte renderers.
//! Includes invalid UTF-8, NUL, case-insensitive keys and parsed query-name positions.
//! Generator/evidence: .context/artifacts/identifier-bytes-20261023/native-name-oracle.cpp.
use rustdb_ast::SelectStatement;
use rustdb_common::{ByteString, CaseInsensitiveMap};
use rustdb_serde::binary::{BinaryDeserializer, BinarySerializer};
use rustdb_serde::{Deserialize, Deserializer, SerializationOptions, Serialize, Serializer};
use rustdb_types::{LogicalType, Value};

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
fn compare<T: Deserialize + Serialize>(
    encoded: &[u8],
    expected: &[u8],
    render: impl FnOnce(&T) -> ByteString,
) -> rustdb_common::Result<()> {
    let mut reader = BinaryDeserializer::new(encoded);
    reader.begin()?;
    let value: T = (&mut reader as &mut dyn Deserializer)
        .read_property(100, "payload")?;
    reader.end()?;
    let mut writer = BinarySerializer::new(SerializationOptions::default());
    writer.begin();
    (&mut writer as &mut dyn Serializer)
        .write_property(100, "payload", &value)?;
    writer.end();
    assert_eq!(writer.bytes(), encoded);
    assert_eq!(render(&value).as_bytes(), expected);
    Ok(())
}

#[test]
fn native_name_wires_and_sql_match_frozen_cpp() -> rustdb_common::Result<()> {
    let mut count = 0;
    for line in include_str!("data/native-names-1.5.6.tsv").lines() {
        let fields: Vec<_> = line.split('\t').collect();
        let encoded = decode(fields[2]);
        let expected = decode(fields[3]);
        match fields[0] {
            "query" => compare::<SelectStatement>(&encoded, &expected, SelectStatement::to_sql_bytes),
            "type" => compare::<LogicalType>(&encoded, &expected, LogicalType::to_bytes),
            "value" => compare::<Value>(&encoded, &expected, Value::to_sql_string),
            "map" => compare::<CaseInsensitiveMap<u64, ByteString>>(&encoded, &expected, |map| {
                let name = ByteString::from(decode(fields[1]));
                assert_eq!(map.get(name.to_ascii_lowercase()), Some(&43));
                assert_eq!(map.iter().next().map(|(key, _)| key), Some(&name));
                map.len().to_string().into()
            }),
            _ => unreachable!(),
        }?;
        count += 1;
    }
    assert_eq!(count, 20);
    Ok(())
}
