//! Compare saved C++ native lookup and suggestion cases from stdin.
#[path = "support/native_binding_names.rs"]
mod native_binding_names;
use std::io::{self, BufRead};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut count = 0;
    for line in io::stdin().lock().lines() {
        native_binding_names::compare(&line?)?;
        count += 1;
    }
    println!("{count} C++ native binding-name helper cases matched");
    Ok(())
}
