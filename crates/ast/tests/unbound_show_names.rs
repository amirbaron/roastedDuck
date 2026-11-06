//! Frozen C++ records for both unbound-type wire encodings and native SHOW metadata.
#[path = "../examples/unbound_show_names.rs"]
#[allow(dead_code, reason = "reuse the oracle CLI comparison routine")]
mod codec;
#[test]
fn native_unbound_and_show_names_match_cpp() {
    for line in include_str!("data/unbound_show_names_1.5.6.tsv").lines() {
        codec::compare(line).unwrap();
    }
}
