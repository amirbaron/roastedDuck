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

#[test]
fn fast_float_path_matches_correct_rounding() {
    let mut seed: u64 = 0x2545_F491_4F6C_DD1D;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for _ in 0..200_000 {
        let int_digits = (next() % 12) as usize;
        let frac_digits = (next() % 12) as usize + 1;
        let mut text = String::new();
        if next() % 2 == 0 {
            text.push('-');
        }
        if int_digits == 0 {
            text.push('0');
        }
        for i in 0..int_digits {
            let digit = if i == 0 { next() % 9 + 1 } else { next() % 10 };
            text.push(char::from(b'0' + digit as u8));
        }
        text.push('.');
        for _ in 0..frac_digits {
            text.push(char::from(b'0' + (next() % 10) as u8));
        }
        if next() % 3 == 0 {
            let exponent = (next() % 50) as i64 - 25;
            text.push('e');
            text.push_str(&exponent.to_string());
        }
        let mut arena = JsonArena::default();
        let result = arena.read(text.as_bytes(), false).unwrap();
        let expected: f64 = text.parse().unwrap();
        assert_eq!(
            arena.doc().get_real(result.root).to_bits(),
            expected.to_bits(),
            "{text}"
        );
    }
}

#[test]
fn nested_containers_and_long_strings() {
    let long = "x".repeat(100);
    let input = format!(r#"{{"a":[{{"b":[[1],[2,{{"c":"{long}"}}]]}},3],"d":{{}},"e":"q\"{long}é"}}"#);
    let mut arena = JsonArena::default();
    let result = arena.read(input.as_bytes(), false).unwrap();
    let doc = arena.doc();
    assert_eq!(doc.obj_size(result.root), 3);
    assert_eq!(doc.subtree_len(result.root), 18);
    let mut out = Vec::new();
    write_val(&mut out, &doc, result.root);
    assert_eq!(String::from_utf8(out).unwrap(), input);
    assert_eq!(parse("18446744073709551615").unwrap(), "18446744073709551615");
    assert_eq!(parse("18446744073709551616").unwrap(), "18446744073709551616");
    assert_eq!(parse("-9223372036854775808").unwrap(), "-9223372036854775808");
    assert_eq!(parse("1234567890123456789").unwrap(), "1234567890123456789");
}

/// Timing probes: `cargo test --release -p roastedduck-json speed -- --ignored --nocapture`
/// (`PROBE_ITERS` passes over a million documents).
fn probe_docs() -> (Vec<String>, u32) {
    let iters = std::env::var("PROBE_ITERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(5);
    if let Ok(file) = std::env::var("PROBE_FILE") {
        let text = std::fs::read_to_string(file).unwrap();
        let lines: Vec<String> = text.lines().map(str::to_owned).collect();
        let docs = lines.iter().cycle().take(1_000_000).cloned().collect();
        return (docs, iters);
    }
    let docs = (0..1_000_000u64)
        .map(|i| {
            format!(
                r#"{{"a":{i},"b":{{"c":"x{i}","d":[1,2,3]}},"e":{}}}"#,
                i as f64 * 0.5
            )
        })
        .collect();
    (docs, iters)
}

#[test]
#[ignore = "timing probe"]
fn parse_speed() {
    let (docs, iters) = probe_docs();
    let mut arena = JsonArena::default();
    let start = std::time::Instant::now();
    let mut total = 0usize;
    for _ in 0..iters {
        for doc in &docs {
            arena.reset();
            let result = arena.read(doc.as_bytes(), false).unwrap();
            total += arena.doc().subtree_len(result.root);
        }
    }
    println!(
        "parse: {:?} per doc ({total})",
        start.elapsed() / (iters * 1_000_000)
    );
}

#[test]
#[ignore = "timing probe"]
fn extract_speed() {
    let (docs, iters) = probe_docs();
    let mut arena = JsonArena::default();
    let start = std::time::Instant::now();
    let mut out = Vec::new();
    let path = crate::json_common::CompiledPath::new(b"$.b.c");
    for _ in 0..iters {
        out.clear();
        for doc in &docs {
            arena.reset();
            let result = arena.read(doc.as_bytes(), false).unwrap();
            let view = arena.doc();
            if let Some(val) = path.get(&view, result.root) {
                write_val(&mut out, &view, val);
            }
        }
    }
    println!(
        "extract speed: {:?} per doc ({})",
        start.elapsed() / (iters * 1_000_000),
        out.len()
    );
}

#[test]
#[ignore = "timing probe"]
fn bulk_speed() {
    let (docs, iters) = probe_docs();
    let bulk: Vec<String> = docs
        .chunks(1000)
        .map(|chunk| format!("[{}]", chunk.join(",")))
        .collect();
    let mut arena = JsonArena::default();
    let start = std::time::Instant::now();
    let mut total = 0usize;
    for _ in 0..iters {
        for doc in &bulk {
            arena.reset();
            let result = arena.read(doc.as_bytes(), false).unwrap();
            total += arena.doc().subtree_len(result.root);
        }
    }
    println!(
        "bulk speed: {:?} per doc ({total})",
        start.elapsed() / (iters * 1_000_000)
    );
    let start = std::time::Instant::now();
    for _ in 0..iters {
        for _ in &docs {
            arena.reset();
            let result = arena.read(b"1", false).unwrap();
            total += arena.doc().subtree_len(result.root);
        }
    }
    println!(
        "tiny speed: {:?} per doc ({total})",
        start.elapsed() / (iters * 1_000_000)
    );
}
