//! Prints the statements of a corpus that take the longest to parse (dev tool).

use std::time::Instant;

fn main() {
    let path = std::env::args().nth(1).expect("corpus path");
    let data = std::fs::read(&path).expect("corpus");
    let mut records = Vec::new();
    let mut pos = 0;
    while pos < data.len() {
        let nl = pos + data[pos..].iter().position(|&b| b == b'\n').expect("header");
        let len: usize = std::str::from_utf8(&data[pos..nl])
            .expect("len")
            .parse()
            .expect("len");
        records.push(String::from_utf8_lossy(&data[nl + 1..nl + 1 + len]).into_owned());
        pos = nl + 1 + len;
    }
    let mut timings: Vec<(f64, usize)> = records
        .iter()
        .enumerate()
        .map(|(i, sql)| {
            let start = Instant::now();
            for _ in 0..5 {
                let _ = rustdb_pg_parser::parse(sql);
            }
            (start.elapsed().as_secs_f64() / 5.0, i)
        })
        .collect();
    let total: f64 = timings.iter().map(|t| t.0).sum();
    timings.sort_by(|a, b| b.0.total_cmp(&a.0));
    println!("total {total:.3} s");
    for (t, i) in timings.iter().take(15) {
        let sql = &records[*i];
        let preview: String = sql.chars().take(100).collect();
        println!(
            "{:8.3} ms  {:7} bytes  {}",
            t * 1e3,
            sql.len(),
            preview.replace('\n', " ")
        );
    }
}
