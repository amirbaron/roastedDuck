//! Frozen C++ local glob expansion with native path bytes.
use rustdb_common::{ByteString, Error, Result};
use rustdb_csv::multi_file::local_glob;
use rustdb_serde::{SerializationOptions, Serializer, binary::BinarySerializer};
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
    if f.len() != 2 {
        return Err(Error::invalid_input("invalid glob oracle record"));
    }
    let path = unhex(f[0])?;
    let actual: Result<ByteString> = (|| {
        let files = local_glob(&path)?;
        let mut w = BinarySerializer::new(SerializationOptions::default());
        w.begin();
        (&mut w as &mut dyn Serializer).write_property(100, "payload", &files)?;
        w.end();
        Ok(w.into_bytes().into())
    })();
    match actual {
        Ok(value) => {
            let expected = f[1]
                .strip_prefix("OK:")
                .ok_or_else(|| Error::internal("C++ glob failed, Rust succeeded"))?;
            if value.as_bytes() != unhex(expected)? {
                return Err(Error::internal("native glob file-list bytes differ"));
            }
        }
        Err(error) => {
            let expected = f[1]
                .strip_prefix("ERR:")
                .ok_or_else(|| Error::internal("C++ glob succeeded, Rust failed"))?;
            if error.into_json().to_bytes() != unhex(expected)? {
                return Err(Error::internal("native glob error bytes differ"));
            }
        }
    }
    Ok(())
}
