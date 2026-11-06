//! Compare saved C++ native CTE/macro/lambda name cases from stdin.
#[path = "support/cte_macro_names.rs"]
mod cte_macro_names;
use std::io::{self, BufRead};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut count = 0;
    for line in io::stdin().lock().lines() {
        cte_macro_names::compare(&line?)?;
        count += 1;
    }
    println!("{count} C++ CTE/macro/lambda native name cases matched");
    Ok(())
}
