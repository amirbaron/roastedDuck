//! Differential test and benchmark against DuckDB's C++ parser.
//!
//! ```text
//! python3 crates/pg-parser/scripts/extract_corpus.py   # -> target/pg-corpus/corpus.txt
//! python3 crates/pg-parser/scripts/mutate_corpus.py    # -> target/pg-corpus/mutations.txt
//! python3 crates/pg-parser/scripts/gen_cpp_oracle.py   # -> target/pg-oracle/pg_oracle
//! target/pg-oracle/pg_oracle < target/pg-corpus/corpus.txt > target/pg-corpus/corpus.oracle.jsonl
//! cargo run --release -p rustdb-pg-parser --example corpus_check -- \
//!     target/pg-corpus/corpus.txt target/pg-corpus/corpus.oracle.jsonl
//! cargo run --release -p rustdb-pg-parser --example corpus_check -- target/pg-corpus/corpus.txt --bench 10
//! target/pg-oracle/pg_oracle bench 10 < target/pg-corpus/corpus.txt
//! ```
//!
//! For every statement the parse tree (or error message and position) is rendered
//! in the oracle's JSON format and compared byte for byte. Statements first go
//! through `strip_unicode_spaces`, as in DuckDB's `Parser::ParseQuery`.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::Write as _;
use std::time::Instant;

use rustdb_common::ErrorKind;
use rustdb_pg_parser::{nodes, parse, strip_unicode_spaces, tokenize};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

/// Reads records of the form `<byte length>\n<bytes>`.
fn read_corpus(path: &str) -> Result<Vec<String>> {
    let data = std::fs::read(path).map_err(|e| format!("cannot read {path}: {e}"))?;
    let mut records = Vec::new();
    let mut rest = &data[..];
    while !rest.is_empty() {
        let newline = rest
            .iter()
            .position(|&b| b == b'\n')
            .ok_or("truncated record header")?;
        let len: usize = std::str::from_utf8(&rest[..newline])?.parse()?;
        let body = rest
            .get(newline + 1..newline + 1 + len)
            .ok_or("truncated record")?;
        records.push(String::from_utf8_lossy(body).into_owned());
        rest = &rest[newline + 1 + len..];
    }
    Ok(records)
}

fn json_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if u32::from(c) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// The Rust result in the oracle's format.
fn render(sql: &str) -> String {
    let stripped = strip_unicode_spaces(sql);
    match parse(stripped.as_deref().unwrap_or(sql)) {
        Ok(stmts) => format!("{{\"ok\":true,\"tree\":{}}}", nodes::to_json(&stmts)),
        Err(err) => {
            let mut out = String::from("{\"ok\":false,\"msg\":");
            if err.kind() == ErrorKind::Parser {
                json_string(&mut out, err.raw_message());
            } else {
                json_string(&mut out, &err.to_string());
            }
            match err.query_location() {
                Some(pos) => {
                    let _ = write!(out, ",\"pos\":{pos}}}");
                }
                None => out.push_str(",\"pos\":null}"),
            }
            out
        }
    }
}

fn compare(corpus: &[String], oracle_path: &str) -> Result<()> {
    let oracle = std::fs::read_to_string(oracle_path)?;
    let oracle: Vec<&str> = oracle.lines().collect();
    if oracle.len() != corpus.len() {
        return Err(format!("{} statements but {} oracle results", corpus.len(), oracle.len()).into());
    }
    let mut stats = BTreeMap::<&str, usize>::new();
    let mut report = String::new();
    for (i, (sql, &expected)) in corpus.iter().zip(&oracle).enumerate() {
        let actual = render(sql);
        let expected_ok = expected.starts_with("{\"ok\":true");
        let actual_ok = actual.starts_with("{\"ok\":true");
        let category = match (expected_ok, actual_ok, actual == expected) {
            (true, true, true) => "accepted, identical tree",
            (false, false, true) => "rejected, identical error and position",
            (true, true, false) => "accepted, TREE DIFFERS",
            (false, false, false) => "rejected, ERROR DIFFERS",
            (true, false, _) => "ACCEPT/REJECT MISMATCH (C++ accepts)",
            (false, true, _) => "ACCEPT/REJECT MISMATCH (C++ rejects)",
        };
        *stats.entry(category).or_default() += 1;
        if actual != expected {
            let _ = writeln!(
                report,
                "#{i} {category}\nSQL: {sql}\nC++:  {expected}\nRust: {actual}\n"
            );
        }
    }
    println!("{} statements", corpus.len());
    for (category, count) in &stats {
        println!("  {count:6}  {category}");
    }
    let report_path = format!("{oracle_path}.mismatches.txt");
    std::fs::write(&report_path, report)?;
    println!("mismatch details: {report_path}");
    Ok(())
}

#[allow(clippy::cast_precision_loss)]
fn bench(corpus: &[String], iterations: usize, tokens_only: bool) {
    let queries: Vec<String> = corpus
        .iter()
        .map(|sql| strip_unicode_spaces(sql).unwrap_or_else(|| sql.clone()))
        .collect();
    let bytes: usize = queries.iter().map(String::len).sum();
    let start = Instant::now();
    let mut ok = 0;
    for _ in 0..iterations {
        for sql in &queries {
            ok += if tokens_only {
                usize::from(!tokenize(sql).is_empty())
            } else {
                usize::from(parse(sql).is_ok())
            };
        }
    }
    let elapsed = start.elapsed().as_secs_f64();
    let mb_per_s = (bytes * iterations) as f64 / elapsed / 1e6;
    let stmts_per_s = (queries.len() * iterations) as f64 / elapsed;
    println!(
        "Rust parser: {} statements x {iterations} iterations in {elapsed:.3} s \
         ({mb_per_s:.1} MB/s, {stmts_per_s:.0} stmts/s), {} ok",
        queries.len(),
        ok / iterations.max(1)
    );
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let corpus = read_corpus(
        args.first()
            .ok_or("usage: corpus_check <corpus> (<oracle.jsonl> | --bench N)")?,
    )?;
    match args.get(1).map(String::as_str) {
        Some(mode @ ("--bench" | "--bench-scanner")) => bench(
            &corpus,
            args.get(2).and_then(|n| n.parse().ok()).unwrap_or(10),
            mode == "--bench-scanner",
        ),
        Some(oracle) => compare(&corpus, oracle)?,
        None => return Err("missing oracle output or --bench".into()),
    }
    Ok(())
}
