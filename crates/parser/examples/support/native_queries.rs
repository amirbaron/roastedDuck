//! Frozen C++ SELECT binary AST/original SQL and processed `ErrorData` JSON.
use roastedduck_ast::SqlStatementKind;
use roastedduck_common::{Error, Result};
use roastedduck_serde::binary::BinarySerializer;
use roastedduck_serde::{SerializationOptions, Serializer};
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
    let fields: Vec<_> = line.split('\t').collect();
    if fields.len() != 4 {
        return Err(Error::invalid_input("invalid query oracle record"));
    }
    let bytes = unhex(fields[1])?;
    let parsed = roastedduck_parser::parse_bytes(&bytes);
    if fields[2] == "ERROR" {
        let error = parsed
            .err()
            .ok_or_else(|| Error::internal("C++ rejected, Rust accepted"))?;
        if error.into_json().to_bytes() != unhex(fields[3])? {
            return Err(Error::internal("native parser error bytes/metadata differ"));
        }
        return Ok(());
    }
    let statements = parsed?;
    let [statement] = statements.as_slice() else {
        return Err(Error::internal("expected one statement"));
    };
    let SqlStatementKind::Select(select) = &statement.kind else {
        return Err(Error::internal("expected SELECT"));
    };
    let mut writer = BinarySerializer::new(SerializationOptions::default());
    writer.begin();
    (&mut writer as &mut dyn Serializer).write_property(100, "payload", select)?;
    writer.end();
    if writer.bytes() != unhex(fields[2])? {
        return Err(Error::internal("native SELECT wire differs"));
    }
    if select.to_sql_bytes().as_bytes() != unhex(fields[3])? {
        return Err(Error::internal("native SELECT SQL differs"));
    }
    Ok(())
}
