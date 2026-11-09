//! Frozen C++ native exception metadata and JSON comparison.
use roastedduck_common::{ByteString, Error, Result};
fn decode(hex: &str) -> Result<Vec<u8>> {
    if !hex.len().is_multiple_of(2) {
        return Err(Error::invalid_input("odd oracle hex length"));
    }
    hex.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(
            |pair| match (char::from(pair[0]).to_digit(16), char::from(pair[1]).to_digit(16)) {
                (Some(a), Some(b)) => Ok((a * 16 + b) as u8),
                _ => Err(Error::invalid_input("invalid oracle hex")),
            },
        )
        .collect()
}
pub fn compare(line: &str) -> Result<()> {
    let fields: Vec<_> = line.split('\t').collect();
    let name = ByteString::from(decode(fields[1])?);
    let mut error = Error::binder("m").with_extra_info("name", &name);
    if fields[0] == "1" {
        error = error.with_position(Some(7));
    }
    assert_eq!(
        error
            .extra_info()
            .into_iter()
            .find(|(key, _)| *key == "name")
            .map(|(_, value)| value),
        Some(name.as_bytes())
    );
    assert_eq!(
        error.into_json().to_bytes(),
        decode(fields[2])?,
        "native exception metadata JSON"
    );
    Ok(())
}
