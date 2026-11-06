//! Compare saved C++ pivot/UNPIVOT codecs and value name rendering from stdin.
#[path = "support/table_names.rs"]
mod table_names;
use std::io::{self, BufRead};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut count = 0;
    for line in io::stdin().lock().lines() {
        table_names::compare(&line?)?;
        count += 1;
    }
    println!("{count} C++ native pivot/value outcomes matched");
    Ok(())
}
