//! Independently generated full parse-tree/errors from frozen C++ DuckDB 1.5.6.
#[path = "../examples/support/pg_native.rs"]
mod codec;
fn unhex(text: &str) -> Option<Vec<u8>> {
    let (pairs, rest) = text.as_bytes().as_chunks::<2>();
    if !rest.is_empty() {
        return None;
    }
    pairs
        .iter()
        .map(|pair| {
            let digit = |b| match b {
                b'0'..=b'9' => Some(b - b'0'),
                b'a'..=b'f' => Some(b - b'a' + 10),
                _ => None,
            };
            Some(digit(pair[0])? * 16 + digit(pair[1])?)
        })
        .collect()
}
#[test]
fn native_queries_match_cpp_trees_errors_and_positions() {
    let mut count = 0;
    for line in include_str!("data/native_queries_1.5.6.tsv").lines() {
        let fields: Vec<_> = line.split('\t').collect();
        assert_eq!(fields.len(), 3);
        let query = unhex(fields[1]).expect("oracle input hex");
        let actual = codec::render(&query, fields[0] == "1");
        assert_eq!(
            actual.as_bytes(),
            unhex(fields[2]).expect("oracle output hex"),
            "case {count}, preserve={}",
            fields[0]
        );
        count += 1;
    }
    assert!(count >= 120);
}
