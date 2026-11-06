//! Compare saved C++ INSERT/JOIN/COPY native-name cases from stdin.
#[path = "support/dml_names.rs"]
mod dml_names;
use std::io::{self, BufRead};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut count = 0;
    for line in io::stdin().lock().lines() {
        dml_names::compare(&line?)?;
        count += 1;
    }
    println!("{count} C++ native INSERT/JOIN/COPY cases matched");
    Ok(())
}
