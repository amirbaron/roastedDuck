//! C++-recorded hive parsing, URL decoding and typed partition values.
use roastedduck_common::{ByteString, Error, Result};
use roastedduck_csv::multi_file::{hive_parse, hive_value, url_decode};
use roastedduck_function::cast::CastFunctionSet;
use roastedduck_serde::{SerializationOptions, Serializer, binary::BinarySerializer};
use roastedduck_types::LogicalType;
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
    let f: Vec<_> = line.split('\t').collect();
    if f.len() != 3 {
        return Err(Error::invalid_input("invalid hive oracle record"));
    }
    let name = unhex(f[1])?;
    let actual: Result<ByteString> = (|| match f[0] {
        "0" | "1" => {
            let path = if f[0] == "0" {
                roastedduck_common::byte_format!("root/{name}=v/part={name}/file.csv")
            } else {
                roastedduck_common::byte_format!("root/k={name}/k=other/file.csv")
            };
            let parts = hive_parse(&path);
            let mut w = BinarySerializer::new(SerializationOptions::default());
            w.begin();
            (&mut w as &mut dyn Serializer).write_property(100, "payload", &parts)?;
            w.end();
            Ok(w.into_bytes().into())
        }
        "2" => url_decode(&name),
        "3" | "4" => {
            let ty = if f[0] == "3" {
                LogicalType::VARCHAR
            } else {
                LogicalType::BIGINT
            };
            hive_value(&name, &name, &ty, &CastFunctionSet::new())?.try_to_sql_string()
        }
        _ => Err(Error::invalid_input("invalid hive oracle mode")),
    })();
    match actual {
        Ok(value) => {
            let expected = f[2]
                .strip_prefix("OK:")
                .ok_or_else(|| Error::internal("C++ hive failed, Rust succeeded"))?;
            if value.as_bytes() != unhex(expected)? {
                return Err(Error::internal("native hive value/wire differs"));
            }
        }
        Err(error) => {
            let expected = f[2]
                .strip_prefix("ERR:")
                .ok_or_else(|| Error::internal("C++ hive succeeded, Rust failed"))?;
            if error.into_json().to_bytes() != unhex(expected)? {
                return Err(Error::internal("native hive error differs"));
            }
        }
    }
    Ok(())
}
