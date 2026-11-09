//! Differential test and benchmark against DuckDB's C++ parser.
//!
//! ```text
//! python3 crates/pg-parser/scripts/extract_corpus.py   # -> target/pg-corpus/corpus.txt
//! python3 crates/pg-parser/scripts/mutate_corpus.py    # -> target/pg-corpus/mutations.txt
//! python3 crates/pg-parser/scripts/gen_cpp_oracle.py   # -> target/pg-oracle/pg_oracle
//! target/pg-oracle/pg_oracle < target/pg-corpus/corpus.txt > target/pg-corpus/corpus.oracle.jsonl
//! cargo run --release -p roastedduck-pg-parser --example corpus_check -- \
//!     target/pg-corpus/corpus.txt target/pg-corpus/corpus.oracle.jsonl
//! cargo run --release -p roastedduck-pg-parser --example corpus_check -- target/pg-corpus/corpus.txt --bench 10
//! target/pg-oracle/pg_oracle bench 10 < target/pg-corpus/corpus.txt
//! ```
//!
//! For every statement the parse tree (or error message and position) is rendered
//! in the oracle's JSON format and compared byte for byte. Statements first go
//! through `strip_unicode_spaces`, as in DuckDB's `Parser::ParseQuery`.

use std::collections::BTreeMap;
use std::error::Error;
use std::time::Instant;

use roastedduck_common::{ByteString, byte_write};
use roastedduck_pg_parser::{parse_bytes, strip_unicode_spaces_bytes, tokenize_bytes};
#[path = "support/pg_native.rs"]
mod pg_native;
use pg_native::render;

type Result<T> = std::result::Result<T, Box<dyn Error>>;

/// Reads records of the form `<byte length>\n<bytes>`.
fn read_corpus(path: &str) -> Result<Vec<ByteString>> {
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
        records.push(body.into());
        rest = &rest[newline + 1 + len..];
    }
    Ok(records)
}

fn compare(corpus: &[ByteString], oracle_path: &str, preserve_identifier_case: bool) -> Result<()> {
    let oracle = std::fs::read(oracle_path)?;
    let oracle: Vec<&[u8]> = oracle
        .strip_suffix(b"\n")
        .unwrap_or(&oracle)
        .split(|byte| *byte == b'\n')
        .collect();
    if oracle.len() != corpus.len() {
        return Err(format!("{} statements but {} oracle results", corpus.len(), oracle.len()).into());
    }
    let mut stats = BTreeMap::<&str, usize>::new();
    let mut report = ByteString::new();
    for (i, (sql, &expected)) in corpus.iter().zip(&oracle).enumerate() {
        let actual = render(sql, preserve_identifier_case);
        let expected_ok = expected.starts_with(b"{\"ok\":true");
        let actual_ok = actual.starts_with(b"{\"ok\":true");
        let category = match (expected_ok, actual_ok, actual.as_bytes() == expected) {
            (true, true, true) => "accepted, identical tree",
            (false, false, true) => "rejected, identical error and position",
            (true, true, false) => "accepted, TREE DIFFERS",
            (false, false, false) => "rejected, ERROR DIFFERS",
            (true, false, _) => "ACCEPT/REJECT MISMATCH (C++ accepts)",
            (false, true, _) => "ACCEPT/REJECT MISMATCH (C++ rejects)",
        };
        *stats.entry(category).or_default() += 1;
        if actual.as_bytes() != expected {
            let _ = byte_write!(
                report,
                "#{i} {category}\nSQL: {sql}\nC++:  {expected}\nRust: {actual}\n\n"
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
    if stats
        .keys()
        .any(|key| key.contains("DIFFERS") || key.contains("MISMATCH"))
    {
        return Err("parser differential mismatch".into());
    }
    Ok(())
}

#[allow(clippy::cast_precision_loss)]
fn bench(corpus: &[ByteString], iterations: usize, tokens_only: bool) {
    let queries: Vec<ByteString> = corpus
        .iter()
        .map(|sql| strip_unicode_spaces_bytes(sql).unwrap_or_else(|| sql.clone()))
        .collect();
    let bytes: usize = queries.iter().map(|query| query.len()).sum();
    let start = Instant::now();
    let mut ok = 0;
    for _ in 0..iterations {
        for sql in &queries {
            ok += if tokens_only {
                usize::from(!tokenize_bytes(sql).is_empty())
            } else {
                usize::from(parse_bytes(sql).is_ok())
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
        Some(oracle) => compare(&corpus, oracle, args.get(2).is_none_or(|arg| arg != "--fold"))?,
        None => return Err("missing oracle output or --bench".into()),
    }
    Ok(())
}
