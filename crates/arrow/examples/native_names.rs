//! Actual Arrow C-string schema export/import against C++ 1.5.6.
use rustdb_arrow::{
    ArrowTableSchema, ClientProperties,
    ffi::{ArrowSchema, ArrowSchemaWrapper},
    populate_arrow_table_schema, to_arrow_schema,
};
use rustdb_common::{ByteString, Error, Result};
use rustdb_types::LogicalType;
use std::fmt::Write as _;
use std::io::{self, BufRead};
pub fn decode(hex: &str) -> Result<Vec<u8>> {
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
fn hex(bytes: &[u8]) -> String {
    const D: &[u8] = b"0123456789abcdef";
    bytes
        .iter()
        .flat_map(|&b| [char::from(D[(b >> 4) as usize]), char::from(D[(b & 15) as usize])])
        .collect()
}
fn dump(schema: &ArrowSchema, out: &mut String) {
    // SAFETY: only schemas exported and owned by our live ArrowSchemaWrapper reach here.
    let name = unsafe { schema.name_string() };
    let format = unsafe { schema.format_str() };
    let _ = write!(out, "{}:{format}:{}[", hex(&name), schema.flags);
    // SAFETY: this is a valid exported C schema with live children.
    for child in unsafe { schema.child_schemas() } {
        dump(child, out);
    }
    out.push(']');
    // SAFETY: a non-null dictionary is owned by the same exported schema wrapper.
    if let Some(dictionary) = unsafe { schema.dictionary.as_ref() } {
        out.push('D');
        dump(dictionary, out);
    }
}
fn outcome(mode: &str, name: ByteString) -> Result<ByteString> {
    let (types, names) = if mode == "import" {
        (
            vec![LogicalType::INTEGER, LogicalType::INTEGER],
            vec![name.clone(), name],
        )
    } else {
        (
            vec![
                LogicalType::INTEGER,
                LogicalType::struct_type(vec![(name.clone(), LogicalType::INTEGER)]),
                LogicalType::union_type(vec![(name.clone(), LogicalType::INTEGER)]),
            ],
            vec![name, "nested".into(), "union".into()],
        )
    };
    let mut schema = ArrowSchemaWrapper::default();
    to_arrow_schema(
        &mut schema.arrow_schema,
        &types,
        &names,
        &ClientProperties::default(),
    )?;
    if mode == "export" {
        let mut out = String::new();
        dump(&schema.arrow_schema, &mut out);
        return Ok(out.into());
    }
    let mut imported = ArrowTableSchema::default();
    // SAFETY: schema is a live exported struct schema owned for the whole call.
    unsafe { populate_arrow_table_schema(&mut imported, &schema.arrow_schema) }?;
    if mode == "nested-import" {
        use rustdb_serde::{SerializationOptions, Serializer};
        let mut writer = rustdb_serde::binary::BinarySerializer::new(SerializationOptions::default());
        writer.begin();
        (&mut writer as &mut dyn Serializer).write_property(100, "payload", &imported.types().to_vec())?;
        writer.end();
        return Ok(hex(writer.bytes()).into());
    }
    let mut out = String::new();
    for name in imported.names() {
        let _ = write!(out, "{};", hex(name));
    }
    Ok(out.into())
}
pub fn compare(line: &str) -> Result<()> {
    let f: Vec<_> = line.split('\t').collect();
    if f.len() != 4 {
        return Err(Error::invalid_input("invalid Arrow native record"));
    }
    let result = outcome(f[0], decode(f[1])?.into());
    let (status, bytes) = match result {
        Ok(bytes) => ("ok", bytes),
        Err(error) => ("error", error.raw_message_bytes().into()),
    };
    if status != f[2] || bytes.as_bytes() != decode(f[3])? {
        return Err(Error::internal(format!(
            "Arrow native mismatch {} name {} ({status}), output {:?}",
            f[0],
            f[1],
            bytes.as_bytes()
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
    println!("{count} C++ Arrow native-name schema comparisons passed");
    Ok(())
}
