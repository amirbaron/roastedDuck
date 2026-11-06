//! Independent C++ names at Arrow schema and dictionary-buffer boundaries.
#[path = "../examples/native_dictionary.rs"]
#[allow(dead_code, reason = "reuse oracle CLI comparison")]
mod dictionary;
use dictionary::schema;
#[test]
fn schema_names_match_cpp() {
    for line in include_str!("data/native_names_1.5.6.tsv").lines() {
        schema::compare(line).unwrap();
    }
}
#[test]
fn enum_dictionary_bytes_match_cpp() {
    for line in include_str!("data/native_dictionary_1.5.6.tsv").lines() {
        dictionary::compare(line).unwrap();
    }
}
