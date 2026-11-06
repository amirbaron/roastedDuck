//! Native SQL entry point checked against independent frozen C++ records.
#[path = "../examples/support/native_queries.rs"]
mod queries;
#[path = "../examples/support/native_statements.rs"]
mod statements;
#[test]
fn raw_select_names_and_errors_match_cpp() {
    for line in include_str!("fixtures/raw_queries_1.5.6.tsv").lines() {
        queries::compare(line).unwrap();
    }
}
#[test]
fn native_statement_codecs_source_and_copy_keys_match_cpp() {
    for line in include_str!("fixtures/raw_statements_1.5.6.tsv").lines() {
        statements::compare(line).unwrap();
    }
}
