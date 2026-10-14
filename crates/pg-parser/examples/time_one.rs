//! Times repeated parses of one statement (dev tool).

use std::time::Instant;

fn main() {
    let sql = std::env::args().nth(1).expect("sql");
    let n: usize = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(1000);
    let start = Instant::now();
    for _ in 0..n {
        let _ = rustdb_pg_parser::parse(&sql);
    }
    println!(
        "{:.3} us per parse",
        start.elapsed().as_secs_f64() * 1e6 / n as f64
    );
    println!("size_of Node = {}", std::mem::size_of::<rustdb_pg_parser::Node>());
    println!(
        "size_of Value = {}",
        std::mem::size_of::<rustdb_pg_parser::nodes::Value>()
    );
}
