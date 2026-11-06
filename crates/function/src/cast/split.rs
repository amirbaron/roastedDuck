//! Splitting the text form of nested values into their parts (DuckDB's `VectorStringToList`,
//! `VectorStringToStruct` and `VectorStringToMap` helpers in `vector_cast_helpers.cpp`).
//!
//! The splitters walk the input once, find the raw extent of every element (honouring
//! quotes, backslash escapes and nested brackets) and hand each element to a callback as the
//! VARCHAR text DuckDB would cast into the child type: `None` for an unquoted
//! case-insensitive `NULL`, otherwise the text with surrounding whitespace trimmed and
//! top-level quotes and escapes removed (text inside nested brackets is kept verbatim).
//! Elements that need no unescaping are passed as sub-slices of the input; the others are
//! written into a caller-provided scratch buffer, so splitting does not allocate in the
//! common case.
//!
//! The splitters only report success or failure; the cast layer turns a failure into
//! `Type VARCHAR with value '...' can't be cast to the destination type ...`. A failure can
//! happen after some elements were already reported, exactly like in DuckDB.

// The VARCHAR -> LIST/STRUCT/MAP casts consume this module.
#![cfg_attr(not(test), allow(dead_code))]

/// DuckDB's `StringUtil::CharacterIsSpace`.
const fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\x0b' | b'\x0c' | b'\r')
}

/// The closing bracket matching an opening one.
const fn closer(c: u8) -> Option<u8> {
    match c {
        b'[' => Some(b']'),
        b'{' => Some(b'}'),
        b'(' => Some(b')'),
        _ => None,
    }
}

/// DuckDB's `IsNull`: exactly the four characters `null`, in any case.
fn is_null(raw: &[u8]) -> bool {
    raw.eq_ignore_ascii_case(b"null")
}

/// A stack of expected closing brackets that only allocates beyond `INLINE` nesting levels.
#[derive(Debug)]
struct ScopeStack {
    inline: [u8; ScopeStack::INLINE],
    len: usize,
    spill: Vec<u8>,
}

impl ScopeStack {
    const INLINE: usize = 32;

    const fn new() -> Self {
        Self {
            inline: [0; Self::INLINE],
            len: 0,
            spill: Vec::new(),
        }
    }

    const fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn push(&mut self, closer: u8) {
        match self.inline.get_mut(self.len) {
            Some(slot) => *slot = closer,
            None => self.spill.push(closer),
        }
        self.len += 1;
    }

    fn top(&self) -> Option<u8> {
        let idx = self.len.checked_sub(1)?;
        self.inline.get(idx).or_else(|| self.spill.last()).copied()
    }

    fn pop(&mut self) {
        if self.len > Self::INLINE {
            self.spill.pop();
        }
        self.len = self.len.saturating_sub(1);
    }
}

/// The inclusive byte range `(first, last)` of an element's non-whitespace extent.
type Span = Option<(usize, usize)>;

/// Scanning state shared by the splitters (DuckDB's `StringCastInputState`).
#[derive(Debug)]
struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
    /// Whether the previous character was an unquoted backslash. Like in DuckDB this
    /// survives separators (`[a\,"b"]` treats the `"` as escaped) and is only reset by
    /// skipped whitespace.
    escaped: bool,
}

impl<'a> Cursor<'a> {
    const fn new(buf: &'a [u8]) -> Self {
        Self {
            buf,
            pos: 0,
            escaped: false,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.buf.get(self.pos).copied()
    }

    /// Consumes the current character if it is `c`.
    fn eat(&mut self, c: u8) -> bool {
        let found = self.peek() == Some(c);
        if found {
            self.pos += 1;
        }
        found
    }

    /// The raw text of `span` (empty for an empty element).
    fn raw(&self, span: Span) -> &'a [u8] {
        span.and_then(|(first, last)| self.buf.get(first..=last))
            .unwrap_or_default()
    }

    fn skip_whitespace(&mut self) {
        while self.peek().is_some_and(is_space) {
            self.pos += 1;
            self.escaped = false;
        }
    }

    /// Skips trailing whitespace after the closing bracket at the cursor and checks that
    /// nothing else follows.
    fn finish(mut self) -> bool {
        self.pos += 1;
        self.skip_whitespace();
        self.pos == self.buf.len()
    }

    /// Extends `span` to the current position.
    fn mark(&self, span: &mut Span, first: usize) {
        *span = Some((span.map_or(first, |(first, _)| first), self.pos));
    }

    /// From an opening quote, moves to the matching unescaped closing quote (DuckDB's
    /// `SkipToCloseQuotes`).
    fn skip_to_close_quote(&mut self) -> bool {
        let Some(quote) = self.peek() else { return false };
        self.pos += 1;
        while let Some(c) = self.peek() {
            if c == quote && !self.escaped {
                return true;
            }
            self.escaped = c == b'\\' && !self.escaped;
            self.pos += 1;
        }
        false
    }

    /// From an opening bracket, moves to the bracket that closes it (DuckDB's
    /// `SkipToClose`). Quoted text is skipped; a closing bracket that does not match the
    /// innermost open one is ignored.
    fn skip_to_close(&mut self) -> bool {
        let mut scopes = ScopeStack::new();
        while let Some(c) = self.peek() {
            let mut set_escaped = false;
            if c == b'"' || c == b'\'' {
                if !self.escaped && !self.skip_to_close_quote() {
                    return false;
                }
            } else if let Some(close) = closer(c) {
                scopes.push(close);
            } else if scopes.top() == Some(c) {
                scopes.pop();
                if scopes.is_empty() {
                    return true;
                }
            } else if c == b'\\' {
                set_escaped = true;
            }
            self.escaped = set_escaped;
            self.pos += 1;
        }
        false
    }

    /// Consumes one character (or one quoted string / bracketed value) of an element,
    /// extending `span` over non-whitespace (DuckDB's `ValueStateTransition`).
    fn value_step(&mut self, span: &mut Span) -> bool {
        let Some(c) = self.peek() else { return false };
        let first = self.pos;
        let mut set_escaped = false;
        match c {
            b'"' | b'\'' => {
                if !self.escaped && !self.skip_to_close_quote() {
                    return false;
                }
                self.mark(span, first);
            }
            b'[' | b'{' | b'(' => {
                if !self.skip_to_close() {
                    return false;
                }
                self.mark(span, first);
            }
            b'\\' => {
                set_escaped = true;
                self.mark(span, first);
            }
            _ if !is_space(c) => self.mark(span, first),
            _ => {}
        }
        self.escaped = set_escaped;
        self.pos += 1;
        true
    }

    /// Scans an element up to (not past) the first unbracketed, unquoted character in
    /// `terminators`; `None` if the input ends first or is malformed.
    fn value(&mut self, terminators: &[u8]) -> Option<Span> {
        let mut span = None;
        loop {
            let c = self.peek()?;
            if terminators.contains(&c) {
                return Some(span);
            }
            if !self.value_step(&mut span) {
                return None;
            }
        }
    }

    /// Scans a named struct key up to (not past) the next `:`. Unlike values, brackets have
    /// no meaning in keys.
    fn struct_key(&mut self) -> Option<Span> {
        let mut span = None;
        loop {
            let c = self.peek()?;
            if c == b':' {
                return Some(span);
            }
            let first = self.pos;
            let mut set_escaped = false;
            if self.escaped {
                self.mark(&mut span, first);
            } else if c == b'"' || c == b'\'' {
                if !self.skip_to_close_quote() {
                    return None;
                }
                self.mark(&mut span, first);
            } else if c == b'\\' {
                set_escaped = true;
                self.mark(&mut span, first);
            } else if !is_space(c) {
                self.mark(&mut span, first);
            }
            self.escaped = set_escaped;
            self.pos += 1;
        }
    }
}

/// Turns the raw text of an element into the text DuckDB stores in the VARCHAR child
/// (DuckDB's `HandleString`): outside nested brackets, quotes are removed and a backslash
/// escapes the next character when inside quotes or before a quote. With
/// `respect_scopes == false` (struct keys) brackets are ordinary characters.
fn unescape<'a>(raw: &'a [u8], respect_scopes: bool, scratch: &'a mut Vec<u8>) -> &'a [u8] {
    if !raw.iter().any(|&c| matches!(c, b'\\' | b'\'' | b'"')) {
        return raw;
    }
    scratch.clear();
    let mut escaped = false;
    let mut quote = None;
    let mut scopes = ScopeStack::new();
    for (i, &c) in raw.iter().enumerate() {
        if escaped {
            escaped = false;
            scratch.push(c);
            continue;
        }
        if scopes.is_empty() {
            if c == b'\\' && (quote.is_some() || matches!(raw.get(i + 1), Some(b'\'' | b'"'))) {
                escaped = true;
                continue;
            }
            if c == b'\'' || c == b'"' {
                match quote {
                    None => {
                        quote = Some(c);
                        continue;
                    }
                    Some(q) if q == c => {
                        quote = None;
                        continue;
                    }
                    Some(_) => {}
                }
            }
        }
        if quote.is_none() {
            if scopes.top() == Some(c) {
                scopes.pop();
            }
            if let (true, Some(close)) = (respect_scopes, closer(c)) {
                scopes.push(close);
            }
        }
        scratch.push(c);
    }
    scratch
}

/// The VARCHAR child value of an element: `None` for `NULL`.
fn element<'a>(raw: &'a [u8], scratch: &'a mut Vec<u8>) -> Option<&'a [u8]> {
    (!is_null(raw)).then(|| unescape(raw, true, scratch))
}

/// Walks a `[a, b, ...]` list, passing the raw text of each element (DuckDB's
/// `SplitStringListInternal`).
fn split_list_raw(input: &[u8], mut on_element: impl FnMut(&[u8])) -> bool {
    let mut cur = Cursor::new(input);
    cur.skip_whitespace();
    if !cur.eat(b'[') {
        return false;
    }
    cur.skip_whitespace();
    let mut seen_value = false;
    while cur.peek().is_some() {
        let Some(span) = cur.value(b",]") else {
            return false;
        };
        let at_end = cur.peek() == Some(b']');
        // `[]` is empty, but `[,]` has two empty elements.
        if !at_end || span.is_some() || seen_value {
            on_element(cur.raw(span));
            seen_value = true;
        }
        if at_end {
            break;
        }
        cur.pos += 1;
        cur.skip_whitespace();
    }
    cur.finish()
}

/// Splits the text form of a LIST (`[1, NULL, 'a,b', [2, 3]]`), calling `on_element` with
/// each element's VARCHAR child value (`None` for NULL). Returns `false` for malformed
/// input (DuckDB's `VectorStringToList::SplitStringList`).
pub(crate) fn split_list(
    input: &[u8],
    scratch: &mut Vec<u8>,
    mut on_element: impl FnMut(Option<&[u8]>),
) -> bool {
    split_list_raw(input, |raw| on_element(element(raw, scratch)))
}

/// Number of elements [`split_list`] reports for `input`; for malformed input, the number
/// reported before the failure (DuckDB's `VectorStringToList::CountPartsList`).
pub(crate) fn count_list_parts(input: &[u8]) -> usize {
    let mut count = 0;
    split_list_raw(input, |_| count += 1);
    count
}

/// One part of a MAP's text form, in input order (a key is always followed by its value).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MapPart<'a> {
    /// A key. `None` is a NULL key: DuckDB still reserves an entry for it (with a NULL
    /// key and value), and the split then fails.
    Key(Option<&'a [u8]>),
    /// The value of the preceding key; `None` is NULL.
    Value(Option<&'a [u8]>),
}

/// The raw text of a map key or value.
#[derive(Clone, Copy, Debug)]
enum RawMapPart<'a> {
    Key(&'a [u8]),
    Value(&'a [u8]),
}

/// Walks a `{k=v, ...}` map, passing the raw text of each key and value; `on_part`
/// returning `false` aborts the split (DuckDB's `SplitStringMapInternal`).
fn split_map_raw(input: &[u8], mut on_part: impl FnMut(RawMapPart<'_>) -> bool) -> bool {
    let mut cur = Cursor::new(input);
    cur.skip_whitespace();
    if !cur.eat(b'{') {
        return false;
    }
    cur.skip_whitespace();
    match cur.peek() {
        None => return false,
        Some(b'}') => return cur.finish(),
        Some(_) => {}
    }
    while cur.peek().is_some() {
        let Some(key) = cur.value(b"=") else { return false };
        if !on_part(RawMapPart::Key(cur.raw(key))) {
            return false;
        }
        cur.pos += 1;
        cur.skip_whitespace();
        let Some(value) = cur.value(b",}") else {
            return false;
        };
        on_part(RawMapPart::Value(cur.raw(value)));
        if cur.peek() == Some(b'}') {
            break;
        }
        cur.pos += 1;
        cur.skip_whitespace();
    }
    cur.finish()
}

/// Splits the text form of a MAP (`{1=a, 2=NULL}`), calling `on_part` with each key and
/// value as VARCHAR child values. Returns `false` for malformed input and after a NULL key
/// (DuckDB's `VectorStringToMap::SplitStringMap`).
pub(crate) fn split_map(input: &[u8], scratch: &mut Vec<u8>, mut on_part: impl FnMut(MapPart<'_>)) -> bool {
    split_map_raw(input, |part| match part {
        RawMapPart::Key(raw) => {
            let key = element(raw, scratch);
            on_part(MapPart::Key(key));
            key.is_some()
        }
        RawMapPart::Value(raw) => {
            on_part(MapPart::Value(element(raw, scratch)));
            true
        }
    })
}

/// Number of map entries (key/value pairs) to reserve for `input`; exact for well-formed
/// input (DuckDB's `(VectorStringToMap::CountPartsMap + 1) / 2`).
pub(crate) fn count_map_entries(input: &[u8]) -> usize {
    let mut parts = 0usize;
    split_map_raw(input, |_| {
        parts += 1;
        true
    });
    parts.div_ceil(2)
}

/// The fields of the STRUCT type a text value is cast to.
#[derive(Clone, Copy, Debug)]
pub(crate) enum StructFields<'a> {
    /// A struct with named fields: `{name: value}` keys are matched case-sensitively.
    Named(&'a [&'a [u8]]),
    /// An unnamed struct (`ROW(...)`) with this many fields: only `(v1, v2)` matches.
    Unnamed(usize),
}

impl StructFields<'_> {
    fn len(self) -> usize {
        match self {
            Self::Named(names) => names.len(),
            Self::Unnamed(count) => count,
        }
    }

    fn index_of(self, key: &[u8]) -> Option<usize> {
        match self {
            Self::Named(names) => names.iter().position(|name| *name == key),
            Self::Unnamed(_) => None,
        }
    }
}

/// Splits the text form of a STRUCT, calling `on_field(field_index, value)` with each
/// field's VARCHAR child value (`None` for NULL). Fields that are not mentioned are not
/// reported (they stay NULL); a repeated key is reported again (the last one wins).
/// Returns `false` for malformed input (DuckDB's `VectorStringToStruct::SplitStruct`).
///
/// Two forms are accepted for any struct type:
/// - named, `{a: 1, 'b': "x"}`: every key must name a field (case-sensitively); keys may
///   be quoted and must not be empty or an unquoted `NULL`;
/// - positional, `(1, 'x')`: values fill the fields in order; fewer values than fields is
///   fine, more is an error.
pub(crate) fn split_struct(
    input: &[u8],
    fields: StructFields<'_>,
    scratch: &mut Vec<u8>,
    mut on_field: impl FnMut(usize, Option<&[u8]>),
) -> bool {
    let mut cur = Cursor::new(input);
    cur.skip_whitespace();
    let close = match cur.peek() {
        Some(b'{') => b'}',
        Some(b'(') => b')',
        _ => return false,
    };
    cur.pos += 1;
    cur.skip_whitespace();
    match cur.peek() {
        None => return false,
        Some(c) if c == close => return cur.finish(),
        Some(_) => {}
    }
    if close == b'}' {
        while cur.peek().is_some() {
            let Some(Some(key_span)) = cur.struct_key() else {
                return false;
            };
            let raw_key = cur.raw(Some(key_span));
            if is_null(raw_key) {
                return false;
            }
            let Some(field) = fields.index_of(unescape(raw_key, false, scratch)) else {
                return false;
            };
            cur.pos += 1;
            cur.skip_whitespace();
            let Some(value) = cur.value(b",}") else {
                return false;
            };
            on_field(field, element(cur.raw(value), scratch));
            if cur.peek() == Some(b'}') {
                break;
            }
            cur.pos += 1;
            cur.skip_whitespace();
        }
    } else {
        let mut field = 0;
        while cur.peek().is_some() {
            if field == fields.len() {
                return false;
            }
            let Some(value) = cur.value(b",)") else {
                return false;
            };
            on_field(field, element(cur.raw(value), scratch));
            if cur.peek() == Some(b')') {
                break;
            }
            field += 1;
            cur.pos += 1;
            cur.skip_whitespace();
        }
    }
    cur.finish()
}

#[cfg(test)]
mod tests {
    //! Expected outputs were produced by DuckDB 1.5.6 through `TRY_CAST(text AS VARCHAR[])`,
    //! `STRUCT(...)` and `MAP(VARCHAR, VARCHAR)` casts (a NULL result means the split failed).

    use super::*;

    /// Input and its elements; `None` when the split fails.
    type ListCase = (&'static str, Option<&'static [Option<&'static str>]>);
    /// Input and the non-NULL `(field index, value)`s; `None` when the split fails.
    type StructCase = (&'static str, Option<&'static [(usize, &'static str)]>);
    /// Input and its `(key, value)` entries; `None` when the split fails.
    type MapCase = (
        &'static str,
        Option<&'static [(&'static str, Option<&'static str>)]>,
    );

    fn text(bytes: &[u8]) -> String {
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    fn list(input: &str) -> Option<Vec<Option<String>>> {
        let mut scratch = Vec::new();
        let mut elements = Vec::new();
        let ok = split_list(input.as_bytes(), &mut scratch, |e| elements.push(e.map(text)));
        ok.then_some(elements)
    }

    fn strukt(input: &str, fields: StructFields<'_>) -> Option<Vec<Option<String>>> {
        let mut scratch = Vec::new();
        let mut values = vec![None; fields.len()];
        let ok = split_struct(input.as_bytes(), fields, &mut scratch, |field, value| {
            values[field] = value.map(text);
        });
        ok.then_some(values)
    }

    fn map(input: &str) -> Option<Vec<(Option<String>, Option<String>)>> {
        let mut scratch = Vec::new();
        let mut entries = Vec::new();
        let ok = split_map(input.as_bytes(), &mut scratch, |part| match part {
            MapPart::Key(key) => entries.push((key.map(text), None)),
            MapPart::Value(value) => entries.last_mut().unwrap().1 = value.map(text),
        });
        ok.then_some(entries)
    }

    fn owned(values: &[Option<&str>]) -> Vec<Option<String>> {
        values.iter().map(|v| v.map(str::to_owned)).collect()
    }

    #[test]
    fn list_matches_duckdb() {
        for &(input, expected) in LIST_CASES {
            let expected = expected.map(owned);
            assert_eq!(list(input), expected, "{input:?}");
            if let Some(elements) = expected {
                assert_eq!(count_list_parts(input.as_bytes()), elements.len(), "{input:?}");
            }
        }
    }

    fn check_struct(fields: &[&str], cases: &[StructCase]) {
        for &(input, expected) in cases {
            let expected = expected.map(|set| {
                let mut values = vec![None; fields.len()];
                for &(field, value) in set {
                    values[field] = Some(value.to_owned());
                }
                values
            });
            let names: Vec<_> = fields.iter().map(|name| name.as_bytes()).collect();
            assert_eq!(strukt(input, StructFields::Named(&names)), expected, "{input:?}");
        }
    }

    #[test]
    fn struct_matches_duckdb() {
        check_struct(STRUCT_AB_FIELDS, STRUCT_AB_CASES);
        check_struct(STRUCT_WIDE_FIELDS, STRUCT_WIDE_CASES);
    }

    #[test]
    fn unnamed_struct_only_accepts_positional_form() {
        let unnamed = StructFields::Unnamed(2);
        assert_eq!(strukt("(1, 2)", unnamed), Some(owned(&[Some("1"), Some("2")])));
        assert_eq!(strukt("(1)", unnamed), Some(owned(&[Some("1"), None])));
        assert_eq!(strukt("{}", unnamed), Some(owned(&[None, None])));
        assert_eq!(strukt("{\"\": 1}", unnamed), None);
        assert_eq!(strukt("{a: 1}", unnamed), None);
        assert_eq!(strukt("(1, 2, 3)", unnamed), None);
    }

    #[test]
    fn map_matches_duckdb() {
        for &(input, expected) in MAP_CASES {
            let expected: Option<Vec<_>> = expected.map(|entries| {
                entries
                    .iter()
                    .map(|&(k, v)| (Some(k.to_owned()), v.map(str::to_owned)))
                    .collect()
            });
            assert_eq!(map(input), expected, "{input:?}");
            if let Some(entries) = expected {
                assert_eq!(count_map_entries(input.as_bytes()), entries.len(), "{input:?}");
            }
        }
    }

    #[test]
    fn null_map_key_is_reported_then_fails() {
        let mut parts = Vec::new();
        let ok = split_map(b"{a=1, null=2}", &mut Vec::new(), |part| {
            parts.push(match part {
                MapPart::Key(k) => ('k', k.map(text)),
                MapPart::Value(v) => ('v', v.map(text)),
            });
        });
        assert!(!ok);
        assert_eq!(
            parts,
            [('k', Some("a".into())), ('v', Some("1".into())), ('k', None)]
        );
        // DuckDB sizes the child vectors from both keys and values seen.
        assert_eq!(count_map_entries(b"{a=1, null=2}"), 2);
    }

    #[test]
    fn counts_on_malformed_input_stop_at_the_failure() {
        assert_eq!(count_list_parts(b"[1, 2"), 1);
        assert_eq!(count_list_parts(b"x"), 0);
        assert_eq!(count_map_entries(b"{a=1, b}"), 1);
    }

    #[test]
    fn plain_elements_borrow_the_input() {
        let input = b"[abc, 'q']";
        let mut scratch = Vec::new();
        let mut borrowed = Vec::new();
        assert!(split_list(input, &mut scratch, |e| {
            borrowed.push(e.is_some_and(|e| input.as_ptr_range().contains(&e.as_ptr())));
        }));
        assert_eq!(borrowed, [true, false]);
    }

    #[test]
    fn scope_stack_spills_past_inline_capacity() {
        let mut stack = ScopeStack::new();
        for i in 0..100u8 {
            stack.push(i);
            assert_eq!(stack.top(), Some(i));
        }
        for i in (0..100u8).rev() {
            assert_eq!(stack.top(), Some(i));
            stack.pop();
        }
        assert!(stack.is_empty());
        assert_eq!(stack.top(), None);
    }

    const LIST_CASES: &[ListCase] = &[
        (
            "[1, NULL, 'a,b', \"x\\\"y\", [2,3]]",
            Some(&[Some("1"), None, Some("a,b"), Some("x\"y"), Some("[2,3]")]),
        ),
        ("[]", Some(&[])),
        ("[ ]", Some(&[])),
        (" [ ] ", Some(&[])),
        ("[,]", Some(&[Some(""), Some("")])),
        ("[1,]", Some(&[Some("1"), Some("")])),
        ("[,1]", Some(&[Some(""), Some("1")])),
        ("[1,,2]", Some(&[Some("1"), Some(""), Some("2")])),
        (
            "[NULL, 'NULL', \"null\", nUlL,  null , null x, nullx]",
            Some(&[
                None,
                Some("NULL"),
                Some("null"),
                None,
                None,
                Some("null x"),
                Some("nullx"),
            ]),
        ),
        (
            "[a b, 'a' b, 'a'b'c', '', \"\", 'a''b']",
            Some(&[
                Some("a b"),
                Some("a b"),
                Some("abc"),
                Some(""),
                Some(""),
                Some("ab"),
            ]),
        ),
        ("[\\'a, a\\,b]", Some(&[Some("'a"), Some("a\\"), Some("b")])),
        ("[a\\]", Some(&[Some("a\\")])),
        ("[\\\\]", Some(&[Some("\\\\")])),
        (
            "['\\\\', 'a\\'b', \"a\\\\\", a\"b\"c, \"a\" \"b\", 'a\"b']",
            Some(&[
                Some("\\"),
                Some("a'b"),
                Some("a\\"),
                Some("abc"),
                Some("a b"),
                Some("a\"b"),
            ]),
        ),
        ("[[1, 2], [3]]", Some(&[Some("[1, 2]"), Some("[3]")])),
        (
            "[{'a': 1}, (1,2), [1, 'x]y'], [a, \"b\\\"c\"]]",
            Some(&[
                Some("{'a': 1}"),
                Some("(1,2)"),
                Some("[1, 'x]y]"),
                Some("[a, \"b\\\"c\"]"),
            ]),
        ),
        (
            "[[ 'a', \"b\" ], {a: [1, 2]}, x]",
            Some(&[Some("[ 'a', \"b\" ]"), Some("{a: [1, 2]}"), Some("x")]),
        ),
        ("[[]]", Some(&[Some("[]")])),
        ("[[1]", None),
        ("[1]]", None),
        ("[1] x", None),
        ("[1", None),
        ("1]", None),
        ("x[1]", None),
        ("['abc]", None),
        ("[\"a]", None),
        ("[(1]]", None),
        ("[[1)]", None),
        ("[{1]}]", Some(&[Some("{1]}")])),
        ("\t[ 1 ,\n 2 ]\r", Some(&[Some("1"), Some("2")])),
        ("[\\[1]", None),
        ("[ 'a' , 'b' ]", Some(&[Some("a"), Some("b")])),
        ("", None),
        ("   ", None),
        ("[", None),
        ("[ ", None),
        ("[a\\'b]", Some(&[Some("a'b")])),
        ("[\\\"a\"]", None),
        ("[a\\\\'b']", None),
        ("['a\\\\'b]", Some(&[Some("a\\b")])),
        ("['a'\\'b']", None),
        ("[\"\\\\\\\"]", None),
        ("[['a]']]", Some(&[Some("['a]]")])),
        ("[[\\'a]]", Some(&[Some("[\\'a]")])),
        ("[a[b]c]", Some(&[Some("a[b]c")])),
        ("[a(b]", None),
        ("[a{b,c}d]", Some(&[Some("a{b,c}d")])),
        ("[a'b,c'd]", Some(&[Some("ab,cd")])),
        ("[\\' a]", Some(&[Some("' a")])),
        ("[ \\x ]", Some(&[Some("\\x")])),
        ("[\\]", Some(&[Some("\\")])),
        ("[\\\\\\]", Some(&[Some("\\\\\\")])),
        ("[a\\ ]", Some(&[Some("a\\")])),
        ("[x\u{b}]", Some(&[Some("x")])),
        ("[\u{c}x]", Some(&[Some("x")])),
        ("[1,\u{a0}2]", Some(&[Some("1"), Some("\u{a0}2")])),
        ("[\\\\a]", Some(&[Some("\\\\a")])),
        ("['\\a']", Some(&[Some("a")])),
        ("['\\\\a']", Some(&[Some("\\a")])),
        ("[{a: '}'}]", Some(&[Some("{a: '}}")])),
        ("[[\\]]]", None),
        ("[[a\\']]", Some(&[Some("[a\\']")])),
        ("[NULL]", Some(&[None])),
        ("[null,]", Some(&[None, Some("")])),
        ("[,null]", Some(&[Some(""), None])),
        ("[ \"\" ]", Some(&[Some("")])),
        ("[(]", None),
        ("[)]", Some(&[Some(")")])),
        ("[}]", Some(&[Some("}")])),
        ("['a''b']", Some(&[Some("ab")])),
        ("[a, [b, c']]", None),
        ("[a, '[b', c]", Some(&[Some("a"), Some("[b"), Some("c")])),
        ("[a\\,\"b]", Some(&[Some("a\\"), Some("b")])),
        ("[a\\, \"b\"]", Some(&[Some("a\\"), Some("b")])),
        ("[a\\,'b']", None),
        (
            "[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[1]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]], 'q']",
            None,
        ),
        (
            "[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[['\\\"]x']]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]], y]",
            Some(&[
                Some("[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[['\\\"]x']]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]"),
                Some("y"),
            ]),
        ),
        ("[\u{e9}, '\u{fc}']", Some(&[Some("\u{e9}"), Some("\u{fc}")])),
    ];
    const STRUCT_AB_FIELDS: &[&str] = &["a", "b"];
    const STRUCT_AB_CASES: &[StructCase] = &[
        ("{a: 1, 'b': \"x\"}", Some(&[(0, "1"), (1, "x")])),
        ("{}", Some(&[])),
        (" { } ", Some(&[])),
        ("()", Some(&[])),
        ("(1, 2)", Some(&[(0, "1"), (1, "2")])),
        ("(1)", Some(&[(0, "1")])),
        ("(1, 2, 3)", None),
        ("(1,)", Some(&[(0, "1"), (1, "")])),
        ("(,)", Some(&[(0, ""), (1, "")])),
        ("{A: 1}", None),
        ("{a: 1, a: 2}", Some(&[(0, "2")])),
        ("{c: 1}", None),
        ("{b: 2}", Some(&[(1, "2")])),
        ("{a: NULL, b: 'NULL'}", Some(&[(1, "NULL")])),
        ("{null: 1}", None),
        ("{'null': 1}", None),
        ("{: 1}", None),
        ("{a 1}", None),
        ("{a: 1,}", None),
        ("{a: 1, b: 2} x", None),
        (
            "{a: [1, 2], b: {x: (1)}}",
            Some(&[(0, "[1, 2]"), (1, "{x: (1)}")]),
        ),
        ("{\"a\": \"x,y\", b: 'q''r'}", Some(&[(0, "x,y"), (1, "qr")])),
        ("{a:}", Some(&[(0, "")])),
        ("{a: , b: 2}", Some(&[(0, ""), (1, "2")])),
        ("{ a : 1 , b : 2 }", Some(&[(0, "1"), (1, "2")])),
        ("{a\\: 1}", None),
        ("{\\a: 1}", None),
        ("{'a' : 1}", Some(&[(0, "1")])),
        ("{ 'a' x: 1}", None),
        ("{a: 1)", None),
        ("(1, 2}", None),
        ("{a: (1, 2}", None),
        ("{[a]: 1}", None),
        ("{a: 1 2}", Some(&[(0, "1 2")])),
        ("{a: 1", None),
        ("{", None),
        ("(", None),
        ("", None),
        ("x", None),
        ("{a: '}', b: \"]\"}", Some(&[(0, "}"), (1, "]")])),
        ("( \"x\" , 'y' )", Some(&[(0, "x"), (1, "y")])),
        ("(NULL, null)", Some(&[])),
        ("{a\\'b: 1}", None),
        ("{\"a\\\"\": 1}", None),
        ("{a b: 1}", None),
        ("{\"a b\": 1}", None),
        ("{a: 1, b: 2, a: 3}", Some(&[(0, "3"), (1, "2")])),
        ("{b: 1, a: 2}", Some(&[(0, "2"), (1, "1")])),
        ("{a: {}, b: ()}", Some(&[(0, "{}"), (1, "()")])),
        ("{a: \\, b: 2}", Some(&[(0, "\\"), (1, "2")])),
        ("{ a: 1 }   ", Some(&[(0, "1")])),
        ("{a: 1}}", None),
        ("{a::1}", Some(&[(0, ":1")])),
        ("{a: x:y}", Some(&[(0, "x:y")])),
        ("{'a:b': 1}", None),
        ("{a: x\\, b: 'y'}", Some(&[(0, "x\\"), (1, "y")])),
        ("{a: x\\,'b': 1}", None),
        ("{'a\\'b': 1}", None),
        ("{\\'b: 1}", None),
        ("{a\\\\: 1}", None),
        ("{'a' 'b': 1}", None),
        ("(1, (2, 3))", Some(&[(0, "1"), (1, "(2, 3)")])),
        ("((1, 2), [3])", Some(&[(0, "(1, 2)"), (1, "[3]")])),
        ("(a\\,'b')", None),
    ];
    const STRUCT_WIDE_FIELDS: &[&str] = &["a", "b", "a b", "a'b", "a\"", "a:b", "[a]", "'b", "a\\", "a x"];
    const STRUCT_WIDE_CASES: &[StructCase] = &[
        ("{a: 1, 'b': \"x\"}", Some(&[(0, "1"), (1, "x")])),
        ("{}", Some(&[])),
        (" { } ", Some(&[])),
        ("()", Some(&[])),
        ("(1, 2)", Some(&[(0, "1"), (1, "2")])),
        ("(1)", Some(&[(0, "1")])),
        ("(1, 2, 3)", Some(&[(0, "1"), (1, "2"), (2, "3")])),
        ("(1,)", Some(&[(0, "1"), (1, "")])),
        ("(,)", Some(&[(0, ""), (1, "")])),
        ("{A: 1}", None),
        ("{a: 1, a: 2}", Some(&[(0, "2")])),
        ("{c: 1}", None),
        ("{b: 2}", Some(&[(1, "2")])),
        ("{a: NULL, b: 'NULL'}", Some(&[(1, "NULL")])),
        ("{null: 1}", None),
        ("{'null': 1}", None),
        ("{: 1}", None),
        ("{a 1}", None),
        ("{a: 1,}", None),
        ("{a: 1, b: 2} x", None),
        (
            "{a: [1, 2], b: {x: (1)}}",
            Some(&[(0, "[1, 2]"), (1, "{x: (1)}")]),
        ),
        ("{\"a\": \"x,y\", b: 'q''r'}", Some(&[(0, "x,y"), (1, "qr")])),
        ("{a:}", Some(&[(0, "")])),
        ("{a: , b: 2}", Some(&[(0, ""), (1, "2")])),
        ("{ a : 1 , b : 2 }", Some(&[(0, "1"), (1, "2")])),
        ("{a\\: 1}", Some(&[(8, "1")])),
        ("{\\a: 1}", None),
        ("{'a' : 1}", Some(&[(0, "1")])),
        ("{ 'a' x: 1}", Some(&[(9, "1")])),
        ("{a: 1)", None),
        ("(1, 2}", None),
        ("{a: (1, 2}", None),
        ("{[a]: 1}", Some(&[(6, "1")])),
        ("{a: 1 2}", Some(&[(0, "1 2")])),
        ("{a: 1", None),
        ("{", None),
        ("(", None),
        ("", None),
        ("x", None),
        ("{a: '}', b: \"]\"}", Some(&[(0, "}"), (1, "]")])),
        ("( \"x\" , 'y' )", Some(&[(0, "x"), (1, "y")])),
        ("(NULL, null)", Some(&[])),
        ("{a\\'b: 1}", Some(&[(3, "1")])),
        ("{\"a\\\"\": 1}", Some(&[(4, "1")])),
        ("{a b: 1}", Some(&[(2, "1")])),
        ("{\"a b\": 1}", Some(&[(2, "1")])),
        ("{a: 1, b: 2, a: 3}", Some(&[(0, "3"), (1, "2")])),
        ("{b: 1, a: 2}", Some(&[(0, "2"), (1, "1")])),
        ("{a: {}, b: ()}", Some(&[(0, "{}"), (1, "()")])),
        ("{a: \\, b: 2}", Some(&[(0, "\\"), (1, "2")])),
        ("{ a: 1 }   ", Some(&[(0, "1")])),
        ("{a: 1}}", None),
        ("{a::1}", Some(&[(0, ":1")])),
        ("{a: x:y}", Some(&[(0, "x:y")])),
        ("{'a:b': 1}", Some(&[(5, "1")])),
        ("{a: x\\, b: 'y'}", Some(&[(0, "x\\"), (1, "y")])),
        ("{a: x\\,'b': 1}", None),
        ("{'a\\'b': 1}", Some(&[(3, "1")])),
        ("{\\'b: 1}", Some(&[(7, "1")])),
        ("{a\\\\: 1}", None),
        ("{'a' 'b': 1}", Some(&[(2, "1")])),
        ("(1, (2, 3))", Some(&[(0, "1"), (1, "(2, 3)")])),
        ("((1, 2), [3])", Some(&[(0, "(1, 2)"), (1, "[3]")])),
        ("(a\\,'b')", None),
    ];
    const MAP_CASES: &[MapCase] = &[
        ("{1=a, 2=b}", Some(&[("1", Some("a")), ("2", Some("b"))])),
        ("{}", Some(&[])),
        (" { } ", Some(&[])),
        ("{NULL=a}", None),
        ("{a=1, null=2}", None),
        (
            "{a=NULL, b= null , 'c'='NULL', \"d\"=\"x,y\", e=[1, 2], f={g=h}, i=}",
            Some(&[
                ("a", None),
                ("b", None),
                ("c", Some("NULL")),
                ("d", Some("x,y")),
                ("e", Some("[1, 2]")),
                ("f", Some("{g=h}")),
                ("i", Some("")),
            ]),
        ),
        ("{=x}", Some(&[("", Some("x"))])),
        ("{a=1, b}", None),
        ("{a=1,}", None),
        ("{a=1} x", None),
        ("{a=1", None),
        ("{", None),
        (
            "{a=b=c, x\\=y=z}",
            Some(&[("a", Some("b=c")), ("x\\", Some("y=z"))]),
        ),
        (
            "{'a=b'=c, [x=1]=z, {p=q}=r}",
            Some(&[("a=b", Some("c")), ("[x=1]", Some("z")), ("{p=q}", Some("r"))]),
        ),
        (
            "{ a b = c d , \"x\\\"y\" = 'q\\'r' }",
            Some(&[("a b", Some("c d")), ("x\"y", Some("q'r"))]),
        ),
        ("[a=1]", None),
        ("{a=1)", None),
        ("{a=(1}", None),
        (
            "{nullx=1, \"null\"=2, nUlL1=3}",
            Some(&[("nullx", Some("1")), ("null", Some("2")), ("nUlL1", Some("3"))]),
        ),
        ("{,}", None),
        ("{a=1}}", None),
        ("{a=}", Some(&[("a", Some(""))])),
        ("", None),
        ("{a\\=\"b\"=c}", None),
        ("{a\\=\"b\"=c\"}", Some(&[("a\\", Some("b=c"))])),
        ("{(a=1)=(b=2)}", Some(&[("(a=1)", Some("(b=2)"))])),
        (
            "{a=1, b=2,c=3}",
            Some(&[("a", Some("1")), ("b", Some("2")), ("c", Some("3"))]),
        ),
        ("{x=}y}", None),
        ("{x={}}", Some(&[("x", Some("{}"))])),
    ];
}
