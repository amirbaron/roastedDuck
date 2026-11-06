//! Compare every saved C++ native `IndexStorageInfo` case from stdin.
#[path = "support/index_names.rs"]
mod index_names;
use std::io::{self, BufRead};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut count = 0;
    for line in io::stdin().lock().lines() {
        index_names::compare(&line?)?;
        count += 1;
    }
    println!("{count} C++ index storage wire cases matched");
    Ok(())
}
