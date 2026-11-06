//! Actual C++ `GeoParquet` JSON values and exact serialization exceptions.
#[path = "../examples/geo_names.rs"]
#[allow(dead_code, reason = "reuse the oracle CLI comparison routine")]
mod codec;
#[test]
fn native_geo_names_match_cpp() {
    for line in include_str!("data/native_geo_names_1.5.6.tsv").lines() {
        codec::compare(line).unwrap();
    }
}
