//! Frozen C++ native path expansion and canonicalization comparisons.
use roastedduck_common::{ByteString, Error, Result};
use roastedduck_function::file_system::{
    canonicalize_local_path_bytes, canonicalize_path_bytes, expand_path_bytes,
};
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
    if fields.len() != 3 {
        return Err(Error::invalid_input("invalid path oracle record"));
    }
    let path = unhex(fields[1])?;
    let actual: ByteString = match fields[0] {
        "0" => expand_path_bytes(&path),
        "1" => canonicalize_path_bytes(&path),
        "2" => canonicalize_local_path_bytes(&path),
        _ => return Err(Error::invalid_input("invalid path oracle mode")),
    };
    let expected = fields[2]
        .strip_prefix("OK:")
        .ok_or_else(|| Error::internal("unexpected C++ path error"))?;
    if actual.as_bytes() != unhex(expected)? {
        return Err(Error::internal("native path bytes differ"));
    }
    Ok(())
}
