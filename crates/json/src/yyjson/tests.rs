use super::*;

fn parse(input: &str) -> Result<String, ReadError> {
    let mut arena = JsonArena::default();
    let result = arena.read(input.as_bytes(), false)?;
    let mut out = Vec::new();
    write_val(&mut out, &arena.doc(), result.root);
    Ok(String::from_utf8(out).unwrap())
}

fn error(input: &str) -> (usize, &'static str) {
    let err = parse(input).unwrap_err();
    (err.pos, err.msg)
}

#[test]
fn round_trips_values() {
    assert_eq!(
        parse(" { \"a\" : [1, -2, 3.5, true, false, null] } ").unwrap(),
        r#"{"a":[1,-2,3.5,true,false,null]}"#
    );
    assert_eq!(parse("[]").unwrap(), "[]");
    assert_eq!(parse("{}").unwrap(), "{}");
    assert_eq!(parse("[[],{},[[]]]").unwrap(), "[[],{},[[]]]");
    assert_eq!(parse(r#""a\"b\\c\/dAé😀""#).unwrap(), "\"a\\\"b\\\\c/dAé😀\"");
    assert_eq!(parse("\"\\u0001\\u000b\"").unwrap(), "\"\\u0001\\u000B\"");
}

#[test]
fn numbers_follow_yyjson() {
    assert_eq!(parse("-0").unwrap(), "0");
    assert_eq!(parse("-0.0").unwrap(), "-0.0");
    assert_eq!(parse("0e5").unwrap(), "0.0");
    assert_eq!(parse("1e20").unwrap(), "100000000000000000000.0");
    assert_eq!(parse("1e21").unwrap(), "1e21");
    assert_eq!(parse("1.5e-7").unwrap(), "1.5e-7");
    assert_eq!(parse("0.000001").unwrap(), "0.000001");
    assert_eq!(parse("5e-324").unwrap(), "5e-324");
    assert_eq!(parse("0.1").unwrap(), "0.1");
    assert_eq!(
        parse("123456789012345678901234").unwrap(),
        "123456789012345678901234"
    );
    assert_eq!(parse("-9223372036854775808").unwrap(), "-9223372036854775808");
    assert_eq!(parse("-9223372036854775809").unwrap(), "-9223372036854775809");
    assert_eq!(parse("18446744073709551615").unwrap(), "18446744073709551615");
    assert_eq!(parse("1e400").unwrap(), "1e400");
    assert_eq!(parse("[NaN, -inf, Infinity]").unwrap(), "[NaN,-inf,Infinity]");
}

#[test]
fn trailing_commas_are_allowed() {
    assert_eq!(parse("{\"a\": 1,}").unwrap(), r#"{"a":1}"#);
    assert_eq!(parse("[1,]").unwrap(), "[1]");
}

#[test]
fn errors_match_yyjson() {
    assert_eq!(error(""), (0, "input length is 0"));
    assert_eq!(error("  "), (0, "input data is empty"));
    assert_eq!(error("[1,2"), (4, "unexpected end of data"));
    assert_eq!(error("{\"a\":tru}"), (5, "invalid literal"));
    assert_eq!(error("nul"), (3, "unexpected end of data"));
    assert_eq!(error("\"abc"), (4, "unexpected end of data"));
    assert_eq!(error("01"), (0, "number with leading zero is not allowed"));
    assert_eq!(error("[1] x"), (4, "unexpected content after document"));
    assert_eq!(error("[1,,2]"), (3, "unexpected character"));
}

#[test]
fn pretty_prints_like_yyjson() {
    let mut arena = JsonArena::default();
    let root = arena
        .read(br#"{"a":[1,{"b":null}],"c":{},"d":[]}"#, false)
        .unwrap()
        .root;
    let mut out = Vec::new();
    write_val_pretty(&mut out, &arena.doc(), root);
    assert_eq!(
        String::from_utf8(out).unwrap(),
        "{\n    \"a\": [\n        1,\n        {\n            \"b\": null\n        }\n    ],\n    \"c\": {},\n    \"d\": []\n}"
    );
}

#[test]
fn pointers_resolve() {
    let mut arena = JsonArena::default();
    let root = arena.read(br#"{"a/b":[10,20],"~":1}"#, false).unwrap().root;
    let doc = arena.doc();
    let found = ptr_get(&doc, root, b"/a~1b/1").unwrap();
    assert_eq!(doc.get_uint(found), 20);
    assert_eq!(doc.get_uint(ptr_get(&doc, root, b"/~0").unwrap()), 1);
    assert!(ptr_get(&doc, root, b"/a~1b/2").is_none());
}

#[test]
fn merge_patch_follows_rfc_7386() {
    let mut arena = JsonArena::default();
    let orig = arena.read(br#"{"a":1,"b":{"c":2,"d":3}}"#, false).unwrap().root;
    let patch = arena
        .read(br#"{"a":null,"b":{"c":4},"e":5}"#, false)
        .unwrap()
        .root;
    let doc = arena.doc();
    let merged = merge_patch(
        Some(&MutVal::from_doc(&doc, orig)),
        &MutVal::from_doc(&doc, patch),
    );
    let mut out = Vec::new();
    merged.write(&mut out);
    assert_eq!(String::from_utf8(out).unwrap(), r#"{"b":{"d":3,"c":4},"e":5}"#);
}
