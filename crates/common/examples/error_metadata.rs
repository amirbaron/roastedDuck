//! Compare saved C++ native exception metadata from stdin.
#[path = "support/error_metadata.rs"]
mod error_metadata;
use std::io::{self, BufRead};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut count = 0;
    for line in io::stdin().lock().lines() {
        error_metadata::compare(&line?)?;
        count += 1;
    }
    println!("{count} C++ native exception metadata cases matched");
    Ok(())
}
