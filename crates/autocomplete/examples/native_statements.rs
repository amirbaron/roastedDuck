//! Compare full native SQL input, statement serialization/source, and `ErrorData` JSON.
#[path = "support/native_statements.rs"]
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
        return Err("native statement comparison failed".into());
    }
    Ok(())
}
