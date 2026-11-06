//! C++ 1.5.6 native metadata, including invalid UTF-8 and embedded NULs.
#[path = "../examples/support/error_metadata.rs"]
mod error_metadata;
#[test]
fn native_exception_metadata_matches_cpp() -> rustdb_common::Result<()> {
    let mut count = 0;
    for line in include_str!("data/error_metadata_1.5.6.tsv").lines() {
        error_metadata::compare(line)?;
        count += 1;
    }
    assert_eq!(count, 16);
    Ok(())
}
