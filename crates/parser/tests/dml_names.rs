//! C++ 1.5.6 native INSERT/JOIN/COPY names, including raw bytes.
#[path = "../examples/support/dml_names.rs"]
mod dml_names;
#[test]
fn native_statement_names_match_cpp() -> roastedduck_common::Result<()> {
    let mut count = 0;
    for line in include_str!("fixtures/dml_names_1.5.6.tsv").lines() {
        dml_names::compare(line)?;
        count += 1;
    }
    assert_eq!(count, 24);
    Ok(())
}
