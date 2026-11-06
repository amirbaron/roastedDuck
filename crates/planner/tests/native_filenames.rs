//! Independently recorded native filename-pattern wire and deterministic paths.
#[path = "../examples/support/native_filenames.rs"]
mod codec;
#[test]
fn native_filename_patterns_match_cpp() {
    for line in include_str!("data/native_filenames_1.5.6.tsv").lines() {
        codec::compare(line).unwrap();
    }
}

#[test]
fn native_export_path_literals_match_cpp() {
    for line in include_str!("data/native_export_literals_1.5.6.tsv").lines() {
        codec::compare(line).unwrap();
    }
}
