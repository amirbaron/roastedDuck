//! C++ byte-name lookup and candidate-suggestion comparisons.
use roastedduck_common::string_util::{candidates_message_bytes, similarity_rating, top_n_jaro_winkler};
use roastedduck_common::{ByteString, CaseInsensitiveSet, Error, Result};

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
    let name = ByteString::from(decode(fields[0])?);
    let lower = name.to_ascii_lowercase();
    let mut extended = name.clone();
    extended.push(b'X');
    let set: CaseInsensitiveSet<ByteString> = [name.clone(), lower.clone(), extended.clone()]
        .into_iter()
        .collect();
    assert_eq!(set.len().to_string(), fields[3], "case-insensitive set");
    let expected_score: f64 = fields[1]
        .parse()
        .map_err(|_| Error::invalid_input("invalid oracle score"))?;
    let actual_score = similarity_rating(&name, &extended);
    assert!(
        (expected_score - actual_score).abs() < 1e-15,
        "name similarity {expected_score} != {actual_score}"
    );
    let candidates = vec![name.clone(), extended, lower, "missing".into(), "duck".into()];
    let ranked = top_n_jaro_winkler(&candidates, &name);
    let message = candidates_message_bytes(&ranked, b"Candidate bindings");
    assert_eq!(
        message.as_bytes(),
        decode(fields[2])?,
        "ranked native candidate message"
    );
    Ok(())
}
