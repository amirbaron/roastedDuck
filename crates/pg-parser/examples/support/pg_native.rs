//! Byte-oriented diagnostic format shared by the C++ comparator and frozen fixture tests.
use roastedduck_common::{ByteString, ErrorKind, byte_format};
use roastedduck_pg_parser::{ParserOptions, nodes, parse_bytes_with_options, strip_unicode_spaces_bytes};
use std::fmt::Write as _;

fn json_string(out: &mut ByteString, s: &[u8]) {
    out.push(b'"');
    for &byte in s {
        match byte {
            b'"' => out.push_bytes(b"\\\""),
            b'\\' => out.push_bytes(b"\\\\"),
            b'\n' => out.push_bytes(b"\\n"),
            b'\r' => out.push_bytes(b"\\r"),
            b'\t' => out.push_bytes(b"\\t"),
            byte if byte < 0x20 => {
                let _ = write!(out, "\\u{byte:04x}");
            }
            byte => out.push(byte),
        }
    }
    out.push(b'"');
}

/// The Rust result in the oracle's format.
pub fn render(sql: &[u8], preserve_identifier_case: bool) -> ByteString {
    let stripped = strip_unicode_spaces_bytes(sql);
    match parse_bytes_with_options(
        stripped.as_deref().unwrap_or(sql),
        &ParserOptions {
            preserve_identifier_case,
        },
    ) {
        Ok(stmts) => byte_format!("{{\"ok\":true,\"tree\":{}}}", nodes::to_json(&stmts)),
        Err(err) => {
            let mut out = ByteString::from("{\"ok\":false,\"msg\":");
            if err.kind() == ErrorKind::Parser {
                json_string(&mut out, err.raw_message_bytes());
            } else {
                json_string(&mut out, &err.to_bytes());
            }
            match err.query_location() {
                Some(pos) => {
                    let _ = write!(out, ",\"pos\":{pos}}}");
                }
                None => out.push_bytes(",\"pos\":null}"),
            }
            out
        }
    }
}
