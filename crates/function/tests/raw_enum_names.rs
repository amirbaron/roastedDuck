//! Native ENUM dictionaries and values, independently recorded from C++ DuckDB 1.5.6.
#[path = "../examples/enum_name_codec.rs"]
#[allow(dead_code, reason = "reuse the oracle CLI's comparison routine")]
mod codec;
#[test]
fn native_enum_names_match_cpp() {
    for line in include_str!("data/raw_enum_names_1.5.6.tsv").lines() {
        codec::compare(line).unwrap();
    }
}
