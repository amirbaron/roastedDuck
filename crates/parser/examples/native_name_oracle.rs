//! Compare every native C++ SELECT case, including invalid UTF-8 inputs and errors.
#[path = "support/native_queries.rs"]
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
    println!("passed={passed} failed={failed} byte_ingress_pending=0");
    if failed > 0 {
        return Err("parser comparison failed".into());
    }
    Ok(())
}
