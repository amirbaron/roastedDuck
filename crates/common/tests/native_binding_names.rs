//! Frozen C++ byte-name set, similarity and candidate-suggestion cases.
#[path = "../examples/support/native_binding_names.rs"]
mod native_binding_names;
#[test]
fn native_name_lookup_and_suggestions_match_cpp() -> rustdb_common::Result<()> {
    let mut count = 0;
    for line in include_str!("data/native_binding_names_1.5.6.tsv").lines() {
        native_binding_names::compare(line)?;
        count += 1;
    }
    assert_eq!(count, 8);
    Ok(())
}
