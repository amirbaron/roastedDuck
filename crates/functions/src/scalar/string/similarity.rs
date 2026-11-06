//! String similarity: `levenshtein`/`editdist3` (DuckDB's `levenshtein.cpp`),
//! `damerau_levenshtein`, `hamming`/`mismatches`, `jaccard`, `jaro_similarity` and
//! `jaro_winkler_similarity` (the rapidfuzz-based `jaro_winkler.cpp`). All work on bytes.

use rustdb_common::{Error, Result};
use rustdb_function::{FunctionState, ScalarFunction, ScalarFunctionSet};
use rustdb_types::{DataChunk, LogicalType, Vector};

use crate::scalar::executor::{Str, binary, ternary};

/// DuckDB's `LevenshteinDistance` (two-row dynamic programming over bytes).
fn levenshtein(text: &[u8], target: &[u8]) -> usize {
    if text.is_empty() {
        return target.len();
    }
    if target.is_empty() {
        return text.len();
    }
    let mut previous: Vec<usize> = (0..=target.len()).collect();
    let mut current = vec![0; target.len() + 1];
    for (i, &t) in text.iter().enumerate() {
        current[0] = i + 1;
        for (j, &g) in target.iter().enumerate() {
            let deletion = previous[j + 1] + 1;
            let insertion = current[j] + 1;
            let substitution = previous[j] + usize::from(t != g);
            current[j + 1] = deletion.min(substitution.min(insertion));
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[target.len()]
}

/// DuckDB's `DamerauLevenshteinDistance` (unrestricted, with adjacent transpositions).
fn damerau_levenshtein(source: &[u8], target: &[u8]) -> usize {
    let (source_len, target_len) = (source.len(), target.len());
    if source_len == 0 {
        return target_len;
    }
    if target_len == 0 {
        return source_len;
    }
    let infinity = source_len + target_len + 1;
    let width = target_len + 2;
    let mut distance = vec![infinity; (source_len + 2) * width];
    let at = |row: usize, column: usize| row * width + column;
    for i in 0..=source_len {
        distance[at(i + 1, 1)] = i;
    }
    for j in 1..=target_len {
        distance[at(1, j + 1)] = j;
    }
    let mut last_source_match = [0usize; 256];
    for i in 0..source_len {
        let mut last_target_match = 0;
        for j in 0..target_len {
            let source_match = last_source_match[usize::from(target[j])];
            let target_match = last_target_match;
            let cost = if source[i] == target[j] {
                last_target_match = j + 1;
                0
            } else {
                1
            };
            let transposition =
                distance[at(source_match, target_match)] + (i - source_match) + 1 + (j - target_match);
            distance[at(i + 2, j + 2)] = (distance[at(i + 1, j + 1)] + cost).min(
                (distance[at(i + 2, j + 1)] + 1).min((distance[at(i + 1, j + 2)] + 1).min(transposition)),
            );
        }
        last_source_match[usize::from(source[i])] = i + 1;
    }
    distance[at(source_len + 1, target_len + 1)]
}

fn hamming(left: &[u8], right: &[u8]) -> Result<i64> {
    if left.len() != right.len() {
        return Err(Error::invalid_input(
            "Mismatch Function: Strings must be of equal length!",
        ));
    }
    if left.is_empty() {
        return Err(Error::invalid_input(
            "Mismatch Function: Strings must be of length > 0!",
        ));
    }
    Ok(left.iter().zip(right).filter(|(a, b)| a != b).count() as i64)
}

fn jaccard(left: &[u8], right: &[u8]) -> Result<f64> {
    if left.is_empty() || right.is_empty() {
        return Err(Error::invalid_input("Jaccard Function: An argument too short!"));
    }
    let set = |input: &[u8]| {
        let mut bits = [false; 256];
        for &byte in input {
            bits[usize::from(byte)] = true;
        }
        bits
    };
    let (a, b) = (set(left), set(right));
    let intersection = a.iter().zip(&b).filter(|(x, y)| **x && **y).count();
    let union = a.iter().zip(&b).filter(|(x, y)| **x || **y).count();
    Ok(intersection as f64 / union as f64)
}

/// rapidfuzz's `jaro_similarity` with `P` = `pattern` and `T` = `text`.
fn jaro(pattern: &[u8], text: &[u8], score_cutoff: f64) -> f64 {
    let (p_len, t_len) = (pattern.len(), text.len());
    if p_len == 0 || t_len == 0 {
        return 0.0;
    }
    let min_len = p_len.min(t_len) as f64;
    let upper_bound = (min_len / p_len as f64 + min_len / t_len as f64 + 1.0) / 3.0;
    if upper_bound < score_cutoff {
        return 0.0;
    }
    if p_len == 1 && t_len == 1 {
        return f64::from(u8::from(pattern[0] == text[0]));
    }
    let bound = p_len.max(t_len) / 2 - 1;
    let mut p_flag = vec![false; p_len];
    let mut t_flag = vec![false; t_len];
    let mut common = 0usize;
    for (j, &t) in text.iter().enumerate() {
        let low = j.saturating_sub(bound);
        let high = (j + bound).min(p_len - 1);
        if low > high {
            continue;
        }
        for i in low..=high {
            if !p_flag[i] && pattern[i] == t {
                p_flag[i] = true;
                t_flag[j] = true;
                common += 1;
                break;
            }
        }
    }
    if common == 0 {
        return 0.0;
    }
    let common_f = common as f64;
    let filter = (common_f / p_len as f64 + common_f / t_len as f64 + 1.0) / 3.0;
    if filter < score_cutoff {
        return 0.0;
    }
    let mut transpositions = 0usize;
    let mut k = 0;
    for (j, &t) in text.iter().enumerate() {
        if t_flag[j] {
            while !p_flag[k] {
                k += 1;
            }
            if pattern[k] != t {
                transpositions += 1;
            }
            k += 1;
        }
    }
    let transpositions = (transpositions / 2) as f64;
    let mut similarity = 0.0;
    similarity += common_f / p_len as f64;
    similarity += common_f / t_len as f64;
    similarity += (common_f - transpositions) / common_f;
    let similarity = similarity / 3.0;
    if similarity >= score_cutoff {
        similarity
    } else {
        0.0
    }
}

/// rapidfuzz's `jaro_winkler_similarity` with prefix weight 0.1.
fn jaro_winkler(pattern: &[u8], text: &[u8], score_cutoff: f64) -> f64 {
    const PREFIX_WEIGHT: f64 = 0.1;
    let max_prefix = pattern.len().min(text.len()).min(4);
    let prefix = pattern
        .iter()
        .zip(text)
        .take(max_prefix)
        .take_while(|(a, b)| a == b)
        .count() as f64;
    let mut jaro_cutoff = score_cutoff;
    if jaro_cutoff > 0.7 {
        let prefix_similarity = prefix * PREFIX_WEIGHT;
        jaro_cutoff = if prefix_similarity >= 1.0 {
            0.7
        } else {
            0.7f64.max((prefix_similarity - jaro_cutoff) / (prefix_similarity - 1.0))
        };
    }
    let mut similarity = jaro(pattern, text, jaro_cutoff);
    if similarity > 0.7 {
        // Clang contracts `Sim += prefix * weight * (1.0 - Sim)` into an FMA.
        similarity = (prefix * PREFIX_WEIGHT).mul_add(1.0 - similarity, similarity);
    }
    if similarity >= score_cutoff {
        similarity
    } else {
        0.0
    }
}

type Similarity = fn(&[u8], &[u8], f64) -> f64;

/// DuckDB's `TemplatedJaroWinklerFunction`: when exactly one string argument is constant
/// it is the pattern (`CachedJaroSimilarity`), otherwise the first argument is.
fn jaro_kernel(similarity: Similarity) -> impl Fn(&DataChunk, &mut FunctionState, &mut Vector) -> Result<()> {
    move |args, _, result| {
        let first_constant = args.column(0).is_constant();
        let second_constant = args.column(1).is_constant();
        let swap = second_constant && !first_constant;
        let compute = move |left: &[u8], right: &[u8], cutoff: f64| {
            if swap {
                similarity(right, left, cutoff)
            } else {
                similarity(left, right, cutoff)
            }
        };
        if args.column_count() == 2 {
            binary::<Str, Str, f64>(args, result, |left, right, out| {
                *out = compute(left, right, 0.0);
                Ok(true)
            })
        } else {
            ternary::<Str, Str, f64, f64>(args, result, |left, right, cutoff, out| {
                *out = compute(left, right, cutoff);
                Ok(true)
            })
        }
    }
}

fn jaro_set(name: &str, similarity: Similarity) -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new(name);
    for arguments in [
        vec![LogicalType::VARCHAR, LogicalType::VARCHAR],
        vec![LogicalType::VARCHAR, LogicalType::VARCHAR, LogicalType::DOUBLE],
    ] {
        set.add(ScalarFunction::new(
            name,
            arguments,
            LogicalType::DOUBLE,
            jaro_kernel(similarity),
        ));
    }
    set
}

fn distance_set(name: &str, op: fn(&[u8], &[u8]) -> Result<i64>) -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new(name);
    set.add(ScalarFunction::new(
        name,
        vec![LogicalType::VARCHAR, LogicalType::VARCHAR],
        LogicalType::BIGINT,
        move |args, _, result| {
            binary::<Str, Str, i64>(args, result, |left, right, out| {
                *out = op(left, right)?;
                Ok(true)
            })
        },
    ));
    set
}

/// Every similarity function set with its aliases.
pub fn similarity_functions() -> Vec<(ScalarFunctionSet, &'static [&'static str])> {
    let mut jaccard_set = ScalarFunctionSet::new("jaccard");
    jaccard_set.add(ScalarFunction::new(
        "jaccard",
        vec![LogicalType::VARCHAR, LogicalType::VARCHAR],
        LogicalType::DOUBLE,
        |args, _, result| {
            binary::<Str, Str, f64>(args, result, |left, right, out| {
                *out = jaccard(left, right)?;
                Ok(true)
            })
        },
    ));
    vec![
        (
            distance_set("levenshtein", |a, b| Ok(levenshtein(a, b) as i64)),
            &["editdist3"],
        ),
        (
            distance_set("damerau_levenshtein", |a, b| Ok(damerau_levenshtein(a, b) as i64)),
            &[],
        ),
        (distance_set("hamming", hamming), &["mismatches"]),
        (jaccard_set, &[]),
        (jaro_set("jaro_similarity", jaro), &[]),
        (jaro_set("jaro_winkler_similarity", jaro_winkler), &[]),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distances() {
        assert_eq!(levenshtein(b"kitten", b"sitting"), 3);
        assert_eq!(damerau_levenshtein(b"ca", b"abc"), 2);
        assert_eq!(damerau_levenshtein(b"abcd", b"acbd"), 1);
        assert!((jaro(b"MARTHA", b"MARHTA", 0.0) - 0.944_444_444_444_444_4).abs() < 1e-12);
        assert!((jaro_winkler(b"MARTHA", b"MARHTA", 0.0) - 0.961_111_111_111_111_1).abs() < 1e-12);
    }
}
