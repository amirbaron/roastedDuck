//! Raw parser/`current_query` kernel outcomes from C++ DuckDB 1.5.6.
#[path = "../examples/native_current_query.rs"]
#[allow(dead_code, reason = "reuse oracle comparator")]
mod codec;
#[test]
fn current_query_matches_cpp_native_source() {
    for line in include_str!("data/native_current_query_1.5.6.tsv").lines() {
        codec::compare(line).unwrap();
    }
}
