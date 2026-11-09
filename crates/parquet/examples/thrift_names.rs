//! Native Parquet names against C++ Thrift compact-protocol wire bytes.
use roastedduck_common::{ByteString, Error, Result};
use roastedduck_parquet::thrift::{
    self, ColumnMetaData, CompressionCodec, Encoding, EncryptionWithColumnKey, FieldRepetitionType,
    SchemaElement, ThriftValue, Type,
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
fn check<T: ThriftValue + PartialEq + std::fmt::Debug>(value: &T, expected: &[u8]) -> Result<()> {
    if thrift::write_to_vec(value) != expected {
        return Err(Error::internal("Parquet native Thrift encoding differs from C++"));
    }
    let (restored, used) = thrift::read_from::<T>(expected)?;
    if &restored != value || used != expected.len() {
        return Err(Error::internal("Parquet native Thrift decode differs from C++"));
    }
    Ok(())
}
pub fn compare(line: &str) -> Result<()> {
    let f: Vec<_> = line.split('\t').collect();
    if f.len() != 3 {
        return Err(Error::invalid_input("invalid Parquet name record"));
    }
    let name = ByteString::from(decode(f[1])?);
    let expected = decode(f[2])?;
    match f[0] {
        "schema" => check(
            &SchemaElement {
                name,
                type_: Some(Type::INT32),
                repetition_type: Some(FieldRepetitionType::OPTIONAL),
                ..SchemaElement::default()
            },
            &expected,
        ),
        "column" => check(
            &ColumnMetaData {
                type_: Type::INT32,
                encodings: vec![Encoding::PLAIN],
                path_in_schema: vec![name, "nested".into()],
                codec: CompressionCodec::UNCOMPRESSED,
                num_values: 2,
                total_uncompressed_size: 8,
                total_compressed_size: 8,
                data_page_offset: 4,
                ..ColumnMetaData::default()
            },
            &expected,
        ),
        "encryption-path" => check(
            &EncryptionWithColumnKey {
                path_in_schema: vec![name],
                ..EncryptionWithColumnKey::default()
            },
            &expected,
        ),
        _ => Err(Error::invalid_input("unknown Parquet name record")),
    }
}
fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut count = 0;
    for line in io::stdin().lock().lines() {
        compare(&line?)?;
        count += 1;
    }
    println!("{count} C++ Parquet Thrift name comparisons passed");
    Ok(())
}
