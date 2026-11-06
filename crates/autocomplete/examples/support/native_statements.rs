//! Native parser statement codecs checked against frozen C++ 1.5.6.
use rustdb_ast::{CopyInfo, ParseInfo, SqlStatementKind};
use rustdb_common::{Error, Result};
use rustdb_serde::{
    Deserialize, Deserializer, SerializationOptions, Serialize, Serializer,
    binary::{BinaryDeserializer, BinarySerializer},
};
fn unhex(text: &str) -> Result<Vec<u8>> {
    let (pairs, rest) = text.as_bytes().as_chunks::<2>();
    if !rest.is_empty() {
        return Err(Error::invalid_input("odd oracle hex length"));
    }
    pairs
        .iter()
        .map(|pair| {
            let digit = |byte| match byte {
                b'0'..=b'9' => Ok(byte - b'0'),
                b'a'..=b'f' => Ok(byte - b'a' + 10),
                _ => Err(Error::invalid_input("invalid oracle hex digit")),
            };
            Ok(digit(pair[0])? * 16 + digit(pair[1])?)
        })
        .collect()
}
fn wire<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    let mut writer = BinarySerializer::new(SerializationOptions::default());
    writer.begin();
    (&mut writer as &mut dyn Serializer).write_property(100, "payload", value)?;
    writer.end();
    Ok(writer.into_bytes())
}
fn round_trip<T: Serialize + Deserialize>(bytes: &[u8]) -> Result<()> {
    let mut reader = BinaryDeserializer::new(bytes);
    reader.begin()?;
    let value: T = (&mut reader as &mut dyn Deserializer).read_property(100, "payload")?;
    reader.end()?;
    if wire(&value)? != bytes {
        return Err(Error::internal("native statement read/write round trip differs"));
    }
    Ok(())
}
fn copy_round_trip(bytes: &[u8]) -> Result<()> {
    let mut reader = BinaryDeserializer::new(bytes);
    reader.begin()?;
    let deserializer = &mut reader as &mut dyn Deserializer;
    let value: ParseInfo = deserializer.read_property(100, "payload")?;
    let names: Vec<rustdb_common::ByteString> = deserializer.read_property(101, "parsed_names")?;
    reader.end()?;
    let mut writer = BinarySerializer::new(SerializationOptions::default());
    writer.begin();
    let serializer = &mut writer as &mut dyn Serializer;
    serializer.write_property(100, "payload", &value)?;
    serializer.write_property(101, "parsed_names", &names)?;
    writer.end();
    if writer.bytes() != bytes {
        return Err(Error::internal("native COPY codec round trip differs"));
    }
    Ok(())
}
fn copy_wire(value: &CopyInfo) -> Result<Vec<u8>> {
    let mut writer = BinarySerializer::new(SerializationOptions::default());
    writer.begin();
    let serializer = &mut writer as &mut dyn Serializer;
    serializer.write_property(100, "payload", &ParseInfo::Copy(Box::new(value.clone())))?;
    let names: Vec<_> = value.parsed_options.iter().map(|(name, _)| name).collect();
    serializer.write_property(101, "parsed_names", &names)?;
    writer.end();
    Ok(writer.into_bytes())
}
pub fn compare(line: &str) -> Result<()> {
    let fields: Vec<_> = line.split('\t').collect();
    if fields.len() < 3 {
        return Err(Error::invalid_input("invalid statement oracle record"));
    }
    let query = unhex(fields[1])?;
    let text =
        std::str::from_utf8(&query).map_err(|_| Error::invalid_input("PEG probe requires UTF-8 query"))?;
    let result = rustdb_autocomplete::parse_query(text, rustdb_parser::ParserOptions::default());
    if fields[0] == "error" {
        let error = result
            .err()
            .ok_or_else(|| Error::internal("C++ rejected, Rust accepted"))?;
        if error.into_json().to_bytes() != unhex(fields[2])? {
            return Err(Error::internal(
                "native statement exception bytes/metadata differ",
            ));
        }
        return Ok(());
    }
    let statements = result?;
    if fields[0] == "count" {
        if statements.len().to_string() != fields[2] {
            return Err(Error::internal("statement count differs"));
        }
        return Ok(());
    }
    let [statement] = statements.as_slice() else {
        return Err(Error::internal("expected one statement"));
    };
    let (kind, actual) = match &statement.kind {
        SqlStatementKind::Select(value) => ("select", wire(value)?),
        SqlStatementKind::Create(value) => ("create", wire(value)?),
        SqlStatementKind::Alter(value) => ("alter", wire(&ParseInfo::Alter(value.clone()))?),
        SqlStatementKind::Copy(value) => ("copy", copy_wire(value)?),
        _ => return Err(Error::internal("unexpected statement kind")),
    };
    if kind != fields[0] {
        return Err(Error::internal("native statement kind differs"));
    }
    let expected = unhex(fields[2])?;
    if actual != expected {
        return Err(Error::internal("native statement wire differs"));
    }
    match kind {
        "select" => round_trip::<rustdb_ast::SelectStatement>(&expected)?,
        "create" => round_trip::<rustdb_ast::CreateInfo>(&expected)?,
        "alter" => round_trip::<ParseInfo>(&expected)?,
        "copy" => copy_round_trip(&expected)?,
        _ => unreachable!(),
    }
    if statement.query.as_bytes() != unhex(fields[3])? {
        return Err(Error::internal("native statement source differs"));
    }
    Ok(())
}
