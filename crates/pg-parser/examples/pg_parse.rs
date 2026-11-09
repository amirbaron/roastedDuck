//! Parses each argument and prints the parse tree as JSON (or the error).
//!
//! `cargo run -p roastedduck-pg-parser --example pg_parse -- "SELECT 1"`

fn main() -> std::io::Result<()> {
    use std::io::Write;
    let mut stdout = std::io::stdout().lock();
    for sql in std::env::args().skip(1) {
        match roastedduck_pg_parser::parse(&sql) {
            Ok(stmts) => {
                stdout.write_all(&roastedduck_pg_parser::nodes::to_json(&stmts))?;
                stdout.write_all(b"\n")?;
            }
            Err(err) => {
                writeln!(stdout, "ERROR {:?} {}", err.query_location(), err)?;
            }
        }
    }
    Ok(())
}
