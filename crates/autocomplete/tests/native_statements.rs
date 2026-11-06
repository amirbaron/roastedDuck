//! Actual C++ PEG transformer AST/source/error records for native metadata.
#[path = "../examples/support/native_statements.rs"]
mod codec;
#[test]
fn peg_native_statement_metadata_matches_cpp() {
    for line in include_str!("data/peg_statements_1.5.6.tsv").lines() {
        codec::compare(line).unwrap();
    }
}
