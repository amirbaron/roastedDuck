//! Parses each argument and prints the parse tree as JSON (or the error).
//!
//! `cargo run -p rustdb-pg-parser --example pg_parse -- "SELECT 1"`

fn main() {
    for sql in std::env::args().skip(1) {
        match rustdb_pg_parser::parse(&sql) {
            Ok(stmts) => println!("{}", rustdb_pg_parser::nodes::to_json(&stmts)),
            Err(err) => println!("ERROR {:?} {}", err.query_location(), err),
        }
    }
}
