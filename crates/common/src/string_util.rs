//! String similarity for "did you mean" suggestions (DuckDB's `StringUtil::SimilarityRating`,
//! `TopNStrings`, `TopNLevenshtein`, `TopNJaroWinkler`, `CandidatesMessage`).

use std::cmp::Ordering;

/// Jaro-Winkler similarity of two byte strings in `[0, 1]` (the `jaro_winkler` library DuckDB
/// vendors: prefix weight 0.1 over at most 4 characters, applied above a Jaro score of 0.7).
pub fn similarity_rating(left: &str, right: &str) -> f64 {
    let (p, t) = (left.as_bytes(), right.as_bytes());
    let prefix = p.iter().zip(t).take(4).take_while(|(a, b)| a == b).count();
    let similarity = jaro_similarity(p, t);
    if similarity > 0.7 {
        similarity + prefix as f64 * 0.1 * (1.0 - similarity)
    } else {
        similarity
    }
}

/// Jaro similarity with the vendored library's windowing (bound from the longer string,
/// out-of-window tails trimmed, common prefix matched up front).
fn jaro_similarity(p: &[u8], t: &[u8]) -> f64 {
    let (p_len, t_len) = (p.len(), t.len());
    if p_len == 0 || t_len == 0 {
        return 0.0;
    }
    if p_len == 1 && t_len == 1 {
        return f64::from(u8::from(p[0] == t[0]));
    }
    let (mut p_view, mut t_view) = (p, t);
    let bound = if t_len > p_len {
        let bound = t_len / 2 - 1;
        if t_len > p_len + bound {
            t_view = &t[..p_len + bound];
        }
        bound
    } else {
        let bound = p_len / 2 - 1;
        if p_len > t_len + bound {
            p_view = &p[..t_len + bound];
        }
        bound
    };
    let prefix = p_view.iter().zip(t_view).take_while(|(a, b)| a == b).count();
    let (p_rest, t_rest) = (&p_view[prefix..], &t_view[prefix..]);
    let mut p_flagged = vec![false; p_rest.len()];
    let mut t_flagged = vec![false; t_rest.len()];
    let mut common = prefix;
    for (j, &byte) in t_rest.iter().enumerate() {
        let low = j.saturating_sub(bound);
        let high = (j + bound + 1).min(p_rest.len());
        if let Some(i) = (low..high).find(|&i| !p_flagged[i] && p_rest[i] == byte) {
            p_flagged[i] = true;
            t_flagged[j] = true;
            common += 1;
        }
    }
    if common == 0 {
        return 0.0;
    }
    let p_matches = p_rest.iter().zip(&p_flagged).filter(|(_, f)| **f).map(|(b, _)| b);
    let t_matches = t_rest.iter().zip(&t_flagged).filter(|(_, f)| **f).map(|(b, _)| b);
    let transpositions = p_matches.zip(t_matches).filter(|(a, b)| a != b).count() / 2;
    let common = common as f64;
    (common / p_len as f64 + common / t_len as f64 + (common - transpositions as f64) / common) / 3.0
}

/// Levenshtein distance after ASCII lower-casing (`StringUtil::Lower`), with a configurable
/// substitution cost (DuckDB's `StringUtil::LevenshteinDistance`). Works on bytes, like DuckDB.
pub fn levenshtein_distance(left: &[u8], right: &[u8], not_equal_penalty: usize) -> usize {
    let (s1, s2) = (left.to_ascii_lowercase(), right.to_ascii_lowercase());
    if s1.is_empty() {
        return s2.len();
    }
    if s2.is_empty() {
        return s1.len();
    }
    let mut previous: Vec<usize> = (0..=s2.len()).collect();
    let mut current = vec![0; s2.len() + 1];
    for (i, &a) in s1.iter().enumerate() {
        current[0] = i + 1;
        for (j, &b) in s2.iter().enumerate() {
            let substitution = previous[j] + if a == b { 0 } else { not_equal_penalty };
            current[j + 1] = (previous[j + 1] + 1).min(current[j] + 1).min(substitution);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[s2.len()]
}

/// The edit distance used to rank suggestions: Levenshtein with substitutions costing 3
/// (DuckDB's `StringUtil::SimilarityScore`).
pub fn similarity_score(left: &[u8], right: &[u8]) -> usize {
    levenshtein_distance(left, right, 3)
}

/// DuckDB's `StringUtil::CILessThan` (shorter strings compare as if NUL-terminated).
fn ci_less_than(left: &str, right: &str) -> bool {
    let (a, b) = (left.as_bytes(), right.as_bytes());
    let length = a.len().min(b.len()) + usize::from(a.len() != b.len());
    let upper = |bytes: &[u8], i: usize| bytes.get(i).map_or(0, u8::to_ascii_uppercase);
    let (mut u1, mut u2) = (0u8, 0u8);
    for i in 0..length {
        u1 = upper(a, i);
        u2 = upper(b, i);
        if u1 != u2 {
            break;
        }
    }
    u1 < u2
}

/// The best-scoring strings, at least one and at most `n`, keeping only those scoring at
/// least `threshold` after the first (DuckDB's `StringUtil::TopNStrings`).
pub fn top_n_strings(mut scores: Vec<(String, f64)>, n: usize, threshold: f64) -> Vec<String> {
    if scores.is_empty() {
        return Vec::new();
    }
    scores.sort_by(|a, b| {
        match b.1.partial_cmp(&a.1) {
            Some(Ordering::Equal) => {}
            Some(order) => return order,
            // NaN scores compare unequal to everything and sort as ties.
            None => return Ordering::Equal,
        }
        if ci_less_than(&a.0, &b.0) {
            Ordering::Less
        } else if ci_less_than(&b.0, &a.0) {
            Ordering::Greater
        } else {
            Ordering::Equal
        }
    });
    let mut result = vec![scores[0].0.clone()];
    for (name, score) in scores.into_iter().take(n).skip(1) {
        if score < threshold {
            break;
        }
        result.push(name);
    }
    result
}

/// [`top_n_strings`] over distances (lower is better) normalised to similarities against
/// the largest distance and `threshold` (DuckDB's `TopNStrings(vector<pair<string, idx_t>>)`).
pub fn top_n_distances(scores: Vec<(String, usize)>, n: usize, threshold: usize) -> Vec<String> {
    let max_score = scores.iter().map(|(_, s)| *s).max().unwrap_or(0).max(threshold);
    let normalize = |score: usize| 1.0 - score as f64 / max_score as f64;
    let normalized = scores
        .into_iter()
        .map(|(name, score)| (name, normalize(score)))
        .collect();
    top_n_strings(normalized, n, normalize(threshold))
}

/// The strings closest to `target` by prefix Levenshtein distance
/// (DuckDB's `StringUtil::TopNLevenshtein` with `n = 5`, `threshold = 5`).
pub fn top_n_levenshtein(strings: &[String], target: &str) -> Vec<String> {
    let scores = strings
        .iter()
        .map(|candidate| {
            let candidate_bytes = candidate.as_bytes();
            let compared = &candidate_bytes[..target.len().min(candidate_bytes.len())];
            (candidate.clone(), similarity_score(compared, target.as_bytes()))
        })
        .collect();
    top_n_distances(scores, 5, 5)
}

/// The strings most similar to `target` by Jaro-Winkler similarity
/// (DuckDB's `StringUtil::TopNJaroWinkler` with `n = 5`, `threshold = 0.5`).
pub fn top_n_jaro_winkler(strings: &[String], target: &str) -> Vec<String> {
    let scores = strings
        .iter()
        .map(|candidate| (candidate.clone(), similarity_rating(candidate, target)))
        .collect();
    top_n_strings(scores, 5, 0.5)
}

/// `\n<label>: "a", "b"`, or `""` without candidates (DuckDB's `StringUtil::CandidatesMessage`).
pub fn candidates_message(candidates: &[String], label: &str) -> String {
    if candidates.is_empty() {
        return String::new();
    }
    let quoted: Vec<String> = candidates.iter().map(|c| format!("\"{c}\"")).collect();
    format!("\n{label}: {}", quoted.join(", "))
}

/// The Levenshtein candidates message (DuckDB's `StringUtil::CandidatesErrorMessage`).
pub fn candidates_error_message(strings: &[String], target: &str, label: &str) -> String {
    candidates_message(&top_n_levenshtein(strings, target), label)
}

/// `value` as a JSON string literal, escaped as yyjson writes it: quotes, backslashes
/// and control characters are escaped, everything else is kept as UTF-8.
pub fn json_quote(value: &str) -> String {
    let mut result = String::with_capacity(value.len() + 2);
    result.push('"');
    for c in value.chars() {
        match c {
            '"' => result.push_str("\\\""),
            '\\' => result.push_str("\\\\"),
            '\n' => result.push_str("\\n"),
            '\r' => result.push_str("\\r"),
            '\t' => result.push_str("\\t"),
            '\u{8}' => result.push_str("\\b"),
            '\u{c}' => result.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                use std::fmt::Write;
                let _ = write!(result, "\\u{:04x}", c as u32);
            }
            c => result.push(c),
        }
    }
    result.push('"');
    result
}

/// yyjson escapes syntax/control bytes and preserves all other bytes, even when an
/// exception message is not UTF-8. The resulting wire JSON can therefore be non-UTF-8.
pub fn json_quote_bytes(value: &[u8]) -> Vec<u8> {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut result = Vec::with_capacity(value.len() + 2);
    result.push(b'"');
    for &byte in value {
        match byte {
            b'"' => result.extend_from_slice(b"\\\""),
            b'\\' => result.extend_from_slice(b"\\\\"),
            b'\n' => result.extend_from_slice(b"\\n"),
            b'\r' => result.extend_from_slice(b"\\r"),
            b'\t' => result.extend_from_slice(b"\\t"),
            8 => result.extend_from_slice(b"\\b"),
            12 => result.extend_from_slice(b"\\f"),
            byte if byte < 0x20 => {
                result.extend_from_slice(b"\\u00");
                result.push(HEX[(byte >> 4) as usize]);
                result.push(HEX[(byte & 15) as usize]);
            }
            byte => result.push(byte),
        }
    }
    result.push(b'"');
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn similarity_matches_jaro_winkler() {
        assert!((similarity_rating("martha", "marhta") - 0.961_111).abs() < 1e-5);
        assert!((similarity_rating("dixon", "dicksonx") - 0.813_333).abs() < 1e-5);
        assert_eq!(similarity_rating("", "a"), 0.0);
        assert_eq!(similarity_rating("abc", "abc"), 1.0);
        assert_eq!(levenshtein_distance(b"Kitten", b"sitting", 1), 3);
    }

    #[test]
    fn top_n_prefers_scores_then_names() {
        let scores = vec![
            ("b".to_owned(), 0.9),
            ("A".to_owned(), 0.9),
            ("c".to_owned(), 0.1),
        ];
        assert_eq!(top_n_strings(scores, 5, 0.5), vec!["A", "b"]);
        assert_eq!(
            candidates_message(&["x".to_owned(), "y".to_owned()], "Candidate bindings"),
            "\nCandidate bindings: \"x\", \"y\""
        );
    }
}
