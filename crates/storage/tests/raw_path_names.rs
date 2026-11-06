//! C++ 1.5.6 native field-name paths through both index codecs.
#[path = "../examples/path_name_codec.rs"]
#[allow(dead_code, reason = "reuse the oracle CLI comparison routine")]
mod codec;
#[test]
fn native_field_path_wire_matches_cpp() {
    for line in include_str!("data/raw_path_names_1.5.6.tsv").lines() {
        codec::compare(line).unwrap();
    }
}
