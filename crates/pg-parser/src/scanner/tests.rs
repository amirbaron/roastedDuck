//! Scanner tests: one per lexical rule of `scan.l`, checked against the token
//! stream DuckDB's flex scanner produces.

use super::{ScanOptions, Scanner};
use crate::semantic::SemValue;
use crate::tables::{YYMAXUTOK, YYTNAME, YYTRANSLATE};

/// Renders every token as `NAME`, `NAME(value)` or `'c'`, with its location.
fn scan(sql: &str) -> Vec<String> {
    scan_with(sql, true)
}

fn scan_with(sql: &str, preserve_identifier_case: bool) -> Vec<String> {
    let mut scanner = Scanner::new(
        sql,
        ScanOptions {
            preserve_identifier_case,
        },
    );
    let mut out = Vec::new();
    loop {
        let tok = match scanner.next_token() {
            Ok(tok) => tok,
            Err(err) => {
                out.push(format!("ERROR {} @{:?}", err.raw_message(), err.query_location()));
                return out;
            }
        };
        if tok.code == 0 {
            return out;
        }
        let code = usize::from(tok.code);
        let symbol = if code <= YYMAXUTOK {
            usize::from(YYTRANSLATE[code])
        } else {
            2
        };
        let name = if code < 256 && symbol != 2 {
            YYTNAME[symbol].to_owned()
        } else if code < 256 {
            format!("'{}'", char::from(tok.code as u8))
        } else {
            YYTNAME[symbol].to_owned()
        };
        let value = match tok.value {
            SemValue::Str(Some(s)) => format!("({})", s.to_str().expect("UTF-8 test input")),
            SemValue::Int(i) => format!("({i})"),
            SemValue::Keyword(k) => format!("({})", std::str::from_utf8(k).expect("ASCII keyword")),
            _ => String::new(),
        };
        out.push(format!("{name}{value}@{}", tok.loc));
    }
}

fn error(sql: &str) -> String {
    scan(sql).pop().unwrap_or_default()
}

#[test]
fn whitespace_and_comments() {
    assert_eq!(
        scan(" \t\n\r\x0c1 -- x\n2 /* a /* b */ c */ 3 --"),
        ["ICONST(1)@5", "ICONST(2)@12", "ICONST(3)@32"]
    );
    assert_eq!(scan("/*/ 1 */ 2 /**/ 3"), ["ICONST(2)@9", "ICONST(3)@16"]);
    assert_eq!(
        error("SELECT /* x"),
        "ERROR unterminated /* comment at or near \"/* x\" @Some(7)"
    );
    assert_eq!(
        error("/* /* */"),
        "ERROR unterminated /* comment at or near \"/* /* */\" @Some(0)"
    );
}

#[test]
fn identifiers_and_keywords() {
    assert_eq!(
        scan("SeLeCt Foo, \"Bar\"\"x\", _a$1, é"),
        [
            "SELECT(SeLeCt)@0",
            "IDENT(Foo)@7",
            "','@10",
            "IDENT(Bar\"x)@12",
            "','@20",
            "IDENT(_a$1)@22",
            "','@26",
            "IDENT(é)@28",
        ]
    );
    assert_eq!(scan_with("Foo \"Foo\"", false), ["IDENT(foo)@0", "IDENT(Foo)@4"]);
    assert_eq!(
        error("SELECT \"\""),
        "ERROR zero-length delimited identifier at or near \"\"\"\" @Some(7)"
    );
    assert_eq!(
        error("SELECT \"abc"),
        "ERROR unterminated quoted identifier at or near \"\"abc\" @Some(7)"
    );
}

#[test]
fn lookahead_filter() {
    assert_eq!(scan("NOT IN"), ["NOT_LA(NOT)@0", "IN_P(IN)@4"]);
    assert_eq!(scan("not like"), ["NOT_LA(not)@0", "LIKE(like)@4"]);
    assert_eq!(scan("NOT x"), ["NOT(NOT)@0", "IDENT(x)@4"]);
    assert_eq!(scan("NULLS FIRST"), ["NULLS_LA(NULLS)@0", "FIRST_P(FIRST)@6"]);
    assert_eq!(scan("WITH TIME"), ["WITH_LA(WITH)@0", "TIME(TIME)@5"]);
    assert_eq!(
        scan("WITH ORDINALITY"),
        ["WITH_LA(WITH)@0", "ORDINALITY(ORDINALITY)@5"]
    );
    assert_eq!(scan("WITH x"), ["WITH(WITH)@0", "IDENT(x)@5"]);
}

#[test]
fn integers_and_floats() {
    assert_eq!(
        scan("1 1_000 2147483647 2147483648"),
        [
            "ICONST(1)@0",
            "ICONST(1000)@2",
            "ICONST(2147483647)@8",
            "FCONST(2147483648)@19",
        ]
    );
    assert_eq!(
        scan("1.5 .5 1. 1e5 1.5E-3 1_0.0_1e1_0"),
        [
            "FCONST(1.5)@0",
            "FCONST(.5)@4",
            "FCONST(1.)@7",
            "FCONST(1e5)@10",
            "FCONST(1.5E-3)@14",
            "FCONST(1_0.0_1e1_0)@21",
        ]
    );
    // {realfail1}/{realfail2} give back the exponent marker
    assert_eq!(
        scan("1e 1.5e+x"),
        [
            "FCONST(1)@0",
            "IDENT(e)@1",
            "FCONST(1.5)@3",
            "IDENT(e)@6",
            "'+'@7",
            "IDENT(x)@8"
        ]
    );
    // {decimalfail}: 1..2 is an integer, DOT_DOT, an integer
    assert_eq!(scan("1..2"), ["ICONST(1)@0", "DOT_DOT@1", "ICONST(2)@3"]);
    // no hex/octal/binary literals and no trailing underscores
    assert_eq!(
        scan("0x1F 1_ 1__0"),
        [
            "ICONST(0)@0",
            "IDENT(x1F)@1",
            "ICONST(1)@5",
            "IDENT(_)@6",
            "ICONST(1)@8",
            "IDENT(__0)@9",
        ]
    );
}

#[test]
fn parameters() {
    assert_eq!(
        scan("$1 ?2 $3_0 ? $a $$"),
        [
            "PARAM(1)@0",
            "PARAM(2)@3",
            "PARAM(3)@6",
            "'?'@11",
            "'$'@13",
            "IDENT(a)@14",
            "ERROR unterminated dollar-quoted string at or near \"$$\" @Some(16)",
        ]
    );
}

#[test]
fn standard_strings() {
    assert_eq!(scan("'it''s' 'a\\n'"), ["SCONST(it's)@0", "SCONST(a\\n)@8"]);
    // adjacent literals concatenate only across a newline
    assert_eq!(scan("'a'\n'b' 'c'"), ["SCONST(ab)@0", "SCONST(c)@8"]);
    assert_eq!(scan("'a' -- c\n 'b'"), ["SCONST(ab)@0"]);
    assert_eq!(scan("'a'\n-- c\n'b'"), ["SCONST(ab)@0"]);
    assert_eq!(scan("'a' -1"), ["SCONST(a)@0", "'-'@4", "ICONST(1)@5"]);
    assert_eq!(
        error("SELECT 'abc"),
        "ERROR unterminated quoted string at or near \"'abc\" @Some(7)"
    );
}

#[test]
fn dollar_quoted_strings() {
    assert_eq!(
        scan("$$a'b$$ $t$x$y$t$ $a$x$$a$"),
        ["SCONST(a'b)@0", "SCONST(x$y)@8", "SCONST(x$)@18"]
    );
    assert_eq!(
        error("$tag$x$ta$"),
        "ERROR unterminated dollar-quoted string at or near \"$tag$x$ta$\" @Some(0)"
    );
}

#[test]
fn extended_strings() {
    assert_eq!(scan(r"E'a\nb\t\\\'\x41\101\q'"), ["SCONST(a\nb\t\\'AAq)@0"]);
    assert_eq!(scan(r"e'\xZ'"), ["SCONST(xZ)@0"]);
    // DuckDB stubs out the helpers these escapes need
    assert_eq!(error(r"E'\u0041'"), "ERROR unicode_to_utf8 NOT IMPLEMENTED @None");
    assert_eq!(error(r"E'\u12'"), "ERROR errhint NOT IMPLEMENTED @None");
    assert_eq!(error(r"E'\xFF'"), "ERROR pg_verifymbstr NOT IMPLEMENTED @None");
    assert_eq!(error(r"E'\0'"), "ERROR pg_verifymbstr NOT IMPLEMENTED @None");
    assert_eq!(
        error(r"x E'\uDC00'"),
        "ERROR invalid Unicode surrogate pair at or near \"E'\\uDC00\" @Some(2)"
    );
    assert_eq!(
        error(r"E'\uD800x'"),
        "ERROR invalid Unicode surrogate pair at or near \"E'\\uD800x\" @Some(0)"
    );
    assert_eq!(
        error(r"E'\u0000'"),
        "ERROR invalid Unicode escape value at or near \"E'\\u0000\" @Some(0)"
    );
}

#[test]
fn bit_and_hex_strings() {
    assert_eq!(
        scan("B'0101' x'1F' b'1'\n'0'"),
        ["BCONST(b0101)@0", "XCONST(x1F)@8", "BCONST(b10)@14"]
    );
    assert_eq!(scan("B'1''0'"), ["BCONST(b1)@0", "SCONST(0)@4"]);
    assert_eq!(
        error("B'01"),
        "ERROR unterminated bit string literal at or near \"B'01\" @Some(0)"
    );
    assert_eq!(
        error("X'01"),
        "ERROR unterminated hexadecimal string literal at or near \"X'01\" @Some(0)"
    );
}

#[test]
fn national_and_unicode_strings() {
    assert_eq!(scan("N'x'"), ["NCHAR(nchar)@0", "SCONST(x)@1"]);
    assert_eq!(scan("u& x"), ["IDENT(u)@0", "Op(&)@1", "IDENT(x)@3"]);
    assert_eq!(error("U&'abc'"), "ERROR pg_verifymbstr NOT IMPLEMENTED @None");
    assert_eq!(
        error(r"U&'d\0061t'"),
        "ERROR unicode_to_utf8 NOT IMPLEMENTED @None"
    );
    assert_eq!(
        error("U&'a' UESCAPE 'a'"),
        "ERROR invalid Unicode escape character at or near \"a'\" @Some(15)"
    );
    assert_eq!(
        error("U&\"\""),
        "ERROR zero-length delimited identifier at or near \"U&\"\"\" @Some(0)"
    );
}

#[test]
fn operators() {
    assert_eq!(
        scan(":: : := .. . -> ->> ** // => <= >= <> !="),
        [
            "TYPECAST@0",
            "SINGLE_COLON@3",
            "COLON_EQUALS@5",
            "DOT_DOT@8",
            "'.'@11",
            "SINGLE_ARROW@13",
            "DOUBLE_ARROW@16",
            "POWER_OF@20",
            "INTEGER_DIVISION@23",
            "EQUALS_GREATER@26",
            "LESS_EQUALS@29",
            "GREATER_EQUALS@32",
            "NOT_EQUALS@35",
            "NOT_EQUALS@38",
        ]
    );
    assert_eq!(
        scan("<=> ~ !~~* @> |/"),
        ["Op(<=>)@0", "Op(~)@4", "Op(!~~*)@6", "Op(@>)@11", "Op(|/)@14"]
    );
}

#[test]
fn operator_trimming() {
    // '+'/'-' cannot end a multi-character SQL operator...
    assert_eq!(scan("a=-1"), ["IDENT(a)@0", "'='@1", "'-'@2", "ICONST(1)@3"]);
    assert_eq!(scan("**-"), ["Op(**)@0", "'-'@2"]);
    assert_eq!(scan("->-"), ["Op(->)@0", "'-'@2"]);
    assert_eq!(scan(">=-"), ["GREATER_EQUALS@0", "'-'@2"]);
    // ...unless it contains a non-SQL operator character
    assert_eq!(scan("@-"), ["Op(@-)@0"]);
    // embedded comment starts end the operator
    assert_eq!(scan("+/* c */1 *--x"), ["'+'@0", "ICONST(1)@8", "'*'@10"]);
    let long = "+".repeat(63) + "@";
    assert!(error(&format!("SELECT {long}")).starts_with("ERROR operator too long"));
}

#[test]
fn other_characters() {
    assert_eq!(scan("# { } \\ ?"), ["'#'@0", "'{'@2", "'}'@4", "'\\'@6", "'?'@8"]);
    // input stops at a NUL byte, like the C scanner
    assert_eq!(scan("1\0 2"), ["ICONST(1)@0"]);
}
