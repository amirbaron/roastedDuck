//! Translation of RE2 patterns (used by DuckDB's runner) into the `regex` crate's dialect.
//!
//! The dialects agree on almost everything the corpus uses. The differences handled here:
//! - RE2 treats a `{` that does not start a valid repetition (`{n}`, `{n,}`, `{n,m}`) as a
//!   literal; `regex` rejects it.
//! - RE2's `\d`, `\s` and `\w` are ASCII-only; `regex` makes them Unicode-aware.
//! - RE2 reads `\<` and `\>` as escaped literals; `regex` reads them as word boundaries.

/// Rewrites an RE2 pattern so that `regex` accepts it with the same meaning.
pub fn translate(pattern: &str) -> String {
    let chars: Vec<char> = pattern.chars().collect();
    let mut out = String::with_capacity(pattern.len() + 8);
    let mut index = 0;
    while index < chars.len() {
        match chars[index] {
            '\\' => {
                index += 1;
                match chars.get(index) {
                    Some(&class) if ascii_class(class, false).is_some() => {
                        out.push_str(ascii_class(class, false).unwrap_or_default());
                    }
                    // `\<` and `\>` are literals in RE2 but word boundaries in `regex`.
                    Some(&bracket @ ('<' | '>')) => out.push(bracket),
                    Some(&other) => {
                        out.push('\\');
                        out.push(other);
                    }
                    None => out.push('\\'),
                }
                index += 1;
            }
            '[' => index = copy_class(&chars, index, &mut out),
            '{' if !is_repetition(&chars[index..]) => {
                out.push_str("\\{");
                index += 1;
            }
            '}' if !closes_repetition(&out) => {
                out.push_str("\\}");
                index += 1;
            }
            other => {
                out.push(other);
                index += 1;
            }
        }
    }
    out
}

/// The ASCII definition of a Perl class escape, outside (`[0-9]`) or inside (`0-9`) a class.
fn ascii_class(escape: char, inside_class: bool) -> Option<&'static str> {
    Some(match (escape, inside_class) {
        ('d', false) => "[0-9]",
        ('D', false) => "[^0-9]",
        ('s', false) => "[\\t\\n\\f\\r ]",
        ('S', false) => "[^\\t\\n\\f\\r ]",
        ('w', false) => "[0-9A-Za-z_]",
        ('W', false) => "[^0-9A-Za-z_]",
        ('d', true) => "0-9",
        ('s', true) => "\\t\\n\\f\\r ",
        ('w', true) => "0-9A-Za-z_",
        _ => return None,
    })
}

/// Copies a bracket expression starting at `start` (`[`) and returns the index after it.
fn copy_class(chars: &[char], start: usize, out: &mut String) -> usize {
    let mut index = start + 1;
    out.push('[');
    if chars.get(index) == Some(&'^') {
        out.push('^');
        index += 1;
    }
    // A `]` right after `[` or `[^` is a literal member.
    if chars.get(index) == Some(&']') {
        out.push_str("\\]");
        index += 1;
    }
    while let Some(&c) = chars.get(index) {
        match c {
            ']' => {
                out.push(']');
                return index + 1;
            }
            '\\' => {
                match chars.get(index + 1) {
                    Some(&class) if ascii_class(class, true).is_some() => {
                        out.push_str(ascii_class(class, true).unwrap_or_default());
                    }
                    Some(&other) => {
                        out.push('\\');
                        out.push(other);
                    }
                    None => out.push('\\'),
                }
                index += 2;
            }
            '[' if chars.get(index + 1) == Some(&':') => {
                // POSIX class such as [:alpha:]: copy through its closing ":]".
                let end = (index + 2..chars.len().saturating_sub(1))
                    .find(|&i| chars[i] == ':' && chars[i + 1] == ']')
                    .map_or(chars.len(), |i| i + 2);
                out.extend(&chars[index..end]);
                index = end;
            }
            '[' => {
                out.push_str("\\[");
                index += 1;
            }
            other => {
                out.push(other);
                index += 1;
            }
        }
    }
    index
}

/// Whether `chars` (starting at `{`) is a repetition `{n}`, `{n,}` or `{n,m}`.
fn is_repetition(chars: &[char]) -> bool {
    let Some(end) = chars.iter().position(|&c| c == '}') else {
        return false;
    };
    let body: String = chars[1..end].iter().collect();
    let (min, max) = body.split_once(',').unwrap_or((&body, "0"));
    !min.is_empty() && min.chars().all(|c| c.is_ascii_digit()) && max.chars().all(|c| c.is_ascii_digit())
}

/// Whether the `}` about to be written closes a repetition opened in `out`.
fn closes_repetition(out: &str) -> bool {
    let Some(open) = out.rfind('{') else {
        return false;
    };
    if out[..open].ends_with('\\') {
        return false;
    }
    let body = &out[open + 1..];
    let (min, max) = body.split_once(',').unwrap_or((body, "0"));
    !min.is_empty() && min.chars().all(|c| c.is_ascii_digit()) && max.chars().all(|c| c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn braces_that_are_not_repetitions_become_literals() {
        assert_eq!(translate("a{2}b{1,}c{1,3}"), "a{2}b{1,}c{1,3}");
        assert_eq!(translate("{'col'={x=1}}"), "\\{'col'=\\{x=1\\}\\}");
        assert_eq!(translate("\\{a\\}"), "\\{a\\}");
    }

    #[test]
    fn perl_classes_are_ascii() {
        assert_eq!(translate("\\d+"), "[0-9]+");
        assert_eq!(translate("[\\d.]"), "[0-9.]");
        assert_eq!(translate("\\."), "\\.");
        assert_eq!(translate("\\(a \\<"), "\\(a <");
    }

    #[test]
    fn bracket_expressions_are_kept() {
        assert_eq!(translate("[]a]"), "[\\]a]");
        assert_eq!(translate("[^{}]"), "[^{}]");
        assert_eq!(translate("[[:alpha:]]+"), "[[:alpha:]]+");
    }

    #[test]
    fn translated_patterns_compile() {
        for pattern in ["{'\"col\"'={size=\\d+}}", ".*\\[point, line\\].*", "x{", "}{"] {
            assert!(regex::Regex::new(&translate(pattern)).is_ok(), "{pattern}");
        }
    }

    fn collect_patterns(dir: &std::path::Path, patterns: &mut Vec<(String, String)>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                collect_patterns(&path, patterns);
            } else if path.extension().is_some_and(|e| e == "test" || e == "test_slow") {
                let text = std::fs::read_to_string(&path).unwrap();
                for field in text.lines().flat_map(|line| line.split('\t')) {
                    if let Some(body) = field
                        .strip_prefix("<REGEX>:")
                        .or_else(|| field.strip_prefix("<!REGEX>:"))
                    {
                        patterns.push((path.display().to_string(), body.to_owned()));
                    }
                }
            }
        }
    }

    /// Every `<REGEX>:` pattern of the DuckDB corpus translates to a valid `regex`.
    #[test]
    #[ignore = "needs the DuckDB source tree: DUCKDB_REF=<dir> cargo test -p slt -- --ignored"]
    fn every_corpus_pattern_compiles() {
        let root = std::env::var("DUCKDB_REF").expect("DUCKDB_REF");
        let mut patterns = Vec::new();
        collect_patterns(&std::path::Path::new(&root).join("test"), &mut patterns);
        assert_ne!(patterns.len(), 0);
        let failures: Vec<_> = patterns
            .iter()
            .filter(|(_, body)| regex::Regex::new(&translate(body)).is_err())
            .collect();
        assert!(
            failures.is_empty(),
            "{} of {} fail: {failures:#?}",
            failures.len(),
            patterns.len()
        );
    }
}
