//! Differential test and benchmark against DuckDB's C++ parser.
//!
//! ```text
//! python3 crates/pg-parser/scripts/extract_corpus.py        # -> target/pg-corpus/corpus.txt
//! python3 crates/pg-parser/scripts/gen_cpp_oracle.py        # -> target/pg-oracle/pg_oracle
//! target/pg-oracle/pg_oracle < target/pg-corpus/corpus.txt > target/pg-corpus/oracle.jsonl
//! cargo run --release -p rustdb-pg-parser --example corpus_check -- \
//!     target/pg-corpus/corpus.txt target/pg-corpus/oracle.jsonl
//! cargo run --release -p rustdb-pg-parser --example corpus_check -- \
//!     target/pg-corpus/corpus.txt --bench 10
//! ```
//!
//! For every statement the parse tree (or error message and position) is rendered
//! in the oracle's JSON format and compared byte for byte.

use std::fmt::Write as _;
use std::time::Instant;

use rustdb_pg_parser::{nodes, parse, strip_unicode_spaces};

fn read_corpus(path: &str) -> Vec<String> {
    let data = std::fs::read(path).unwrap_or_else(|e| panic!("cannot read {path}: {e}"));
    let mut records = Vec::new();
    let mut pos = 0;
    while pos < data.len() {
        let nl = data[pos..]
            .iter()
            .position(|&b| b == b'\n')
            .map(|i| pos + i)
            .expect("record header");
        let len: usize = std::str::from_utf8(&data[pos..nl])
            .expect("length")
            .parse()
            .expect("length");
        let bytes = &data[nl + 1..nl + 1 + len];
        records.push(String::from_utf8_lossy(bytes).into_owned());
        pos = nl + 1 + len;
    }
    records
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
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// The Rust result in the oracle's format.
fn render(sql: &str) -> String {
    let stripped = strip_unicode_spaces(sql);
    let sql = stripped.as_deref().unwrap_or(sql);
    match parse(sql) {
        Ok(stmts) => format!("{{\"ok\":true,\"tree\":{}}}", nodes::to_json(&stmts)),
        Err(err) => {
            let mut out = String::from("{\"ok\":false,\"msg\":");
            if err.kind() == rustdb_common::ErrorKind::Parser {
                json_string(&mut out, err.raw_message());
            } else {
                json_string(&mut out, &err.to_string());
            }
            out.push_str(",\"pos\":");
            match err.query_location() {
                Some(pos) => {
                    let _ = write!(out, "{pos}");
                }
                None => out.push_str("null"),
            }
            out.push('}');
            out
        }
    }
}

fn bench(corpus: &[String], iterations: usize) {
    let queries: Vec<String> = corpus
        .iter()
        .map(|sql| strip_unicode_spaces(sql).unwrap_or_else(|| sql.clone()))
        .collect();
    let bytes: usize = queries.iter().map(String::len).sum();
    let start = Instant::now();
    let mut ok = 0;
    for _ in 0..iterations {
        for sql in &queries {
            ok += usize::from(parse(sql).is_ok());
        }
    }
    let elapsed = start.elapsed().as_secs_f64();
    #[allow(clippy::cast_precision_loss)]
    let (mb, stmts) = (
        (bytes * iterations) as f64 / elapsed / 1e6,
        (queries.len() * iterations) as f64 / elapsed,
    );
    println!(
        "Rust parser: {} statements x {iterations} iterations in {elapsed:.3} s ({mb:.1} MB/s, {stmts:.0} stmts/s), {} ok",
        queries.len(),
        ok / iterations
    );
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let corpus = read_corpus(&args[0]);
    if args.get(1).map(String::as_str) == Some("--bench") {
        let iterations = args.get(2).and_then(|n| n.parse().ok()).unwrap_or(10);
        bench(&corpus, iterations);
        return;
    }
    let oracle: Vec<String> = std::fs::read_to_string(&args[1])
        .expect("oracle output")
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(
        corpus.len(),
        oracle.len(),
        "corpus and oracle output differ in length"
    );

    let mut stats = std::collections::BTreeMap::<&str, usize>::new();
    let mut report = String::new();
    for (i, (sql, expected)) in corpus.iter().zip(&oracle).enumerate() {
        let actual = render(sql);
        let expected_ok = expected.starts_with("{\"ok\":true");
        let actual_ok = actual.starts_with("{\"ok\":true");
        let category = match (expected_ok, actual_ok, &actual == expected) {
            (true, true, true) => "accepted, identical tree",
            (false, false, true) => "rejected, identical error and position",
            (true, true, false) => "accepted, TREE DIFFERS",
            (false, false, false) => "rejected, ERROR DIFFERS",
            (true, false, _) => "ACCEPT/REJECT MISMATCH (C++ accepts)",
            (false, true, _) => "ACCEPT/REJECT MISMATCH (C++ rejects)",
        };
        *stats.entry(category).or_default() += 1;
        if &actual != expected {
            let _ = writeln!(
                report,
                "#{i} {category}\nSQL: {sql}\nC++:  {expected}\nRust: {actual}\n"
            );
        }
    }
    let total = corpus.len();
    println!("{total} statements");
    for (category, count) in &stats {
        println!("  {count:6}  {category}");
    }
    let report_path = format!("{}.mismatches.txt", args[1]);
    std::fs::write(&report_path, report).expect("write report");
    println!("mismatch details: {report_path}");
}
