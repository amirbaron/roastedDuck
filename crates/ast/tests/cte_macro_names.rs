//! C++ 1.5.6 CTE/macro/lambda name wires, including raw bytes.
#[path = "../examples/support/cte_macro_names.rs"]
mod cte_macro_names;
#[test]
fn native_cte_macro_lambda_names_match_cpp() -> rustdb_common::Result<()> {
    let mut count = 0;
    for line in include_str!("data/cte_macro_names_1.5.6.tsv").lines() {
        cte_macro_names::compare(line)?;
        count += 1;
    }
    assert_eq!(count, 32);
    Ok(())
}
