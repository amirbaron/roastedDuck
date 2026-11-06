//! Exact C++ outcomes for JSON function/cast name validation and output.
#[path = "../examples/native_names.rs"]
#[allow(dead_code, reason = "reuse the oracle CLI comparison routine")]
mod codec;
#[test]
fn native_json_names_match_cpp() {
    for line in include_str!("data/native_names_1.5.6.tsv").lines() {
        codec::compare(line).unwrap();
    }
}
