//! Parses every query of a corpus `iterations` times with `roastedduck_parser::parse`
//! (grammar + transformer), for comparison with DuckDB's `Parser::ParseQuery`
//! (`scripts/parser_bench.cpp`).
//!
//! ```text
//! cargo run --release -p roastedduck-parser --example parse_bench -- <corpus> <iterations>
//! ```
//!
//! The corpus is a sequence of records `<byte length>\n<sql>`; records that are not
//! UTF-8 are skipped (by both benchmarks' callers, which use UTF-8 corpora).

use std::time::Instant;

fn read_corpus(path: &str) -> std::io::Result<Vec<String>> {
    let data = std::fs::read(path)?;
    let mut result = Vec::new();
    let mut pos = 0;
    while let Some(newline) = data[pos..].iter().position(|&byte| byte == b'\n') {
        let length: usize = std::str::from_utf8(&data[pos..pos + newline])
            .ok()
            .and_then(|text| text.parse().ok())
            .unwrap_or(0);
        let start = pos + newline + 1;
        let end = (start + length).min(data.len());
        if let Ok(sql) = std::str::from_utf8(&data[start..end]) {
            result.push(sql.to_owned());
        }
        pos = end;
        if pos >= data.len() {
            break;
        }
    }
    Ok(result)
}

fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (Some(path), Some(iterations)) = (args.first(), args.get(1).and_then(|n| n.parse::<usize>().ok()))
    else {
        eprintln!("usage: parse_bench <corpus> <iterations>");
        std::process::exit(1);
    };
    let corpus = read_corpus(path)?;
    let start = Instant::now();
    let mut statements = 0;
    let mut errors = 0;
    for _ in 0..iterations {
        for sql in &corpus {
            match roastedduck_parser::parse(sql) {
                Ok(parsed) => statements += parsed.len(),
                Err(_) => errors += 1,
            }
        }
    }
    println!(
        "{} queries, {statements} statements, {errors} errors in {:.2?}",
        corpus.len() * iterations,
        start.elapsed()
    );
    Ok(())
}
