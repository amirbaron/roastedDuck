//! Native query/error bytes and grapheme offsets match C++ DuckDB 1.5.6.
#[path = "../examples/query_context_bytes.rs"]
#[allow(dead_code, reason = "reuse the oracle comparison routine")]
mod codec;
#[test]
fn native_context_matches_cpp() {
    for line in include_str!("data/query_context_bytes_1.5.6.tsv").lines() {
        codec::compare(line).unwrap();
    }
}

#[test]
fn rethrow_sanitizes_nul_added_by_query_context() {
    let error = roastedduck_common::Error::parser("bad query")
        .with_position(Some(1))
        .with_query_context_bytes(b"a\0b");
    assert!(error.raw_message_bytes().contains(&0));
    let error = error.rethrown();
    assert!(!error.raw_message_bytes().contains(&0));
    assert_eq!(
        error.raw_message_bytes(),
        b"bad query\n\nLINE 1: a\\0b\n         ^"
    );
}
