//! Compare native path operations with the frozen C++ local filesystem.
#[path = "support/native_paths.rs"]
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
        return Err("native filesystem comparison failed".into());
    }
    Ok(())
}
