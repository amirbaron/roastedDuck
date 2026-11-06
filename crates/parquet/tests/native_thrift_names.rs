//! C++ Thrift bytes cover raw name and path round trips.
#[path = "../examples/thrift_names.rs"]
#[allow(dead_code, reason = "reuse the oracle CLI comparison routine")]
mod codec;
#[test]
fn native_thrift_names_match_cpp() {
    for line in include_str!("data/native_thrift_names_1.5.6.tsv").lines() {
        codec::compare(line).unwrap();
    }
}
