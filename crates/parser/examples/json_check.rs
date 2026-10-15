//! Compares `rustdb_parser::json::serialize_sql` with DuckDB's `json_serialize_sql` on the
//! statement corpus of `tools/corpus` (`statements.jsonl` + `serialized.jsonl`).
//!
//! ```text
//! cargo run --release -p rustdb-parser --example json_check -- \
//!     target/corpus/statements.jsonl target/corpus/serialized.jsonl [mismatch-file]
//! ```
//!
//! Every output variant the oracle recorded is checked: default options, `skip_null +
//! skip_empty + skip_default`, `skip_null + skip_default`, `format := true`, and the
//! latest storage version. Mismatches are written to the mismatch file (default
//! `target/parser-check/mismatches.txt`).

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::time::{Duration, Instant};

use rustdb_ast::json::JsonSerializeOptions;
use rustdb_parser::json::serialize_sql;
use rustdb_serde::SerializationCompatibility;
use rustdb_serde::json::{JsonOptions, JsonValue, parse_json};

fn field(row: &JsonValue, key: &str) -> Option<String> {
    match row.get(key) {
        Some(JsonValue::Str(text)) => Some(text.clone()),
        Some(JsonValue::UInt(number)) => Some(number.to_string()),
        Some(JsonValue::SInt(number)) => Some(number.to_string()),
        _ => None,
    }
}

fn variants() -> Vec<(&'static str, JsonSerializeOptions)> {
    vec![
        ("json", JsonSerializeOptions::default()),
        (
            "json_skip",
            JsonSerializeOptions {
                json: JsonOptions {
                    skip_null: true,
                    skip_empty: true,
                    skip_default: true,
                },
                ..JsonSerializeOptions::default()
            },
        ),
        (
            "json_skip_defaults",
            JsonSerializeOptions {
                json: JsonOptions {
                    skip_null: true,
                    skip_empty: false,
                    skip_default: true,
                },
                ..JsonSerializeOptions::default()
            },
        ),
        (
            "json_pretty",
            JsonSerializeOptions {
                format: true,
                ..JsonSerializeOptions::default()
            },
        ),
        (
            "json_latest",
            JsonSerializeOptions {
                compatibility: SerializationCompatibility::latest(),
                ..JsonSerializeOptions::default()
            },
        ),
    ]
}

/// A short category for a mismatch, from the expected and actual outputs.
fn category(expected: &str, actual: &str) -> String {
    let error_message = |text: &str| -> String {
        parse_json(text)
            .ok()
            .and_then(|value| field(&value, "error_message"))
            .unwrap_or_default()
    };
    let truncate = |text: String| text.chars().take(70).collect::<String>();
    match (
        expected.starts_with(r#"{"error":true"#),
        actual.starts_with(r#"{"error":true"#),
    ) {
        (true, true) => format!("both errors: expected {}", truncate(error_message(expected))),
        (true, false) => format!("expected error: {}", truncate(error_message(expected))),
        (false, true) => format!("unexpected error: {}", truncate(error_message(actual))),
        (false, false) => "different trees".to_owned(),
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let statements_path = args
        .first()
        .map_or("target/corpus/statements.jsonl", String::as_str);
    let serialized_path = args
        .get(1)
        .map_or("target/corpus/serialized.jsonl", String::as_str);
    let mismatch_path = args
        .get(2)
        .map_or("target/parser-check/mismatches.txt", String::as_str);
    if let Some(parent) = std::path::Path::new(mismatch_path).parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut sql_by_id: HashMap<String, String> = HashMap::new();
    for line in BufReader::new(File::open(statements_path)?).lines() {
        let row = parse_json(&line?)?;
        if let (Some(id), Some(sql)) = (field(&row, "id"), field(&row, "sql")) {
            sql_by_id.insert(id, sql);
        }
    }

    let variants = variants();
    let mut mismatches = File::create(mismatch_path)?;
    let mut total = 0_usize;
    let mut equal = vec![0_usize; variants.len()];
    let mut statements_equal = 0_usize;
    let mut select_ok = 0_usize;
    let mut select_equal = 0_usize;
    let mut categories: BTreeMap<String, usize> = BTreeMap::new();
    let mut elapsed = Duration::ZERO;
    for line in BufReader::new(File::open(serialized_path)?).lines() {
        let row = parse_json(&line?)?;
        let Some(id) = field(&row, "id") else { continue };
        let Some(sql) = sql_by_id.get(&id) else { continue };
        total += 1;
        let mut all_equal = true;
        let expected_default = field(&row, "json").unwrap_or_default();
        let is_select = expected_default.starts_with(r#"{"error":false"#);
        if is_select {
            select_ok += 1;
        }
        for (index, (name, options)) in variants.iter().enumerate() {
            let Some(expected) = field(&row, name) else {
                continue;
            };
            let start = Instant::now();
            let actual = serialize_sql(sql, options);
            elapsed += start.elapsed();
            if actual == expected {
                equal[index] += 1;
            } else {
                all_equal = false;
                if index == 0 {
                    *categories.entry(category(&expected, &actual)).or_default() += 1;
                }
                let mut report = String::new();
                let _ = writeln!(
                    report,
                    "=== id {id} [{name}]\n{sql}\n--- expected\n{expected}\n--- actual\n{actual}"
                );
                mismatches.write_all(report.as_bytes())?;
            }
        }
        if all_equal {
            statements_equal += 1;
            if is_select {
                select_equal += 1;
            }
        }
    }
    println!("statements: {total}, all variants identical: {statements_equal}");
    println!("serializable SELECTs: {select_ok}, identical: {select_equal}");
    for (index, (name, _)) in variants.iter().enumerate() {
        println!("  {name:20} identical: {}", equal[index]);
    }
    println!("time in serialize_sql: {:.2?}", elapsed);
    let mut sorted: Vec<_> = categories.into_iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(&a.1));
    println!("mismatch categories (default variant):");
    for (name, count) in sorted.iter().take(60) {
        println!("  {count:6}  {name}");
    }
    Ok(())
}
