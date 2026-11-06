//! Native JSON option state and exact errors from C++ DuckDB 1.5.6.
#[path = "../examples/native_options.rs"]
#[allow(dead_code, reason = "reuse oracle comparison routine")]
mod codec;
#[test]
fn native_json_options_match_cpp() {
    for line in include_str!("data/native_options_1.5.6.tsv").lines() {
        codec::compare(line).unwrap();
    }
}
