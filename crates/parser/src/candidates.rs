//! "Did you mean" candidates for unrecognized names (DuckDB's
//! `StringUtil::TopNJaroWinkler` and `StringUtil::CandidatesMessage`).

use roastedduck_common::Error;

/// The Jaro similarity of two byte strings (rapidfuzz's `jaro_similarity`, as vendored
/// by DuckDB).
fn jaro(p: &[u8], t: &[u8]) -> f64 {
    if p.is_empty() || t.is_empty() {
        return 0.0;
    }
    if p.len() == 1 && t.len() == 1 {
        return f64::from(u8::from(p[0] == t[0]));
    }
    let bound = (p.len().max(t.len()) / 2).saturating_sub(1);
    let mut p_flag = vec![false; p.len()];
    let mut t_flag = vec![false; t.len()];
    let mut common = 0_usize;
    for (j, &t_char) in t.iter().enumerate() {
        let low = j.saturating_sub(bound);
        let high = (j + bound).min(p.len() - 1);
        if low > high {
            continue;
        }
        if let Some(i) = (low..=high).find(|&i| !p_flag[i] && p[i] == t_char) {
            p_flag[i] = true;
            t_flag[j] = true;
            common += 1;
        }
    }
    if common == 0 {
        return 0.0;
    }
    let mut transpositions = 0_usize;
    let mut k = 0;
    for (j, &t_char) in t.iter().enumerate() {
        if !t_flag[j] {
            continue;
        }
        while !p_flag[k] {
            k += 1;
        }
        if p[k] != t_char {
            transpositions += 1;
        }
        k += 1;
    }
    let transpositions = transpositions / 2;
    let common = common as f64;
    (common / p.len() as f64 + common / t.len() as f64 + (common - transpositions as f64) / common) / 3.0
}

/// `jaro_winkler_similarity` with the default prefix weight 0.1.
fn jaro_winkler(p: &[u8], t: &[u8]) -> f64 {
    let max_prefix = p.len().min(t.len()).min(4);
    let prefix = (0..max_prefix).take_while(|&i| p[i] == t[i]).count();
    let mut similarity = jaro(p, t);
    if similarity > 0.7 {
        similarity += prefix as f64 * 0.1 * (1.0 - similarity);
    }
    similarity
}

/// `StringUtil::TopNJaroWinkler(strings, target)`: the best match, plus up to four more
/// scoring at least 0.5.
fn top_n_jaro_winkler<'s>(strings: &[&'s str], target: &[u8]) -> Vec<&'s str> {
    let mut scores: Vec<(&str, f64)> = strings
        .iter()
        .map(|candidate| (*candidate, jaro_winkler(candidate.as_bytes(), target)))
        .collect();
    scores.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.to_ascii_lowercase().cmp(&b.0.to_ascii_lowercase()))
    });
    let mut result = Vec::new();
    for (index, (candidate, score)) in scores.into_iter().enumerate().take(5) {
        if index > 0 && score < 0.5 {
            break;
        }
        result.push(candidate);
    }
    result
}

/// `StringUtil::CandidatesMessage`: `\n<label>: "a", "b"`, or empty.
fn candidates_message(candidates: &[&str], label: &str) -> String {
    if candidates.is_empty() {
        return String::new();
    }
    let quoted: Vec<String> = candidates
        .iter()
        .map(|candidate| format!("\"{candidate}\""))
        .collect();
    format!("\n{label}: {}", quoted.join(", "))
}

/// The error of `StringUtil::StringToEnum` for a value that names no member.
pub(crate) fn enum_error(value: impl AsRef<[u8]>, enum_name: &str, names: &[&str]) -> Error {
    let value = value.as_ref();
    let closest = top_n_jaro_winkler(names, value);
    Error::not_implemented(roastedduck_common::byte_format!(
        "Enum value: unrecognized value \"{value}\" for enum \"{enum_name}\"\n{}",
        candidates_message(&closest, "Candidates")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jaro_winkler_matches_reference_values() {
        assert!((jaro(b"MARTHA", b"MARHTA") - 0.944_444).abs() < 1e-5);
        assert!((jaro_winkler(b"MARTHA", b"MARHTA") - 0.961_111).abs() < 1e-5);
        assert!((jaro_winkler(b"DIXON", b"DICKSONX") - 0.813_333).abs() < 1e-5);
    }
}
