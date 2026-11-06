//! Native ColumnIndex/StorageIndex binary round trips against frozen C++ 1.5.6.
use rustdb_common::{ByteString, Error, Result};
use rustdb_serde::binary::{BinaryDeserializer, BinarySerializer};
use rustdb_serde::{Deserialize, Deserializer, SerializationOptions, Serialize, Serializer};
use rustdb_storage::table::StorageIndex;
use rustdb_types::{ColumnIndex, ColumnIndexType, LogicalType};
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
fn check<T: Serialize + Deserialize + PartialEq>(expected: &T, wire: &[u8]) -> Result<()> {
    let mut reader = BinaryDeserializer::new(wire);
    reader.begin()?;
    let actual: T = (&mut reader as &mut dyn Deserializer).read_property(100, "payload")?;
    reader.end()?;
    if &actual != expected {
        return Err(Error::internal("native path metadata differs from C++"));
    }
    let mut writer = BinarySerializer::new(SerializationOptions::default());
    writer.begin();
    (&mut writer as &mut dyn Serializer).write_property(100, "payload", &actual)?;
    writer.end();
    if writer.bytes() != wire {
        return Err(Error::internal("native path wire differs from C++"));
    }
    Ok(())
}
pub fn compare(line: &str) -> Result<()> {
    let fields: Vec<_> = line.split('\t').collect();
    if fields.len() != 3 {
        return Err(Error::invalid_input("invalid path oracle record"));
    }
    let mode = fields[0]
        .as_bytes()
        .last()
        .copied()
        .ok_or_else(|| Error::invalid_input("missing mode"))?;
    let mut column = ColumnIndex::field(ByteString::from(decode(fields[1])?));
    if mode == b'2' {
        let mut nested = ColumnIndex::field("nested");
        nested.ty = LogicalType::variant();
        column.child_indexes.push(nested);
        column.ty = LogicalType::variant();
        column.index_type = ColumnIndexType::PushdownExtract;
    }
    if mode != b'0' {
        column = ColumnIndex::with_children(5, vec![column]);
    }
    if mode == b'2' {
        column.ty = LogicalType::variant();
        column.index_type = ColumnIndexType::PushdownExtract;
    }
    let wire = decode(fields[2])?;
    if fields[0].starts_with("column") {
        check(&column, &wire)
    } else if fields[0].starts_with("storage") {
        check(&StorageIndex::from_column_index(&column, column.index), &wire)
    } else {
        Err(Error::invalid_input("unknown path oracle kind"))
    }
}
fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut count = 0;
    for line in io::stdin().lock().lines() {
        compare(&line?)?;
        count += 1;
    }
    println!("{count} C++ path comparisons passed");
    Ok(())
}
