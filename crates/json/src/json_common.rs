//! Shared JSON machinery (DuckDB's `json_common.hpp` / `json_common.cpp`): reading
//! documents with DuckDB's error messages, value type names, and JSON paths (`$.a[0]`)
//! and pointers (`/a/0`).

use std::borrow::Cow;

use roastedduck_common::{Error, Result};
use roastedduck_types::LogicalTypeId;

use crate::yyjson::{Doc, JsonArena, ReadError, ReadResult, ValId, ptr_get, tag, write_val};

/// `JSONCommon::TYPE_STRING_*`: the names `json_type` returns.
pub const TYPE_STRING_NULL: &str = "NULL";
pub const TYPE_STRING_BOOLEAN: &str = "BOOLEAN";
pub const TYPE_STRING_BIGINT: &str = "BIGINT";
pub const TYPE_STRING_UBIGINT: &str = "UBIGINT";
pub const TYPE_STRING_DOUBLE: &str = "DOUBLE";
pub const TYPE_STRING_VARCHAR: &str = "VARCHAR";
pub const TYPE_STRING_ARRAY: &str = "ARRAY";
pub const TYPE_STRING_OBJECT: &str = "OBJECT";

/// `JSONCommon::ValTypeToString`.
pub fn val_type_to_string(doc: &Doc<'_>, v: ValId) -> &'static str {
    match doc.tag(v) {
        tag::NULL => TYPE_STRING_NULL,
        tag::STR | tag::STR_NOESC => TYPE_STRING_VARCHAR,
        tag::ARR => TYPE_STRING_ARRAY,
        tag::OBJ => TYPE_STRING_OBJECT,
        tag::TRUE | tag::FALSE => TYPE_STRING_BOOLEAN,
        tag::UINT => TYPE_STRING_UBIGINT,
        tag::SINT => TYPE_STRING_BIGINT,
        _ => TYPE_STRING_DOUBLE,
    }
}

/// `JSONCommon::ValTypeToLogicalTypeId`.
pub fn val_type_to_logical_type_id(doc: &Doc<'_>, v: ValId) -> LogicalTypeId {
    match doc.tag(v) {
        tag::NULL => LogicalTypeId::SqlNull,
        tag::STR | tag::STR_NOESC => LogicalTypeId::Varchar,
        tag::ARR => LogicalTypeId::List,
        tag::OBJ => LogicalTypeId::Struct,
        tag::TRUE | tag::FALSE => LogicalTypeId::Boolean,
        tag::UINT => LogicalTypeId::UBigInt,
        tag::SINT => LogicalTypeId::BigInt,
        _ => LogicalTypeId::Double,
    }
}

/// `JSONCommon::FormatParseError`: `Malformed JSON at byte N of input: <msg>. <extra> Input: "<input>"`.
pub fn format_parse_error(data: &[u8], error: &ReadError, extra: &str) -> String {
    let input = if data.len() > 50 {
        let mut truncated = String::from_utf8_lossy(&data[..47]).into_owned();
        truncated.push_str("...");
        truncated
    } else {
        String::from_utf8_lossy(data).into_owned()
    };
    let input = input.replace('\r', "\\r");
    format!(
        "Malformed JSON at byte {} of input: {}. {} Input: \"{}\"",
        error.pos, error.msg, extra, input
    )
}

/// `JSONCommon::ReadDocument`: parses a copy of `data` into `arena`, raising DuckDB's
/// `Invalid Input Error` on malformed JSON.
pub fn read_document(arena: &mut JsonArena, data: &[u8]) -> Result<ReadResult> {
    arena
        .read(data, false)
        .map_err(|error| Error::invalid_input(format_parse_error(data, &error, "")))
}

/// `JSONCommon::ValToString`: the value as JSON, truncated to `max_len` bytes plus `...`.
pub fn val_to_string(doc: &Doc<'_>, v: ValId, max_len: Option<usize>) -> String {
    let mut out = Vec::new();
    write_val(&mut out, doc, v);
    match max_len {
        Some(max) if max < out.len() => {
            let mut text = String::from_utf8_lossy(&out[..max]).into_owned();
            text.push_str("...");
            text
        }
        _ => String::from_utf8_lossy(&out).into_owned(),
    }
}

/// Whether a path has wildcards (`JSONCommon::JSONPathType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JsonPathType {
    /// Extracts a single value.
    Regular,
    /// Extracts a list of values (`*` or `..` in the path).
    Wildcard,
}

/// `ThrowPathError`: the error near the character before `pos`.
fn path_error(path: &[u8], pos: usize, binder: bool) -> Error {
    let message = format!(
        "JSON path error near '{}'",
        String::from_utf8_lossy(&path[pos.saturating_sub(1)..])
    );
    if binder {
        Error::binder(message)
    } else {
        Error::invalid_input(message)
    }
}

/// `JSONKeyReadResult`.
#[derive(Debug)]
struct KeyReadResult<'a> {
    chars_read: usize,
    recursive: bool,
    /// Borrowed from the path unless it had escapes.
    key: Cow<'a, [u8]>,
}

impl KeyReadResult<'_> {
    fn empty() -> Self {
        Self {
            chars_read: 0,
            recursive: false,
            key: Cow::Borrowed(&[]),
        }
    }

    fn wildcard() -> Self {
        Self {
            chars_read: 1,
            recursive: false,
            key: Cow::Borrowed(b"*"),
        }
    }

    fn rec_wildcard() -> Self {
        Self {
            chars_read: 2,
            recursive: true,
            key: Cow::Borrowed(b"*"),
        }
    }

    fn rec_wildcard_shortcut() -> Self {
        Self {
            chars_read: 1,
            recursive: true,
            key: Cow::Borrowed(b"*"),
        }
    }

    fn is_valid(&self) -> bool {
        self.chars_read != 0
    }

    fn is_wildcard(&self) -> bool {
        *self.key == *b"*"
    }
}

/// `ReadString`: a key up to `.`/`[`, or a quoted key with `\"` / `\\` escapes.
fn read_path_string(path: &[u8], escaped: bool) -> KeyReadResult<'_> {
    if escaped {
        if let Some(close) = path.iter().position(|&c| c == b'"' || c == b'\\')
            && path[close] == b'"'
        {
            // No escapes: the key is the text up to the closing quote.
            return KeyReadResult {
                chars_read: close,
                recursive: false,
                key: Cow::Borrowed(&path[..close]),
            };
        }
        let mut key = Vec::with_capacity(path.len());
        let mut backslash = false;
        let mut i = 0;
        while i < path.len() {
            let c = path[i];
            if backslash {
                if c != b'"' && c != b'\\' {
                    key.push(b'\\');
                }
                backslash = false;
            } else if c == b'"' {
                break;
            } else if c == b'\\' {
                backslash = true;
                i += 1;
                continue;
            }
            key.push(c);
            i += 1;
        }
        if i == path.len() || backslash {
            return KeyReadResult::empty();
        }
        KeyReadResult {
            chars_read: i,
            recursive: false,
            key: Cow::Owned(key),
        }
    } else {
        let len = path
            .iter()
            .position(|&c| c == b'.' || c == b'[')
            .unwrap_or(path.len());
        KeyReadResult {
            chars_read: len,
            recursive: false,
            key: Cow::Borrowed(&path[..len]),
        }
    }
}

/// `ReadKey`: the key after a `.`.
fn read_key(path: &[u8]) -> KeyReadResult<'_> {
    let Some(&first) = path.first() else {
        return KeyReadResult::empty();
    };
    if first == b'*' {
        if path.get(1) == Some(&b'*') {
            return KeyReadResult::rec_wildcard();
        }
        return KeyReadResult::wildcard();
    }
    let mut rest = path;
    let mut recursive = false;
    if first == b'.' {
        match path.get(1) {
            Some(b'*') => return KeyReadResult::rec_wildcard(),
            Some(b'[') => return KeyReadResult::rec_wildcard_shortcut(),
            _ => {}
        }
        rest = &path[1..];
        recursive = true;
    }
    if rest.is_empty() {
        return KeyReadResult::empty();
    }
    let escaped = rest[0] == b'"';
    if escaped {
        rest = &rest[1..];
    }
    let mut result = read_path_string(rest, escaped);
    if !result.is_valid() {
        return result;
    }
    if escaped {
        result.chars_read += 2;
    }
    if recursive {
        result.chars_read += 1;
        result.recursive = true;
    }
    result
}

/// `ReadInteger`: digits up to `]` (at most 19); returns the digits read, 0 if invalid.
fn read_integer(path: &[u8], idx: &mut u64) -> usize {
    *idx = 0;
    let mut i = 0;
    while i < 19 {
        let Some(&c) = path.get(i) else {
            return 0;
        };
        if c == b']' {
            break;
        }
        let digit = c.wrapping_sub(b'0');
        if digit > 9 {
            return 0;
        }
        *idx = u64::from(digit) + *idx * 10;
        i += 1;
    }
    if *idx == u64::MAX { 0 } else { i }
}

/// An array index of a path: `[n]`, `[#-n]`/`[-n]` from the back, or `[*]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ArrayIndex {
    Wildcard,
    Index { index: u64, from_back: bool },
}

/// `ReadArrayIndex`: parses after `[`; returns the index and the bytes consumed
/// (including `]`), or on failure how far it got (DuckDB reports errors from there).
fn read_array_index(path: &[u8]) -> std::result::Result<(ArrayIndex, usize), usize> {
    let mut pos = 0;
    if path.first() == Some(&b'*') {
        pos += 1;
        if path.get(pos) != Some(&b']') {
            return Err(pos);
        }
        return Ok((ArrayIndex::Wildcard, pos + 1));
    }
    let mut from_back = false;
    if path.first() == Some(&b'#') {
        pos += 1;
        match path.get(pos) {
            Some(b']') => {
                // `[#]` is always NULL in SQLite: an index that is never present.
                return Ok((
                    ArrayIndex::Index {
                        index: u64::from(u32::MAX),
                        from_back: false,
                    },
                    pos + 1,
                ));
            }
            Some(b'-') => from_back = true,
            _ => return Err(pos),
        }
    }
    if path.get(pos) == Some(&b'-') {
        pos += 1;
        from_back = true;
    }
    let mut index = 0;
    let len = read_integer(&path[pos..], &mut index);
    if len == 0 {
        return Err(pos);
    }
    Ok((ArrayIndex::Index { index, from_back }, pos + len + 1))
}

/// `JSONCommon::ValidatePath`: checks a `$...` path and reports whether it has wildcards.
pub fn validate_path(path: &[u8], binder: bool) -> Result<JsonPathType> {
    let mut path_type = JsonPathType::Regular;
    let end = path.len();
    let mut pos = 1;
    while pos != end {
        let c = path[pos];
        pos += 1;
        if pos == end {
            return Err(path_error(path, pos, binder));
        }
        match c {
            b'.' => {
                let key = read_key(&path[pos..]);
                if !key.is_valid() {
                    return Err(path_error(path, pos, binder));
                }
                if key.is_wildcard() || key.recursive {
                    path_type = JsonPathType::Wildcard;
                }
                pos += key.chars_read;
            }
            b'[' => {
                let (index, consumed) = read_array_index(&path[pos..])
                    .map_err(|failed| path_error(path, pos + failed, binder))?;
                if index == ArrayIndex::Wildcard {
                    path_type = JsonPathType::Wildcard;
                }
                pos += consumed;
            }
            _ => return Err(path_error(path, pos, binder)),
        }
    }
    Ok(path_type)
}

/// The element `index` of an array, counting from the back if requested.
fn array_element(doc: &Doc<'_>, arr: ValId, index: u64, from_back: bool) -> Option<ValId> {
    let len = doc.len(arr) as u64;
    let index = if from_back && index != 0 {
        len.wrapping_sub(index)
    } else {
        index
    };
    usize::try_from(index)
        .ok()
        .and_then(|index| doc.arr_get(arr, index))
}

/// `JSONCommon::GetPath`: follows a validated `$` path without wildcards.
fn get_path(doc: &Doc<'_>, val: ValId, path: &[u8]) -> Option<ValId> {
    let end = path.len();
    let mut pos = 1;
    let mut val = val;
    while pos != end {
        let c = path[pos];
        pos += 1;
        match c {
            b'.' => {
                if !doc.is_obj(val) {
                    return None;
                }
                let key = read_key(&path[pos..]);
                pos += key.chars_read;
                val = doc.obj_getn(val, &key.key)?;
            }
            b'[' => {
                if !doc.is_arr(val) {
                    return None;
                }
                let (index, consumed) = read_array_index(&path[pos..]).ok()?;
                pos += consumed;
                let ArrayIndex::Index { index, from_back } = index else {
                    return None;
                };
                val = array_element(doc, val, index, from_back)?;
            }
            _ => return None,
        }
    }
    Some(val)
}

/// One step of a [`CompiledPath`].
#[derive(Debug)]
pub enum PathStep<'p> {
    Key(Cow<'p, [u8]>),
    Index { index: u64, from_back: bool },
}

/// A constant path parsed once per chunk instead of once per row; following it gives the
/// same value as [`get_unsafe`].
#[derive(Debug)]
pub enum CompiledPath<'p> {
    /// The value itself (an empty path).
    Root,
    /// A `$` path without wildcards.
    Steps(Vec<PathStep<'p>>),
    /// A JSON pointer (`/...`).
    Pointer(&'p [u8]),
    /// A path no value can match.
    Nothing,
}

impl<'p> CompiledPath<'p> {
    /// Parses `path` as [`get_unsafe`] reads it.
    pub fn new(path: &'p [u8]) -> Self {
        match path.first() {
            None => return Self::Root,
            Some(b'/') => return Self::Pointer(path),
            Some(b'$') => {}
            Some(_) => return Self::Nothing,
        }
        let mut steps = Vec::new();
        let mut pos = 1;
        while pos != path.len() {
            let c = path[pos];
            pos += 1;
            match c {
                b'.' => {
                    let key = read_key(&path[pos..]);
                    pos += key.chars_read;
                    steps.push(PathStep::Key(key.key));
                }
                b'[' => {
                    let Ok((ArrayIndex::Index { index, from_back }, consumed)) =
                        read_array_index(&path[pos..])
                    else {
                        return Self::Nothing;
                    };
                    pos += consumed;
                    steps.push(PathStep::Index { index, from_back });
                }
                _ => return Self::Nothing,
            }
        }
        Self::Steps(steps)
    }

    /// The value at the path from `val`, if any.
    #[inline]
    pub fn get(&self, doc: &Doc<'_>, val: ValId) -> Option<ValId> {
        match self {
            Self::Root => Some(val),
            Self::Steps(steps) => {
                let mut val = val;
                for step in steps {
                    val = match step {
                        PathStep::Key(key) => doc.obj_getn(val, key)?,
                        PathStep::Index { index, from_back } => {
                            if !doc.is_arr(val) {
                                return None;
                            }
                            array_element(doc, val, *index, *from_back)?
                        }
                    };
                }
                Some(val)
            }
            Self::Pointer(path) => ptr_get(doc, val, path),
            Self::Nothing => None,
        }
    }
}

/// `JSONCommon::GetUnsafe`: follows a validated pointer (`/...`) or path (`$...`).
pub fn get_unsafe(doc: &Doc<'_>, val: ValId, path: &[u8]) -> Option<ValId> {
    match path.first() {
        None => Some(val),
        Some(b'/') => ptr_get(doc, val, path),
        Some(b'$') => get_path(doc, val, path),
        Some(_) => None,
    }
}

/// `JSONCommon::Get`: follows a path given per row (validated here). Integral path
/// arguments index arrays.
pub fn get(doc: &Doc<'_>, val: ValId, path: &[u8], integral_argument: bool) -> Result<Option<ValId>> {
    if path.is_empty() {
        return Ok(Some(val));
    }
    if integral_argument {
        let mut full = b"$[".to_vec();
        full.extend_from_slice(path);
        full.push(b']');
        return Ok(get_path(doc, val, &full));
    }
    match path[0] {
        b'/' => Ok(ptr_get(doc, val, path)),
        b'$' => {
            if validate_path(path, false)? == JsonPathType::Wildcard {
                return Err(Error::invalid_input(
                    "JSON path cannot contain wildcards if the path is not a constant parameter",
                ));
            }
            Ok(get_path(doc, val, path))
        }
        _ => {
            if path.contains(&b'"') {
                let mut full = b"/".to_vec();
                full.extend_from_slice(path);
                Ok(ptr_get(doc, val, &full))
            } else {
                let mut full = b"$.\"".to_vec();
                full.extend_from_slice(path);
                full.push(b'"');
                Ok(get_path(doc, val, &full))
            }
        }
    }
}

/// `GetWildcardPathInternal`.
fn get_wildcard_path_internal(doc: &Doc<'_>, val: ValId, path: &[u8], vals: &mut Vec<ValId>) {
    let mut val = val;
    let mut pos = 0;
    let end = path.len();
    while pos != end {
        let c = path[pos];
        pos += 1;
        match c {
            b'.' => {
                let key = read_key(&path[pos..]);
                if key.recursive {
                    if key.is_wildcard() {
                        pos += key.chars_read;
                    }
                    let mut rec_vals = vec![val];
                    let mut i = 0;
                    while i < rec_vals.len() {
                        let rec_val = rec_vals[i];
                        if doc.is_arr(rec_val) {
                            rec_vals.extend(doc.arr_iter(rec_val));
                        } else if doc.is_obj(rec_val) {
                            rec_vals.extend(doc.obj_iter(rec_val).map(|(_, value)| value));
                        }
                        if i > 0 || pos != end {
                            get_wildcard_path_internal(doc, rec_val, &path[pos..], vals);
                        }
                        i += 1;
                    }
                    return;
                }
                pos += key.chars_read;
                if !doc.is_obj(val) {
                    return;
                }
                if key.is_wildcard() {
                    for (_, value) in doc.obj_iter(val) {
                        get_wildcard_path_internal(doc, value, &path[pos..], vals);
                    }
                    return;
                }
                match doc.obj_getn(val, &key.key) {
                    Some(found) => val = found,
                    None => return,
                }
            }
            b'[' => {
                if !doc.is_arr(val) {
                    return;
                }
                let Ok((index, consumed)) = read_array_index(&path[pos..]) else {
                    return;
                };
                pos += consumed;
                match index {
                    ArrayIndex::Wildcard => {
                        for element in doc.arr_iter(val) {
                            get_wildcard_path_internal(doc, element, &path[pos..], vals);
                        }
                        return;
                    }
                    ArrayIndex::Index { index, from_back } => match array_element(doc, val, index, from_back)
                    {
                        Some(found) => val = found,
                        None => return,
                    },
                }
            }
            _ => return,
        }
    }
    vals.push(val);
}

/// `JSONCommon::GetWildcardPath`: every value matched by a validated wildcard path.
pub fn get_wildcard_path(doc: &Doc<'_>, val: ValId, path: &[u8], vals: &mut Vec<ValId>) {
    if path.is_empty() {
        return;
    }
    get_wildcard_path_internal(doc, val, &path[1..], vals);
}
