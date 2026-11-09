//! Compares typed byte-string properties with a C++ `BinarySerializer` oracle.
//! Input: payload, required-property and default-property hex, tab-separated.

use std::io::{self, BufRead};

use roastedduck_common::ByteString;
use roastedduck_serde::binary::{BinaryDeserializer, BinarySerializer};
use roastedduck_serde::json::{JsonDeserializer, JsonOptions, JsonSerializer, try_write_json, write_json};
use roastedduck_serde::{Deserializer, SerializationOptions, Serializer};

fn decode(hex: &str) -> Vec<u8> {
    hex.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let digit = |byte: u8| match byte {
                b'0'..=b'9' => byte - b'0',
                b'a'..=b'f' => byte - b'a' + 10,
                _ => unreachable!("oracle output is hexadecimal"),
            };
            digit(pair[0]) * 16 + digit(pair[1])
        })
        .collect()
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut count = 0;
    for line in io::stdin().lock().lines() {
        let line = line?;
        let fields: Vec<_> = line.split('\t').collect();
        assert!(fields.len() == 3 || fields.len() == 4);
        let bytes = decode(fields[0]);
        let value = ByteString::from(bytes.clone());
        for (with_default, encoded) in [(false, fields[1]), (true, fields[2])] {
            let mut writer = BinarySerializer::new(SerializationOptions::default());
            writer.begin();
            if with_default {
                (&mut writer as &mut dyn Serializer).write_property_with_default(100, "name", &value)?;
            } else {
                (&mut writer as &mut dyn Serializer).write_property(100, "name", &value)?;
            }
            writer.end();
            let encoded = decode(encoded);
            assert_eq!(writer.bytes(), encoded);
            let mut reader = BinaryDeserializer::new(&encoded);
            reader.begin()?;
            let decoded: ByteString = if with_default {
                (&mut reader as &mut dyn Deserializer).read_property_with_default(100, "name")?
            } else {
                (&mut reader as &mut dyn Deserializer).read_property(100, "name")?
            };
            reader.end()?;
            assert_eq!(decoded.as_bytes(), bytes);
            if !with_default {
                let mut reader = BinaryDeserializer::new(&encoded);
                reader.begin()?;
                let text: roastedduck_serde::Result<String> =
                    (&mut reader as &mut dyn Deserializer).read_property(100, "name");
                assert_eq!(text.is_ok(), std::str::from_utf8(&bytes).is_ok());
            }
        }
        if fields.len() == 4 {
            let mut serializer = JsonSerializer::new(JsonOptions::default(), SerializationOptions::default());
            (&mut serializer as &mut dyn Serializer).write_property(100, "name", &value)?;
            let document = serializer.finish_document()?;
            let mut reader = JsonDeserializer::new(&document);
            let decoded: ByteString = (&mut reader as &mut dyn Deserializer).read_property(100, "name")?;
            assert_eq!(decoded.as_bytes(), bytes);
            let text = try_write_json(&document, false);
            if fields[3] == "ERROR" {
                assert!(text.is_err());
                assert_eq!(write_json(&document, false), "");
            } else {
                assert_eq!(text?.as_bytes(), decode(fields[3]));
            }
        }
        count += 1;
    }
    println!("{count} C++ byte-string property cases matched; strict text decoding unchanged");
    Ok(())
}
