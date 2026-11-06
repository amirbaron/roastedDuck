//! Compare raw hive partition behavior with C++ DuckDB 1.5.6.
#[path = "support/native_hive.rs"]
mod codec;
use std::io::{self, BufRead};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (mut passed, mut failed) = (0, 0);
    for (index, line) in io::stdin().lock().lines().enumerate() {
        match codec::compare(&line?) {
            Ok(()) => passed += 1,
            Err(error) => {
                failed += 1;
                eprintln!("case {index}: {error}");
            }
        }
    }
    println!("passed={passed} failed={failed}");
    if failed > 0 {
        return Err("native hive comparison failed".into());
    }
    Ok(())
}
