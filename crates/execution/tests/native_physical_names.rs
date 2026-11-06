//! Projection metadata and operator-log construction, independently recorded from C++.
#[path = "../examples/physical_name_codec.rs"]
#[allow(dead_code, reason = "reuse the oracle CLI comparison routine")]
mod codec;
#[test]
fn native_physical_names_match_cpp() {
    for line in include_str!("data/native_physical_names_1.5.6.tsv").lines() {
        codec::compare(line).unwrap();
    }
}
