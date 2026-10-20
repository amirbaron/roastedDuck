//! Compares the PEG parser (`rustdb_autocomplete::parse_query`, DuckDB's parser
//! override) with the Postgres parser (`rustdb_parser`) on the statement corpus of
//! `tools/corpus` (`statements.jsonl`). DuckDB runs its whole test suite with the PEG
//! parser in strict mode, so the two should agree on nearly every statement.
//!
//! ```text
//! cargo run --release -p rustdb-autocomplete --example peg_check -- \
//!     target/corpus/statements.jsonl [report-file]
//! ```
//!
//! Prints the number of statements per outcome and writes a sample of each outcome to
//! the report file (default `target/peg-check/report.txt`).

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::{BufRead, BufReader};
use std::panic::{AssertUnwindSafe, catch_unwind};

use rustdb_ast::SqlStatement;
use rustdb_common::Result;
use rustdb_parser::ParserOptions;
use rustdb_serde::json::{JsonValue, parse_json};

/// The PEG parse as `Parser::ParseQuery` runs it: after replacing Unicode spaces.
fn peg_parse(sql: &str) -> std::result::Result<Result<Vec<SqlStatement>>, String> {
    let stripped = rustdb_parser::strip_unicode_spaces(sql);
    let query = stripped.as_deref().unwrap_or(sql).to_owned();
    catch_unwind(AssertUnwindSafe(|| {
        rustdb_autocomplete::parse_query(&query, ParserOptions::default())
    }))
    .map_err(|panic| {
        panic
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| panic.downcast_ref::<&str>().map(|text| (*text).to_owned()))
            .unwrap_or_default()
    })
}

/// A short, stable category for an error message.
fn error_category(message: &str) -> String {
    let first_line = message.lines().next().unwrap_or_default();
    let trimmed: String = first_line
        .chars()
        .map(|c| if c.is_ascii_digit() { '#' } else { c })
        .take(90)
        .collect();
    trimmed
}

/// The innermost struct and field around the first difference of two renderings.
fn difference_field(expected: &str, actual: &str) -> String {
    let position = expected
        .bytes()
        .zip(actual.bytes())
        .position(|(a, b)| a != b)
        .unwrap_or_else(|| expected.len().min(actual.len()));
    let before = &expected[..position.min(expected.len())];
    let field = before
        .split(|c: char| !(c.is_alphanumeric() || c == '_' || c == ':'))
        .filter(|word| word.len() > 1 && word.ends_with(':'))
        .last()
        .map_or("", |word| word.trim_end_matches(':'));
    let owner = before
        .rsplit(" { ")
        .nth(1)
        .and_then(|text| text.rsplit(|c: char| !(c.is_alphanumeric() || c == '_')).next())
        .unwrap_or("");
    format!("{owner}.{field}")
}

/// The first place the debug renderings of two parses differ, with some context.
fn first_difference(expected: &str, actual: &str) -> String {
    let position = expected
        .bytes()
        .zip(actual.bytes())
        .position(|(a, b)| a != b)
        .unwrap_or_else(|| expected.len().min(actual.len()));
    let start = position.saturating_sub(120);
    let window = |text: &str| -> String {
        let end = (position + 160).min(text.len());
        String::from_utf8_lossy(&text.as_bytes()[start.min(text.len())..end]).into_owned()
    };
    format!("  pg : …{}\n  peg: …{}", window(expected), window(actual))
}

/// The debug rendering of the statements' contents, without the query locations (the
/// PEG transformer's differ from the Postgres parser's) and the statement text (the PEG
/// parser's starts at the first token).
fn rendering(statements: &[SqlStatement]) -> String {
    let mut text = String::new();
    for statement in statements {
        let _ = write!(text, "{:?} {:?}\n", statement.named_param_map, statement.kind);
    }
    let marker = "query_location: ";
    let mut result = String::with_capacity(text.len());
    let mut rest = text.as_str();
    while let Some(position) = rest.find(marker) {
        result.push_str(&rest[..position + marker.len()]);
        rest = &rest[position + marker.len()..];
        if let Some(after) = rest.strip_prefix("None") {
            rest = after;
        } else if let Some(after) = rest.strip_prefix("Some(") {
            rest = after.trim_start_matches(|c: char| c.is_ascii_digit());
            rest = rest.strip_prefix(')').unwrap_or(rest);
        }
        result.push('_');
    }
    result.push_str(rest);
    result
}

/// Which parts of two parses with the same debug rendering differ.
fn equal_rendering_difference(expected: &[SqlStatement], actual: &[SqlStatement]) -> String {
    let mut parts = Vec::new();
    for (expected, actual) in expected.iter().zip(actual) {
        if expected.location != actual.location || expected.length != actual.length {
            parts.push("location/length");
        }
        if expected.query != actual.query {
            parts.push("query");
        }
        if expected.named_param_map != actual.named_param_map {
            parts.push("named_param_map");
        }
        if expected.kind != actual.kind {
            parts.push("kind");
        }
    }
    format!("  parts: {}", parts.join(", "))
}

fn main() -> std::io::Result<()> {
    // The SQL logic test runner runs statements on threads with large stacks too.
    std::thread::Builder::new()
        .stack_size(
            std::env::var("PEG_CHECK_STACK_MB")
                .ok()
                .and_then(|mb| mb.parse::<usize>().ok())
                .unwrap_or(1024)
                << 20,
        )
        .spawn(run)?
        .join()
        .unwrap_or_else(|_| Err(std::io::Error::other("peg_check panicked")))
}

fn run() -> std::io::Result<()> {
    let trace = std::env::var_os("PEG_CHECK_TRACE").is_some();
    // With PEG_CHECK_LOCATIONS, parses differing in locations only count as different.
    let locations = std::env::var_os("PEG_CHECK_LOCATIONS").is_some();
    let mut args = std::env::args().skip(1);
    let statements_path = args
        .next()
        .unwrap_or_else(|| "target/corpus/statements.jsonl".to_owned());
    let report_path = args
        .next()
        .unwrap_or_else(|| "target/peg-check/report.txt".to_owned());
    let reader = BufReader::new(std::fs::File::open(&statements_path)?);
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut samples: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut total = 0usize;
    for line in reader.lines() {
        let line = line?;
        let Ok(row) = parse_json(&line) else {
            continue;
        };
        let Some(JsonValue::Str(sql)) = row.get("sql") else {
            continue;
        };
        total += 1;
        if trace {
            eprintln!("{total}: {sql}");
        }
        let pg = rustdb_parser::parse(sql);
        let peg = peg_parse(sql);
        let (category, detail) = match (pg, peg) {
            (_, Err(panic)) => (format!("PANIC {}", error_category(&panic)), String::new()),
            (Ok(expected), Ok(Ok(actual))) if expected == actual => ("same".to_owned(), String::new()),
            (Ok(expected), Ok(Ok(actual))) => {
                let (expected_text, actual_text) = (rendering(&expected), rendering(&actual));
                let (expected_full, actual_full) = (format!("{expected:?}"), format!("{actual:?}"));
                if expected_text != actual_text {
                    (
                        format!("different: {}", difference_field(&expected_text, &actual_text)),
                        first_difference(&expected_text, &actual_text),
                    )
                } else if expected_full == actual_full {
                    (
                        "different (same rendering)".to_owned(),
                        equal_rendering_difference(&expected, &actual),
                    )
                } else if locations {
                    (
                        "different locations".to_owned(),
                        first_difference(&expected_full, &actual_full),
                    )
                } else {
                    ("same (but locations)".to_owned(), String::new())
                }
            }
            (Ok(_), Ok(Err(error))) => (
                format!("peg error: {}", error_category(&error.to_string())),
                String::new(),
            ),
            (Err(_), Ok(Ok(_))) => ("peg accepts".to_owned(), String::new()),
            (Err(expected), Ok(Err(actual))) => {
                let (expected, actual) = (expected.to_string(), actual.to_string());
                if expected == actual {
                    ("both fail alike".to_owned(), String::new())
                } else {
                    (
                        "both fail differently".to_owned(),
                        format!(
                            "  pg : {}\n  peg: {}",
                            error_category(&expected),
                            error_category(&actual)
                        ),
                    )
                }
            }
        };
        *counts.entry(category.clone()).or_default() += 1;
        let entries = samples.entry(category).or_default();
        if entries.len() < 30 {
            let mut sample = sql.chars().take(400).collect::<String>();
            if !detail.is_empty() {
                sample.push('\n');
                sample.push_str(&detail);
            }
            entries.push(sample);
        }
    }
    let mut ranked: Vec<(&String, &usize)> = counts.iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
    println!("{total} statements");
    for (category, count) in ranked.iter().take(60) {
        println!("{count:>7}  {category}");
    }
    let mut report = String::new();
    for (category, count) in &ranked {
        let _ = writeln!(report, "=== {category} ({count})");
        for sample in &samples[*category] {
            let _ = writeln!(report, "{sample}\n");
        }
    }
    if let Some(parent) = std::path::Path::new(&report_path).parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&report_path, report)?;
    println!("samples → {report_path}");
    Ok(())
}
