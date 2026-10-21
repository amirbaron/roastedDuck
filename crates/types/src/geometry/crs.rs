//! Coordinate reference systems of GEOMETRY types (DuckDB's `common/types/geometry_crs.cpp`
//! and the default `COORDINATE_SYSTEM_ENTRY` catalog entries): a definition is an
//! `AUTH:CODE`, a PROJJSON or WKT2:2019 document, or an opaque identifier.

use rustdb_common::{Error, Result};

use super::default_crs::{OGC_CRS83_PROJJSON, OGC_CRS83_WKT2_2019, OGC_CRS84_PROJJSON, OGC_CRS84_WKT2_2019};
use super::{number_prefix, parse_number};

/// `CoordinateReferenceSystemType`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CrsType {
    /// Not set or invalid.
    Invalid,
    /// An opaque identifier.
    Srid,
    /// `AUTH:CODE`.
    AuthCode,
    Projjson,
    Wkt2_2019,
}

/// A parsed coordinate reference system (DuckDB's `CoordinateReferenceSystem`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Crs {
    pub crs_type: CrsType,
    /// The (normalized) definition text.
    pub definition: String,
    /// The identifier found in the definition, if any.
    identifier: String,
}

/// C's `isspace` in the C locale.
fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\x0b' | b'\x0c' | b'\r')
}

impl Crs {
    fn invalid() -> Self {
        Self {
            crs_type: CrsType::Invalid,
            definition: String::new(),
            identifier: String::new(),
        }
    }

    /// `CoordinateReferenceSystem(definition)` (`ParseDefinition`): fails only on WKT
    /// that starts like WKT but is malformed.
    pub fn parse(definition: &str) -> Result<Self> {
        if definition.bytes().all(is_space) {
            return Ok(Self::invalid());
        }
        if let Some(result) = try_parse_projjson(definition) {
            return Ok(result);
        }
        if let Some(result) = try_parse_auth_code(definition) {
            return Ok(result);
        }
        if let Some(result) = try_parse_wkt2(definition)? {
            return Ok(result);
        }
        Ok(Self {
            crs_type: CrsType::Srid,
            definition: definition.to_owned(),
            identifier: String::new(),
        })
    }

    /// `CoordinateReferenceSystem::Deserialize`: an unparsable definition is invalid.
    pub fn parse_lenient(definition: &str) -> Self {
        Self::parse(definition).unwrap_or_else(|_| Self::invalid())
    }

    /// `GetIdentifier`: e.g. `EPSG:4326`; the definition when it has no identifier.
    pub fn identifier(&self) -> &str {
        if self.identifier.is_empty() {
            &self.definition
        } else {
            &self.identifier
        }
    }

    /// `IsComplete`: a full PROJJSON or WKT2 definition.
    pub fn is_complete(&self) -> bool {
        matches!(self.crs_type, CrsType::Projjson | CrsType::Wkt2_2019)
    }

    /// `Equals`: the same identifier, or the same kind and definition text.
    pub fn equals(&self, other: &Self) -> bool {
        if !self.identifier.is_empty() && self.identifier == other.identifier {
            return true;
        }
        self.crs_type == other.crs_type && self.definition == other.definition
    }
}

// ---------------------------------------------------------------------------------------
// WKT2:2019
// ---------------------------------------------------------------------------------------

/// A node of a WKT document (`WKTValue`).
enum WktValue {
    Keyword { name: String, children: Vec<WktValue> },
    Number(f64),
    String(String),
}

/// `WKTParser`.
struct WktParser<'a> {
    text: &'a [u8],
    pos: usize,
    depth: u32,
}

impl WktParser<'_> {
    fn skip_whitespace(&mut self) {
        while self.pos < self.text.len() && is_space(self.text[self.pos]) {
            self.pos += 1;
        }
    }

    fn try_match(&mut self, c: u8) -> bool {
        if self.text.get(self.pos) == Some(&c) {
            self.pos += 1;
            self.skip_whitespace();
            return true;
        }
        false
    }

    fn match_char(&mut self, c: u8) -> Result<()> {
        if self.try_match(c) {
            return Ok(());
        }
        match self.text.get(self.pos) {
            None => Err(Error::invalid_input(format!(
                "Expected '{}' but got end of input at position {}",
                char::from(c),
                self.pos
            ))),
            Some(&got) => Err(Error::invalid_input(format!(
                "Expected '{}' but got '{}' at position {}",
                char::from(c),
                char::from(got),
                self.pos
            ))),
        }
    }

    fn try_match_text(&mut self) -> Option<String> {
        let start = self.pos;
        if self
            .text
            .get(self.pos)
            .is_some_and(|c| c.is_ascii_alphabetic() || *c == b'_')
        {
            self.pos += 1;
            while self
                .text
                .get(self.pos)
                .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_')
            {
                self.pos += 1;
            }
        }
        if self.pos == start {
            return None;
        }
        let name = String::from_utf8_lossy(&self.text[start..self.pos]).into_owned();
        self.skip_whitespace();
        Some(name)
    }

    fn parse_string(&mut self) -> Result<Option<WktValue>> {
        if !self.try_match(b'"') {
            return Ok(None);
        }
        let mut start = self.pos;
        let mut result = Vec::new();
        while self.pos < self.text.len() {
            if self.text[self.pos] == b'"' {
                // A doubled quote is an escaped quote.
                if self.text.get(self.pos + 1) == Some(&b'"') {
                    result.extend_from_slice(&self.text[start..=self.pos]);
                    self.pos += 2;
                    start = self.pos;
                } else {
                    break;
                }
            } else {
                self.pos += 1;
            }
        }
        if self.pos == self.text.len() {
            return Err(Error::invalid_input(format!(
                "Unterminated string starting at position {start}"
            )));
        }
        result.extend_from_slice(&self.text[start..self.pos]);
        self.match_char(b'"')?;
        self.skip_whitespace();
        Ok(Some(WktValue::String(String::from_utf8_lossy(&result).into_owned())))
    }

    fn parse_children(&mut self, close: u8) -> Result<Vec<WktValue>> {
        let mut children = Vec::new();
        loop {
            let Some(child) = self.parse_node()? else {
                break;
            };
            children.push(child);
            if !self.try_match(b',') {
                break;
            }
        }
        self.match_char(close)?;
        Ok(children)
    }

    fn parse_keyword(&mut self) -> Result<Option<WktValue>> {
        let Some(name) = self.try_match_text() else {
            return Ok(None);
        };
        let children = if self.try_match(b'[') {
            self.parse_children(b']')?
        } else if self.try_match(b'(') {
            self.parse_children(b')')?
        } else {
            Vec::new()
        };
        Ok(Some(WktValue::Keyword { name, children }))
    }

    fn parse_number(&mut self) -> Option<WktValue> {
        let rest = &self.text[self.pos..];
        let length = number_prefix(rest)?;
        let value = parse_number(std::str::from_utf8(&rest[..length]).ok()?)?;
        self.pos += length;
        self.skip_whitespace();
        Some(WktValue::Number(value))
    }

    fn parse_node(&mut self) -> Result<Option<WktValue>> {
        // Guards against stack overflows on malicious input.
        self.depth += 1;
        if self.depth > 1001 {
            return Err(Error::invalid_input("WKT input is too deeply nested to parse"));
        }
        if let Some(node) = self.parse_string()? {
            return Ok(Some(node));
        }
        if let Some(node) = self.parse_keyword()? {
            return Ok(Some(node));
        }
        Ok(self.parse_number())
    }
}

/// `TryExtractIDFromWKTNode`: `AUTH:CODE` of the `ID` child of a keyword.
fn wkt_id(children: &[WktValue]) -> String {
    for child in children {
        let WktValue::Keyword { name, children } = child else {
            continue;
        };
        if !name.eq_ignore_ascii_case("ID") || children.len() < 2 {
            continue;
        }
        let WktValue::String(authority) = &children[0] else {
            continue;
        };
        match &children[1] {
            WktValue::Number(number) => {
                if *number < 0.0 || *number > u64::MAX as f64 {
                    continue;
                }
                return format!("{authority}:{}", *number as u64);
            }
            WktValue::String(code) => return format!("{authority}:{code}"),
            WktValue::Keyword { .. } => return String::new(),
        }
    }
    String::new()
}

/// `TryParseWKT2`.
fn try_parse_wkt2(text: &str) -> Result<Option<Crs>> {
    let mut parser = WktParser {
        text: text.as_bytes(),
        pos: 0,
        depth: 0,
    };
    parser.skip_whitespace();
    let Some(WktValue::Keyword { name, children }) = parser.parse_node()? else {
        return Ok(None);
    };
    const CRS_KEYWORDS: [&str; 14] = [
        "BOUNDCRS",
        "COMPOUNDCRS",
        "ENGCRS",
        "ENGINEERINGCRS",
        "GEODCRS",
        "GEODETICCRS",
        "GEOGCRS",
        "GEOGRAPHICCRS",
        "PARAMETRICCRS",
        "PROJCRS",
        "PROJECTEDCRS",
        "TIMECRS",
        "VERTCRS",
        "VERTICALCRS",
    ];
    if !CRS_KEYWORDS.iter().any(|keyword| keyword.eq_ignore_ascii_case(&name)) || children.is_empty() {
        return Ok(None);
    }
    let identifier = if name.eq_ignore_ascii_case("BOUNDCRS") {
        // The name of the transformation names a bound CRS.
        let Some(WktValue::Keyword {
            children: transform, ..
        }) = children.get(2)
        else {
            return Ok(None);
        };
        let Some(WktValue::String(transform_name)) = transform.first() else {
            return Ok(None);
        };
        let id = wkt_id(transform);
        if id.is_empty() { transform_name.clone() } else { id }
    } else {
        let WktValue::String(crs_name) = &children[0] else {
            return Ok(None);
        };
        let id = wkt_id(&children);
        if id.is_empty() { crs_name.clone() } else { id }
    };
    Ok(Some(Crs {
        crs_type: CrsType::Wkt2_2019,
        definition: text.trim_matches(|c: char| c.is_ascii() && is_space(c as u8)).to_owned(),
        identifier,
    }))
}

// ---------------------------------------------------------------------------------------
// PROJJSON
// ---------------------------------------------------------------------------------------

/// A JSON value as yyjson reads it: numbers are unsigned, signed or real.
enum Json {
    Null,
    Bool(bool),
    UInt(u64),
    SInt(i64),
    Real(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    fn get(&self, key: &str) -> Option<&Self> {
        match self {
            Self::Obj(members) => members.iter().find(|(name, _)| name == key).map(|(_, value)| value),
            _ => None,
        }
    }

    fn as_str(&self) -> Option<&str> {
        match self {
            Self::Str(text) => Some(text),
            _ => None,
        }
    }
}

/// A strict JSON reader (`yyjson_read` with `YYJSON_READ_NOFLAG`).
struct JsonReader<'a> {
    text: &'a [u8],
    pos: usize,
    depth: u32,
}

impl JsonReader<'_> {
    fn skip_whitespace(&mut self) {
        while matches!(self.text.get(self.pos), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn eat(&mut self, c: u8) -> bool {
        if self.text.get(self.pos) == Some(&c) {
            self.pos += 1;
            return true;
        }
        false
    }

    fn literal(&mut self, word: &[u8], value: Json) -> Option<Json> {
        if self.text[self.pos..].starts_with(word) {
            self.pos += word.len();
            return Some(value);
        }
        None
    }

    fn value(&mut self) -> Option<Json> {
        self.depth += 1;
        if self.depth > 4096 {
            return None;
        }
        self.skip_whitespace();
        let result = match *self.text.get(self.pos)? {
            b'{' => {
                self.pos += 1;
                let mut members = Vec::new();
                self.skip_whitespace();
                if !self.eat(b'}') {
                    loop {
                        self.skip_whitespace();
                        let key = self.string()?;
                        self.skip_whitespace();
                        if !self.eat(b':') {
                            return None;
                        }
                        let value = self.value()?;
                        members.push((key, value));
                        self.skip_whitespace();
                        if self.eat(b'}') {
                            break;
                        }
                        if !self.eat(b',') {
                            return None;
                        }
                    }
                }
                Json::Obj(members)
            }
            b'[' => {
                self.pos += 1;
                let mut elements = Vec::new();
                self.skip_whitespace();
                if !self.eat(b']') {
                    loop {
                        elements.push(self.value()?);
                        self.skip_whitespace();
                        if self.eat(b']') {
                            break;
                        }
                        if !self.eat(b',') {
                            return None;
                        }
                    }
                }
                Json::Arr(elements)
            }
            b'"' => Json::Str(self.string()?),
            b't' => self.literal(b"true", Json::Bool(true))?,
            b'f' => self.literal(b"false", Json::Bool(false))?,
            b'n' => self.literal(b"null", Json::Null)?,
            _ => self.number()?,
        };
        self.depth -= 1;
        Some(result)
    }

    fn hex4(&mut self) -> Option<u32> {
        let digits = self.text.get(self.pos..self.pos + 4)?;
        let text = std::str::from_utf8(digits).ok()?;
        let value = u32::from_str_radix(text, 16).ok()?;
        self.pos += 4;
        Some(value)
    }

    fn string(&mut self) -> Option<String> {
        if !self.eat(b'"') {
            return None;
        }
        let mut out = Vec::new();
        loop {
            let c = *self.text.get(self.pos)?;
            self.pos += 1;
            match c {
                b'"' => break,
                b'\\' => {
                    let escape = *self.text.get(self.pos)?;
                    self.pos += 1;
                    match escape {
                        b'"' => out.push(b'"'),
                        b'\\' => out.push(b'\\'),
                        b'/' => out.push(b'/'),
                        b'b' => out.push(0x08),
                        b'f' => out.push(0x0C),
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'u' => {
                            let mut code = self.hex4()?;
                            if (0xD800..0xDC00).contains(&code) {
                                if !(self.eat(b'\\') && self.eat(b'u')) {
                                    return None;
                                }
                                let low = self.hex4()?;
                                if !(0xDC00..0xE000).contains(&low) {
                                    return None;
                                }
                                code = 0x10000 + ((code - 0xD800) << 10) + (low - 0xDC00);
                            } else if (0xDC00..0xE000).contains(&code) {
                                return None;
                            }
                            let mut buffer = [0; 4];
                            out.extend_from_slice(char::from_u32(code)?.encode_utf8(&mut buffer).as_bytes());
                        }
                        _ => return None,
                    }
                }
                0..0x20 => return None,
                _ => out.push(c),
            }
        }
        String::from_utf8(out).ok()
    }

    fn number(&mut self) -> Option<Json> {
        let start = self.pos;
        let negative = self.eat(b'-');
        let digits_start = self.pos;
        while self.text.get(self.pos).is_some_and(u8::is_ascii_digit) {
            self.pos += 1;
        }
        let integer_digits = self.pos - digits_start;
        if integer_digits == 0 || (integer_digits > 1 && self.text[digits_start] == b'0') {
            return None;
        }
        let mut is_real = false;
        if self.eat(b'.') {
            is_real = true;
            let fraction_start = self.pos;
            while self.text.get(self.pos).is_some_and(u8::is_ascii_digit) {
                self.pos += 1;
            }
            if self.pos == fraction_start {
                return None;
            }
        }
        if matches!(self.text.get(self.pos), Some(b'e' | b'E')) {
            is_real = true;
            self.pos += 1;
            if matches!(self.text.get(self.pos), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            let exponent_start = self.pos;
            while self.text.get(self.pos).is_some_and(u8::is_ascii_digit) {
                self.pos += 1;
            }
            if self.pos == exponent_start {
                return None;
            }
        }
        let text = std::str::from_utf8(&self.text[start..self.pos]).ok()?;
        if !is_real {
            if negative {
                if let Ok(value) = text.parse::<i64>() {
                    return Some(Json::SInt(value));
                }
            } else if let Ok(value) = text.parse::<u64>() {
                return Some(Json::UInt(value));
            }
        }
        let value: f64 = text.parse().ok()?;
        value.is_finite().then_some(Json::Real(value))
    }
}

/// `yyjson_read` of a whole document.
fn read_json(text: &str) -> Option<Json> {
    let mut reader = JsonReader {
        text: text.as_bytes(),
        pos: 0,
        depth: 0,
    };
    let value = reader.value()?;
    reader.skip_whitespace();
    (reader.pos == reader.text.len()).then_some(value)
}

/// yyjson's `write_string` (no unicode or slash escaping).
fn write_json_string(out: &mut String, text: &str) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\u{c}' => out.push_str("\\f"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// yyjson's `write_f64_raw`: shortest round-trip digits in ECMAScript layout, keeping
/// `.0` on integral values.
fn write_json_real(out: &mut String, value: f64) {
    if value.is_sign_negative() {
        out.push('-');
    }
    let value = value.abs();
    if value == 0.0 {
        out.push_str("0.0");
        return;
    }
    // `{:e}` prints the shortest round-trip digits as `d.ddde[-]x`.
    let scientific = format!("{value:e}");
    let (mantissa, exponent) = scientific.split_once('e').unwrap_or((&scientific, "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let digits = digits.trim_end_matches('0');
    let digits = if digits.is_empty() { "0" } else { digits };
    let len = digits.len() as i32;
    let dot_pos = exponent + 1;
    if -6 < dot_pos && dot_pos <= 21 {
        if dot_pos <= 0 {
            out.push_str("0.");
            out.push_str(&"0".repeat(dot_pos.unsigned_abs() as usize));
            out.push_str(digits);
        } else if len <= dot_pos {
            out.push_str(digits);
            out.push_str(&"0".repeat((dot_pos - len) as usize));
            out.push_str(".0");
        } else {
            out.push_str(&digits[..dot_pos as usize]);
            out.push('.');
            out.push_str(&digits[dot_pos as usize..]);
        }
    } else {
        out.push_str(&digits[..1]);
        if len > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        out.push('e');
        out.push_str(&(dot_pos - 1).to_string());
    }
}

/// `yyjson_write` with `YYJSON_WRITE_NOFLAG`: minified JSON.
fn write_json(out: &mut String, value: &Json) {
    match value {
        Json::Null => out.push_str("null"),
        Json::Bool(value) => out.push_str(if *value { "true" } else { "false" }),
        Json::UInt(value) => out.push_str(&value.to_string()),
        Json::SInt(value) => out.push_str(&value.to_string()),
        Json::Real(value) => write_json_real(out, *value),
        Json::Str(text) => write_json_string(out, text),
        Json::Arr(elements) => {
            out.push('[');
            for (index, element) in elements.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_json(out, element);
            }
            out.push(']');
        }
        Json::Obj(members) => {
            out.push('{');
            for (index, (key, member)) in members.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_json_string(out, key);
                out.push(':');
                write_json(out, member);
            }
            out.push('}');
        }
    }
}

/// `TryParsePROJJSON`: the definition is normalized by writing the document back.
fn try_parse_projjson(text: &str) -> Option<Crs> {
    let root = read_json(text)?;
    let Json::Obj(_) = &root else {
        return None;
    };
    let crs_type = root.get("type")?.as_str()?;
    const PROJJSON_CRS_TYPES: [&str; 9] = [
        "GeographicCRS",
        "GeodeticCRS",
        "ProjectedCRS",
        "CompoundCRS",
        "BoundCRS",
        "VerticalCRS",
        "EngineeringCRS",
        "TemporalCRS",
        "ParametricCRS",
    ];
    if !PROJJSON_CRS_TYPES.iter().any(|name| name.eq_ignore_ascii_case(crs_type)) {
        return None;
    }
    // The transformation names a bound CRS.
    let target = if crs_type.eq_ignore_ascii_case("BoundCRS") {
        let transformation = root.get("transformation")?;
        if !matches!(transformation, Json::Obj(_)) {
            return None;
        }
        transformation
    } else {
        &root
    };
    let mut identifier = String::new();
    if let Some(name) = target.get("name").and_then(Json::as_str) {
        name.clone_into(&mut identifier);
    }
    if let Some(id) = target.get("id")
        && matches!(id, Json::Obj(_))
        && let Some(authority) = id.get("authority").and_then(Json::as_str)
    {
        authority.clone_into(&mut identifier);
        match id.get("code") {
            // `yyjson_is_int`: any integer; printed with `%d` of the `int` it holds.
            Some(Json::UInt(code)) => identifier.push_str(&format!(":{}", *code as i32)),
            Some(Json::SInt(code)) => identifier.push_str(&format!(":{}", *code as i32)),
            Some(Json::Str(code)) => {
                identifier.push(':');
                identifier.push_str(code);
            }
            _ => {}
        }
    }
    let mut definition = String::with_capacity(text.len());
    write_json(&mut definition, &root);
    Some(Crs {
        crs_type: CrsType::Projjson,
        definition,
        identifier,
    })
}

// ---------------------------------------------------------------------------------------
// AUTH:CODE
// ---------------------------------------------------------------------------------------

/// `TryParseAuthCode`: letters, a colon and alphanumerics, surrounding space trimmed.
fn try_parse_auth_code(text: &str) -> Option<Crs> {
    let trimmed = text.trim_matches(|c: char| c.is_ascii() && is_space(c as u8));
    let (authority, code) = trimmed.split_once(':')?;
    if !authority.bytes().all(|c| c.is_ascii_alphabetic()) || !code.bytes().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    Some(Crs {
        crs_type: CrsType::AuthCode,
        definition: trimmed.to_owned(),
        identifier: String::new(),
    })
}

// ---------------------------------------------------------------------------------------
// The coordinate system catalog
// ---------------------------------------------------------------------------------------

/// A `CoordinateSystemCatalogEntry`.
struct CoordinateSystemEntry {
    name: &'static str,
    authority: &'static str,
    code: &'static str,
    projjson: &'static str,
    wkt2_2019: &'static str,
}

/// `DEFAULT_CRS_DEFINITIONS` (`DefaultCoordinateSystemGenerator`).
const DEFAULT_COORDINATE_SYSTEMS: [CoordinateSystemEntry; 2] = [
    CoordinateSystemEntry {
        name: "OGC:CRS84",
        authority: "OGC",
        code: "CRS84",
        projjson: OGC_CRS84_PROJJSON,
        wkt2_2019: OGC_CRS84_WKT2_2019,
    },
    CoordinateSystemEntry {
        name: "OGC:CRS83",
        authority: "OGC",
        code: "CRS83",
        projjson: OGC_CRS83_PROJJSON,
        wkt2_2019: OGC_CRS83_WKT2_2019,
    },
];

/// `CoordinateReferenceSystem::TryConvert`: `source` in the `target` format, through the
/// coordinate system catalog.
pub fn try_convert(source: &Crs, target: CrsType) -> Result<Option<Crs>> {
    if source.crs_type == target && source.is_complete() {
        return Ok(Some(source.clone()));
    }
    let identifier = source.identifier();
    let Some(entry) = DEFAULT_COORDINATE_SYSTEMS
        .iter()
        .find(|entry| entry.name == identifier)
    else {
        return Ok(None);
    };
    let definition = match target {
        CrsType::AuthCode => {
            if entry.authority.is_empty() || entry.code.is_empty() {
                return Ok(None);
            }
            format!("{}:{}", entry.authority, entry.code)
        }
        CrsType::Srid => entry.name.to_owned(),
        CrsType::Projjson if !entry.projjson.is_empty() => entry.projjson.to_owned(),
        CrsType::Wkt2_2019 if !entry.wkt2_2019.is_empty() => entry.wkt2_2019.to_owned(),
        _ => return Ok(None),
    };
    Crs::parse(&definition).map(Some)
}

/// `CoordinateReferenceSystem::TryIdentify`: the CRS in its most compact known form
/// (`AUTH:CODE`, SRID, PROJJSON, WKT2), the definition itself when it is complete but
/// unknown, `None` when it is neither.
pub fn try_identify(definition: &str) -> Result<Option<Crs>> {
    let source = Crs::parse(definition)?;
    if source.crs_type == CrsType::Invalid {
        return Ok(None);
    }
    for target in [CrsType::AuthCode, CrsType::Srid, CrsType::Projjson, CrsType::Wkt2_2019] {
        if let Some(converted) = try_convert(&source, target)? {
            return Ok(Some(converted));
        }
    }
    Ok(source.is_complete().then_some(source))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifies_known_and_complete_definitions() {
        assert_eq!(try_identify("OGC:CRS84").unwrap().unwrap().definition, "OGC:CRS84");
        assert_eq!(
            try_identify(OGC_CRS84_WKT2_2019).unwrap().unwrap().definition,
            "OGC:CRS84"
        );
        assert_eq!(try_identify(OGC_CRS84_PROJJSON).unwrap().unwrap().definition, "OGC:CRS84");
        assert!(try_identify("EPSG:4326").unwrap().is_none());
        assert!(try_identify("foobar").unwrap().is_none());
        let error = try_identify(r#"GEOGCRS["WGS 84",foo[]"#).unwrap_err();
        assert!(error.to_string().contains("Expected ']' but got end of input at position 22"));
    }

    #[test]
    fn projjson_is_normalized() {
        let crs = Crs::parse(r#" { "type" : "GeographicCRS", "name": "x", "a": [1, -2, 2.50, 1e2, "°/"] } "#).unwrap();
        assert_eq!(crs.crs_type, CrsType::Projjson);
        assert_eq!(
            crs.definition,
            r#"{"type":"GeographicCRS","name":"x","a":[1,-2,2.5,100.0,"°/"]}"#
        );
        assert_eq!(crs.identifier(), "x");
    }
}
