//! Frozen C++ pivot/UNPIVOT and native value rendering/error regression cases.
#[path = "../examples/support/table_names.rs"]
mod table_names;
#[test]
fn native_table_and_value_names_match_cpp() -> rustdb_common::Result<()> {
    let mut count = 0;
    for line in include_str!("fixtures/table_names_1.5.6.tsv").lines() {
        table_names::compare(line)?;
        count += 1;
    }
    assert_eq!(count, 72);
    Ok(())
}
