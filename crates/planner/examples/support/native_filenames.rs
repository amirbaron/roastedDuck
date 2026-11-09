//! Frozen C++ filename-pattern metadata and deterministic generated paths.
use roastedduck_common::{ByteString, Error, Result};
use roastedduck_planner::operator::FilenamePattern;
use roastedduck_serde::{SerializationOptions, Serializer, binary::BinarySerializer};
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
    if f.len() == 2 {
        let path = ByteString::from(unhex(f[0])?).replace_byte(b'\\', b"/");
        if roastedduck_ast::keyword::sql_string_bytes(&path).as_bytes() != unhex(f[1])? {
            return Err(Error::internal("export path SQL literal differs"));
        }
        return Ok(());
    }
    if f.len() != 4 {
        return Err(Error::invalid_input("invalid filename oracle record"));
    }
    let name = ByteString::from(unhex(f[1])?);
    let suffix = match f[0] {
        "0" => b"".as_slice(),
        "1" => b"{i}",
        "2" => b"{i}/{i}",
        "3" => b"{uuid}",
        "4" => b"{uuidv7}",
        "5" => b"{other}",
        _ => return Err(Error::invalid_input("invalid filename mode")),
    };
    let text = roastedduck_common::byte_format!("{name}{suffix}");
    let mut pattern = FilenamePattern::default();
    pattern.set_pattern(&text);
    let mut w = BinarySerializer::new(SerializationOptions::default());
    w.begin();
    (&mut w as &mut dyn Serializer).write_property(100, "payload", &pattern)?;
    w.end();
    if w.bytes()
        != unhex(
            f[2].strip_prefix("OK:")
                .ok_or_else(|| Error::internal("C++ filename error"))?,
        )?
    {
        return Err(Error::internal("filename pattern metadata differs"));
    }
    let rendered = if pattern.has_uuid() {
        ByteString::from("UUID")
    } else {
        pattern.create_filename(&name, "csv", 42)
    };
    if rendered.as_bytes() != unhex(f[3])? {
        return Err(Error::internal("filename pattern path differs"));
    }
    Ok(())
}
