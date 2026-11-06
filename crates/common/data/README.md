`utf8proc_render.bin` contains the character width, grapheme boundary class and
Indic conjunct class from the utf8proc bundled with frozen DuckDB 1.5.6 (Unicode
15.1.0). Each six-byte little-endian record is a range start (u32) and properties
(u16): width bits 0..1, boundary bits 2..6, Indic bits 7..8. The final sentinel is
0x110000; out-of-range codepoints use utf8proc's default property.

Regenerate with `REF=<DuckDB 1.5.6 source> scripts/utf8proc-render-data.sh <external-build-dir>`.
The exporter runs offline and the Rust library includes only 13,032 bytes of data.
The translated grapheme rules and this data retain the notice in `utf8proc-LICENSE`.

The C++ renderer oracle is `scripts/tree-renderer-bytes.cpp`. Supply hexadecimal
byte strings, one per line, on stdin. It emits raw outcome TSV for six formats,
three fields and one/two-child trees; `tree_name_bytes` compares the actual Rust
renderer with those records. The checked-in golden subset is independently
produced by frozen C++ 1.5.6, including successes and exact exceptions.
