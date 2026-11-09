//! The GEOMETRY type's text and binary forms (DuckDB's `common/types/geometry.cpp`):
//! GEOMETRY values are little-endian WKB; casts parse and print WKT, and the `ST_*`
//! functions read WKB.

use roastedduck_common::{Error, Result};

use crate::{GeometryStorageType, LogicalType};

mod crs;
mod default_crs;
mod legacy;
mod shred;

pub use crs::{Crs, CrsType, try_convert, try_identify};
pub use default_crs::{DEFAULT_COORDINATE_SYSTEMS, DefaultCoordinateSystem};
pub use legacy::{from_spatial, is_spatial_type, spatial_type, to_spatial};
pub use shred::{from_vectorized, to_vectorized};

/// The deepest nesting of geometry collections (`Geometry::MAX_RECURSION_DEPTH`).
pub const MAX_RECURSION_DEPTH: u32 = 16;

/// `GeometryType`: the WKB type ids.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum GeometryType {
    Invalid = 0,
    Point = 1,
    LineString = 2,
    Polygon = 3,
    MultiPoint = 4,
    MultiLineString = 5,
    MultiPolygon = 6,
    GeometryCollection = 7,
}

impl GeometryType {
    /// The geometry type with WKB type id `id` (`static_cast<GeometryType>`), if any.
    pub const fn from_id(id: u32) -> Option<Self> {
        Some(match id {
            0 => Self::Invalid,
            1 => Self::Point,
            2 => Self::LineString,
            3 => Self::Polygon,
            4 => Self::MultiPoint,
            5 => Self::MultiLineString,
            6 => Self::MultiPolygon,
            7 => Self::GeometryCollection,
            _ => return None,
        })
    }
}

/// `VertexType`: which of Z and M the vertices carry besides X and Y.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum VertexType {
    Xy = 0,
    Xyz = 1,
    Xym = 2,
    Xyzm = 3,
}

impl VertexType {
    /// The vertex type of WKB flag `flag` (`static_cast<VertexType>`), if any.
    pub const fn from_flag(flag: u32) -> Option<Self> {
        Some(match flag {
            0 => Self::Xy,
            1 => Self::Xyz,
            2 => Self::Xym,
            3 => Self::Xyzm,
            _ => return None,
        })
    }

    pub const fn has_z(self) -> bool {
        matches!(self, Self::Xyz | Self::Xyzm)
    }

    pub const fn has_m(self) -> bool {
        matches!(self, Self::Xym | Self::Xyzm)
    }

    /// Coordinates per vertex (`V::WIDTH`).
    pub const fn width(self) -> usize {
        2 + self.has_z() as usize + self.has_m() as usize
    }

    /// The WKB type id offset of the flags (`(HAS_Z ? 1000 : 0) + (HAS_M ? 2000 : 0)`).
    pub const fn wkb_offset(self) -> u32 {
        (self as u32) * 1000
    }
}

/// `Geometry::GetSpecializedType`: the geometry and vertex type a shredded storage type
/// holds, or `None` for WKB and SPATIAL.
pub const fn specialized_type(storage: GeometryStorageType) -> Option<(GeometryType, VertexType)> {
    let raw = storage as u8;
    if raw < 16 {
        return None;
    }
    let vertex = match raw / 32 {
        0 => VertexType::Xy,
        1 => VertexType::Xyz,
        2 => VertexType::Xym,
        _ => VertexType::Xyzm,
    };
    let geometry = match raw % 16 {
        1 => GeometryType::Point,
        2 => GeometryType::LineString,
        3 => GeometryType::Polygon,
        4 => GeometryType::MultiPoint,
        5 => GeometryType::MultiLineString,
        6 => GeometryType::MultiPolygon,
        _ => return None,
    };
    Some((geometry, vertex))
}

/// `GetStorageType` (`geo_column_data.cpp`): the shredded storage type of a geometry and
/// vertex type; WKB for geometry collections and invalid types, which are not shredded.
pub fn storage_type(geometry: GeometryType, vertex: VertexType) -> GeometryStorageType {
    let base = match geometry {
        GeometryType::Point => 1,
        GeometryType::LineString => 2,
        GeometryType::Polygon => 3,
        GeometryType::MultiPoint => 4,
        GeometryType::MultiLineString => 5,
        GeometryType::MultiPolygon => 6,
        GeometryType::GeometryCollection | GeometryType::Invalid => return GeometryStorageType::Wkb,
    };
    let offset = match vertex {
        VertexType::Xy => 16,
        VertexType::Xyz => 32,
        VertexType::Xym => 64,
        VertexType::Xyzm => 96,
    };
    GeometryStorageType::from_repr(base + offset).unwrap_or(GeometryStorageType::Wkb)
}

/// The STRUCT of DOUBLE coordinates of a vertex type (`Geometry::GetVectorizedType`).
fn vertex_struct_type(vertex: VertexType) -> LogicalType {
    let mut fields = vec![
        ("x".into(), LogicalType::DOUBLE),
        ("y".into(), LogicalType::DOUBLE),
    ];
    if vertex.has_z() {
        fields.push(("z".into(), LogicalType::DOUBLE));
    }
    if vertex.has_m() {
        fields.push(("m".into(), LogicalType::DOUBLE));
    }
    LogicalType::struct_type(fields)
}

/// `Geometry::GetVectorizedType(GeometryType, VertexType)`: the layout a shredded column
/// stores (`GetVectorizedTypeInternal`).
pub fn vectorized_type_of(geometry: GeometryType, vertex: VertexType) -> Result<LogicalType> {
    let vertex = vertex_struct_type(vertex);
    Ok(match geometry {
        GeometryType::Point => vertex,
        GeometryType::LineString | GeometryType::MultiPoint => LogicalType::list(vertex),
        GeometryType::Polygon | GeometryType::MultiLineString => LogicalType::list(LogicalType::list(vertex)),
        GeometryType::MultiPolygon => LogicalType::list(LogicalType::list(LogicalType::list(vertex))),
        GeometryType::GeometryCollection => {
            return Err(Error::not_implemented(
                "GEOMETRYCOLLECTION vectorized type not implemented",
            ));
        }
        GeometryType::Invalid => {
            return Err(Error::invalid_input(format!(
                "Unsupported geometry type {}",
                geometry as u32
            )));
        }
    })
}

/// `Geometry::GetVectorizedType(GeometryStorageType)`: the type of the column a geometry
/// column of storage type `storage` stores.
pub fn vectorized_type(storage: GeometryStorageType) -> Result<LogicalType> {
    match storage {
        GeometryStorageType::Wkb => Ok(LogicalType::GEOMETRY),
        GeometryStorageType::Spatial => Ok(spatial_type()),
        _ => match specialized_type(storage) {
            Some((geometry, vertex)) => vectorized_type_of(geometry, vertex),
            None => Err(Error::invalid_input(format!(
                "Unsupported GeometryStorageType {}",
                storage as u8
            ))),
        },
    }
}

/// `Geometry::GetType`: the geometry and vertex type of little-endian WKB.
pub fn get_type(wkb: &[u8]) -> Result<(GeometryType, VertexType)> {
    let mut reader = BlobReader::new(wkb);
    let byte_order = reader.u8()?;
    if byte_order != 1 {
        return Err(Error::invalid_input(format!(
            "Unsupported byte order {byte_order} in WKB"
        )));
    }
    let meta = reader.u32(true)?;
    let type_id = (meta & 0xFFFF) % 1000;
    let flag = (meta & 0xFFFF) / 1000;
    if !(1..=7).contains(&type_id) {
        return Err(Error::invalid_input(format!(
            "Unsupported geometry type {type_id} in WKB"
        )));
    }
    if flag > 3 {
        return Err(Error::invalid_input(format!(
            "Unsupported geometry flag {flag} in WKB"
        )));
    }
    match (GeometryType::from_id(type_id), VertexType::from_flag(flag)) {
        (Some(geometry), Some(vertex)) => Ok((geometry, vertex)),
        _ => Err(Error::invalid_input("Unsupported geometry type in WKB")),
    }
}

/// Appends little-endian WKB (`BlobWriter`).
#[derive(Default)]
struct BlobWriter {
    buffer: Vec<u8>,
}

impl BlobWriter {
    fn u8(&mut self, value: u8) {
        self.buffer.push(value);
    }

    fn u32(&mut self, value: u32) {
        self.buffer.extend_from_slice(&value.to_le_bytes());
    }

    fn f64(&mut self, value: f64) {
        self.buffer.extend_from_slice(&value.to_le_bytes());
    }

    /// Reserves a count written later (`Reserve<uint32_t>`).
    fn reserve(&mut self) -> usize {
        let offset = self.buffer.len();
        self.buffer.extend_from_slice(&[0; 4]);
        offset
    }

    fn patch(&mut self, offset: usize, value: u32) {
        self.buffer[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
}

/// Reads WKB (`BlobReader`).
struct BlobReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> BlobReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn take<const N: usize>(&mut self) -> Result<[u8; N]> {
        let bytes = self.data.get(self.pos..self.pos + N).ok_or_else(|| {
            Error::invalid_input(format!("Unexpected end of binary data at position {}", self.pos))
        })?;
        self.pos += N;
        let mut out = [0; N];
        out.copy_from_slice(bytes);
        Ok(out)
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.take::<1>()?[0])
    }

    fn u32(&mut self, le: bool) -> Result<u32> {
        let bytes = self.take::<4>()?;
        Ok(if le {
            u32::from_le_bytes(bytes)
        } else {
            u32::from_be_bytes(bytes)
        })
    }

    fn f64(&mut self, le: bool) -> Result<f64> {
        let bytes = self.take::<8>()?;
        Ok(if le {
            f64::from_le_bytes(bytes)
        } else {
            f64::from_be_bytes(bytes)
        })
    }

    fn skip(&mut self, size: usize) -> Result<()> {
        if self.pos + size > self.data.len() {
            return Err(Error::invalid_input(format!(
                "Skipping beyond end of binary data at position {}",
                self.pos
            )));
        }
        self.pos += size;
        Ok(())
    }

    /// The next `size` bytes (`Reserve`).
    fn reserve(&mut self, size: usize) -> Result<&'a [u8]> {
        let bytes = self.data.get(self.pos..self.pos + size).ok_or_else(|| {
            Error::invalid_input(format!(
                "Reserving beyond end of binary data at position {}",
                self.pos
            ))
        })?;
        self.pos += size;
        Ok(bytes)
    }

    fn at_end(&self) -> bool {
        self.pos >= self.data.len()
    }
}

/// Reads WKT (`TextReader`), matching keywords case-insensitively and skipping trailing
/// whitespace after every token.
struct TextReader<'a> {
    text: &'a [u8],
    pos: usize,
}

impl TextReader<'_> {
    fn error(&self, message: impl std::fmt::Display) -> Error {
        Error::invalid_input(format!(
            "Failed to parse geometry: {message} at offset {}",
            self.pos
        ))
    }

    fn skip_whitespace(&mut self) {
        while self.pos < self.text.len() && is_c_space(self.text[self.pos]) {
            self.pos += 1;
        }
    }

    fn try_match(&mut self, word: &str) -> bool {
        let start = self.pos;
        for expected in word.bytes() {
            match self.text.get(self.pos) {
                Some(c) if c.eq_ignore_ascii_case(&expected) => self.pos += 1,
                _ => {
                    self.pos = start;
                    return false;
                }
            }
        }
        self.skip_whitespace();
        true
    }

    fn try_match_char(&mut self, c: u8) -> bool {
        if self
            .text
            .get(self.pos)
            .is_some_and(|got| got.eq_ignore_ascii_case(&c))
        {
            self.pos += 1;
            self.skip_whitespace();
            return true;
        }
        false
    }

    fn match_char(&mut self, c: u8) -> Result<()> {
        if self.try_match_char(c) {
            return Ok(());
        }
        match self.text.get(self.pos) {
            None => Err(self.error(format!("Expected '{}' but got end of input", char::from(c)))),
            Some(&got) => Err(self.error(format!(
                "Expected '{}' but got '{}'",
                char::from(c),
                char::from(got)
            ))),
        }
    }

    /// `MatchNumber`: a number prefix as `fast_float::from_chars` reads it (an optional
    /// `-`, digits with an optional fraction and exponent, or `inf`/`infinity`/`nan`).
    fn match_number(&mut self) -> Result<f64> {
        let rest = &self.text[self.pos..];
        let length = number_prefix(rest).ok_or_else(|| self.error("Expected number"))?;
        let text = std::str::from_utf8(&rest[..length]).map_err(|_| self.error("Expected number"))?;
        let value = parse_number(text).ok_or_else(|| self.error("Expected number"))?;
        self.pos += length;
        self.skip_whitespace();
        Ok(value)
    }
}

/// C's `isspace` in the C locale.
fn is_c_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\x0b' | b'\x0c' | b'\r')
}

/// The length of the number at the start of `text`, if any.
fn number_prefix(text: &[u8]) -> Option<usize> {
    let mut pos = usize::from(text.first() == Some(&b'-'));
    let rest = &text[pos..];
    for special in ["infinity", "inf", "nan"] {
        if rest.len() >= special.len() && rest[..special.len()].eq_ignore_ascii_case(special.as_bytes()) {
            return Some(pos + special.len());
        }
    }
    let digits = |text: &[u8], mut pos: usize| {
        let start = pos;
        while text.get(pos).is_some_and(u8::is_ascii_digit) {
            pos += 1;
        }
        (pos, pos - start)
    };
    let (after_integer, integer_digits) = digits(text, pos);
    pos = after_integer;
    let mut fraction_digits = 0;
    if text.get(pos) == Some(&b'.') {
        let (after_fraction, count) = digits(text, pos + 1);
        fraction_digits = count;
        pos = after_fraction;
    }
    if integer_digits + fraction_digits == 0 {
        return None;
    }
    if matches!(text.get(pos), Some(b'e' | b'E')) {
        let mut exponent = pos + 1;
        if matches!(text.get(exponent), Some(b'+' | b'-')) {
            exponent += 1;
        }
        let (after_exponent, count) = digits(text, exponent);
        if count > 0 {
            pos = after_exponent;
        }
    }
    Some(pos)
}

/// The value of a number accepted by [`number_prefix`].
fn parse_number(text: &str) -> Option<f64> {
    let (negative, magnitude) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text),
    };
    let value = if magnitude.eq_ignore_ascii_case("inf") || magnitude.eq_ignore_ascii_case("infinity") {
        f64::INFINITY
    } else if magnitude.eq_ignore_ascii_case("nan") {
        f64::NAN
    } else {
        magnitude.parse::<f64>().ok()?
    };
    Some(if negative { -value } else { value })
}

/// `FromStringRecursive`.
fn from_text_recursive(
    reader: &mut TextReader<'_>,
    writer: &mut BlobWriter,
    depth: u32,
    parent_z: bool,
    parent_m: bool,
) -> Result<()> {
    if depth == MAX_RECURSION_DEPTH {
        return Err(reader.error(format!(
            "Geometry string exceeds maximum recursion depth of {MAX_RECURSION_DEPTH}"
        )));
    }
    reader.skip_whitespace();
    // EWKT: the SRID is ignored.
    if reader.try_match("SRID") {
        reader.match_char(b'=')?;
        reader.match_number()?;
        reader.match_char(b';')?;
    }
    let ty = if reader.try_match("point") {
        GeometryType::Point
    } else if reader.try_match("linestring") {
        GeometryType::LineString
    } else if reader.try_match("polygon") {
        GeometryType::Polygon
    } else if reader.try_match("multipoint") {
        GeometryType::MultiPoint
    } else if reader.try_match("multilinestring") {
        GeometryType::MultiLineString
    } else if reader.try_match("multipolygon") {
        GeometryType::MultiPolygon
    } else if reader.try_match("geometrycollection") {
        GeometryType::GeometryCollection
    } else {
        return Err(reader.error("Unknown geometry type"));
    };
    let has_z = reader.try_match("z");
    let has_m = reader.try_match("m");
    let is_empty = reader.try_match("empty");
    if depth != 0 && (parent_z != has_z || parent_m != has_m) {
        return Err(reader.error("Geometry has inconsistent Z/M dimensions"));
    }
    let dims = 2 + u32::from(has_z) + u32::from(has_m);
    let meta = |ty: GeometryType| ty as u32 + if has_z { 1000 } else { 0 } + if has_m { 2000 } else { 0 };
    writer.u8(1);
    writer.u32(meta(ty));
    let vertex = |reader: &mut TextReader<'_>, writer: &mut BlobWriter| -> Result<()> {
        for _ in 0..dims {
            writer.f64(reader.match_number()?);
        }
        Ok(())
    };
    let vertices = |reader: &mut TextReader<'_>, writer: &mut BlobWriter| -> Result<()> {
        let count = writer.reserve();
        let mut vertices = 0;
        reader.match_char(b'(')?;
        loop {
            vertex(reader, writer)?;
            vertices += 1;
            if !reader.try_match_char(b',') {
                break;
            }
        }
        reader.match_char(b')')?;
        writer.patch(count, vertices);
        Ok(())
    };
    let rings = |reader: &mut TextReader<'_>, writer: &mut BlobWriter| -> Result<()> {
        let count = writer.reserve();
        let mut rings = 0;
        reader.match_char(b'(')?;
        loop {
            vertices(reader, writer)?;
            rings += 1;
            if !reader.try_match_char(b',') {
                break;
            }
        }
        reader.match_char(b')')?;
        writer.patch(count, rings);
        Ok(())
    };
    if is_empty {
        if ty == GeometryType::Point {
            for _ in 0..dims {
                writer.f64(f64::NAN);
            }
        } else {
            writer.u32(0);
        }
        return Ok(());
    }
    match ty {
        GeometryType::Point => {
            reader.match_char(b'(')?;
            vertex(reader, writer)?;
            reader.match_char(b')')?;
        }
        GeometryType::LineString => vertices(reader, writer)?,
        GeometryType::Polygon => rings(reader, writer)?,
        GeometryType::MultiPoint | GeometryType::MultiLineString | GeometryType::MultiPolygon => {
            let part_type = match ty {
                GeometryType::MultiPoint => GeometryType::Point,
                GeometryType::MultiLineString => GeometryType::LineString,
                _ => GeometryType::Polygon,
            };
            let count = writer.reserve();
            let mut parts = 0;
            reader.match_char(b'(')?;
            loop {
                writer.u8(1);
                writer.u32(meta(part_type));
                if part_type == GeometryType::Point {
                    let has_paren = reader.try_match_char(b'(');
                    if reader.try_match("EMPTY") {
                        for _ in 0..dims {
                            writer.f64(f64::NAN);
                        }
                    } else {
                        vertex(reader, writer)?;
                    }
                    if has_paren {
                        reader.match_char(b')')?;
                    }
                } else if reader.try_match("EMPTY") {
                    writer.u32(0);
                } else if part_type == GeometryType::LineString {
                    vertices(reader, writer)?;
                } else {
                    rings(reader, writer)?;
                }
                parts += 1;
                if !reader.try_match_char(b',') {
                    break;
                }
            }
            reader.match_char(b')')?;
            writer.patch(count, parts);
        }
        GeometryType::GeometryCollection => {
            let count = writer.reserve();
            let mut parts = 0;
            reader.match_char(b'(')?;
            loop {
                from_text_recursive(reader, writer, depth + 1, has_z, has_m)?;
                parts += 1;
                if !reader.try_match_char(b',') {
                    break;
                }
            }
            reader.match_char(b')')?;
            writer.patch(count, parts);
        }
        GeometryType::Invalid => return Err(reader.error("Unknown geometry type")),
    }
    Ok(())
}

/// `Geometry::FromString`: WKT (or EWKT) to WKB.
pub fn from_text(text: &[u8]) -> Result<Vec<u8>> {
    let mut reader = TextReader { text, pos: 0 };
    let mut writer = BlobWriter::default();
    from_text_recursive(&mut reader, &mut writer, 0, false, false)?;
    Ok(writer.buffer)
}

/// The WKB header of a geometry: its type, Z and M flags.
fn read_header(reader: &mut BlobReader<'_>) -> Result<(u32, bool, bool)> {
    let byte_order = reader.u8()?;
    if byte_order != 1 {
        return Err(Error::invalid_input(format!(
            "Unsupported byte order {byte_order} in WKB"
        )));
    }
    let meta = reader.u32(true)?;
    let type_id = (meta & 0xFFFF) % 1000;
    let flag = (meta & 0xFFFF) / 1000;
    Ok((type_id, flag & 1 != 0, flag & 2 != 0))
}

/// Appends a coordinate the way DuckDB's `TextWriter` does: fmt's `{}`, without a
/// trailing `.0` in fixed notation.
fn write_coordinate(out: &mut Vec<u8>, value: f64) {
    let start = out.len();
    crate::format::write_f64(value, out);
    let has_exponent = out[start..].iter().any(|&c| c == b'e' || c == b'E');
    if !has_exponent && out.last() == Some(&b'0') {
        out.pop();
        if out.last() == Some(&b'.') {
            out.pop();
        }
    }
}

/// `ToStringRecursive`.
#[allow(clippy::too_many_lines, reason = "one arm per geometry type, as in DuckDB")]
fn to_text_recursive(
    reader: &mut BlobReader<'_>,
    out: &mut Vec<u8>,
    depth: u32,
    parent_z: bool,
    parent_m: bool,
) -> Result<()> {
    if depth == MAX_RECURSION_DEPTH {
        return Err(Error::invalid_input(format!(
            "Geometry exceeds maximum recursion depth of {MAX_RECURSION_DEPTH}"
        )));
    }
    let (type_id, has_z, has_m) = read_header(reader)?;
    if depth != 0 && (parent_z != has_z || parent_m != has_m) {
        return Err(Error::invalid_input(format!(
            "Geometry has inconsistent Z/M dimensions, starting at position {}",
            reader.pos
        )));
    }
    let dims = 2 + usize::from(has_z) + usize::from(has_m);
    let flag = match (has_z, has_m) {
        (true, true) => " ZM ",
        (true, false) => " Z ",
        (false, true) => " M ",
        (false, false) => " ",
    };
    let write_vertices = |reader: &mut BlobReader<'_>, out: &mut Vec<u8>, count: u32| -> Result<()> {
        out.push(b'(');
        for vertex in 0..count {
            if vertex > 0 {
                out.extend_from_slice(b", ");
            }
            for d in 0..dims {
                if d > 0 {
                    out.push(b' ');
                }
                write_coordinate(out, reader.f64(true)?);
            }
        }
        out.push(b')');
        Ok(())
    };
    let write_rings = |reader: &mut BlobReader<'_>, out: &mut Vec<u8>, count: u32| -> Result<()> {
        out.push(b'(');
        for ring in 0..count {
            if ring > 0 {
                out.extend_from_slice(b", ");
            }
            let vertices = reader.u32(true)?;
            if vertices == 0 {
                out.extend_from_slice(b"EMPTY");
                continue;
            }
            write_vertices(reader, out, vertices)?;
        }
        out.push(b')');
        Ok(())
    };
    let read_point = |reader: &mut BlobReader<'_>| -> Result<(Vec<f64>, bool)> {
        let mut vertex = Vec::with_capacity(dims);
        for _ in 0..dims {
            vertex.push(reader.f64(true)?);
        }
        let all_nan = vertex.iter().all(|value| value.is_nan());
        Ok((vertex, all_nan))
    };
    let part_header =
        |reader: &mut BlobReader<'_>, expected: u32, kind: &str, collection: &str| -> Result<()> {
            let (part_type, part_z, part_m) = read_header(reader)?;
            if part_type != expected {
                return Err(Error::invalid_input(format!(
                    "Expected {kind} in {collection} but got {part_type}"
                )));
            }
            if part_z != has_z || part_m != has_m {
                return Err(Error::invalid_input(format!(
                    "Geometry has inconsistent Z/M dimensions in {collection}, starting at position {}",
                    reader.pos
                )));
            }
            Ok(())
        };
    match type_id {
        1 => {
            out.extend_from_slice(b"POINT");
            out.extend_from_slice(flag.as_bytes());
            let (vertex, all_nan) = read_point(reader)?;
            if all_nan {
                out.extend_from_slice(b"EMPTY");
                return Ok(());
            }
            out.push(b'(');
            for (d, value) in vertex.into_iter().enumerate() {
                if d > 0 {
                    out.push(b' ');
                }
                write_coordinate(out, value);
            }
            out.push(b')');
        }
        2 => {
            out.extend_from_slice(b"LINESTRING");
            out.extend_from_slice(flag.as_bytes());
            let count = reader.u32(true)?;
            if count == 0 {
                out.extend_from_slice(b"EMPTY");
                return Ok(());
            }
            write_vertices(reader, out, count)?;
        }
        3 => {
            out.extend_from_slice(b"POLYGON");
            out.extend_from_slice(flag.as_bytes());
            let count = reader.u32(true)?;
            if count == 0 {
                out.extend_from_slice(b"EMPTY");
                return Ok(());
            }
            write_rings(reader, out, count)?;
        }
        4 => {
            out.extend_from_slice(b"MULTIPOINT");
            out.extend_from_slice(flag.as_bytes());
            let count = reader.u32(true)?;
            if count == 0 {
                out.extend_from_slice(b"EMPTY");
                return Ok(());
            }
            out.push(b'(');
            for part in 0..count {
                part_header(reader, 1, "POINT", "MULTIPOINT")?;
                if part > 0 {
                    out.extend_from_slice(b", ");
                }
                let (vertex, all_nan) = read_point(reader)?;
                if all_nan {
                    out.extend_from_slice(b"EMPTY");
                    continue;
                }
                for (d, value) in vertex.into_iter().enumerate() {
                    if d > 0 {
                        out.push(b' ');
                    }
                    write_coordinate(out, value);
                }
            }
            out.push(b')');
        }
        5 => {
            out.extend_from_slice(b"MULTILINESTRING");
            out.extend_from_slice(flag.as_bytes());
            let count = reader.u32(true)?;
            if count == 0 {
                out.extend_from_slice(b"EMPTY");
                return Ok(());
            }
            out.push(b'(');
            for part in 0..count {
                part_header(reader, 2, "LINESTRING", "MULTILINESTRING")?;
                if part > 0 {
                    out.extend_from_slice(b", ");
                }
                let vertices = reader.u32(true)?;
                if vertices == 0 {
                    out.extend_from_slice(b"EMPTY");
                    continue;
                }
                write_vertices(reader, out, vertices)?;
            }
            out.push(b')');
        }
        6 => {
            out.extend_from_slice(b"MULTIPOLYGON");
            out.extend_from_slice(flag.as_bytes());
            let count = reader.u32(true)?;
            if count == 0 {
                out.extend_from_slice(b"EMPTY");
                return Ok(());
            }
            out.push(b'(');
            for part in 0..count {
                if part > 0 {
                    out.extend_from_slice(b", ");
                }
                part_header(reader, 3, "POLYGON", "MULTIPOLYGON")?;
                let rings = reader.u32(true)?;
                if rings == 0 {
                    out.extend_from_slice(b"EMPTY");
                    continue;
                }
                write_rings(reader, out, rings)?;
            }
            out.push(b')');
        }
        7 => {
            out.extend_from_slice(b"GEOMETRYCOLLECTION");
            out.extend_from_slice(flag.as_bytes());
            let count = reader.u32(true)?;
            if count == 0 {
                out.extend_from_slice(b"EMPTY");
                return Ok(());
            }
            out.push(b'(');
            for part in 0..count {
                if part > 0 {
                    out.extend_from_slice(b", ");
                }
                to_text_recursive(reader, out, depth + 1, has_z, has_m)?;
            }
            out.push(b')');
        }
        other => {
            return Err(Error::invalid_input(format!(
                "Unsupported geometry type {other} in WKB"
            )));
        }
    }
    Ok(())
}

/// `Geometry::ToString`: WKB to WKT.
pub fn to_text(wkb: &[u8], out: &mut Vec<u8>) -> Result<()> {
    to_text_recursive(&mut BlobReader::new(wkb), out, 0, false, false)
}

/// What `AnalyzeWKB` finds out about arbitrary WKB.
#[derive(Default)]
struct WkbAnalysis {
    big_endian: bool,
    extended: bool,
    unknown: bool,
}

/// The type id, Z and M flags of an (E)WKB header read in `le` order, skipping an SRID.
fn read_any_header(reader: &mut BlobReader<'_>, le: bool) -> Result<(u32, bool, bool, bool)> {
    let meta = reader.u32(le)?;
    let type_id = (meta & 0xFFFF) % 1000;
    let flag = (meta & 0xFFFF) / 1000;
    let ext_z = meta & 0x8000_0000 != 0;
    let ext_m = meta & 0x4000_0000 != 0;
    let srid = meta & 0x2000_0000 != 0;
    if srid {
        reader.skip(4)?;
    }
    Ok((
        type_id,
        flag & 1 != 0 || ext_z,
        flag & 2 != 0 || ext_m,
        ext_z || ext_m || srid,
    ))
}

/// `AnalyzeWKB`.
fn analyze(wkb: &[u8]) -> Result<WkbAnalysis> {
    let mut reader = BlobReader::new(wkb);
    let mut result = WkbAnalysis::default();
    while !reader.at_end() {
        let le = reader.u8()? == 1;
        let (type_id, has_z, has_m, extended) = read_any_header(&mut reader, le)?;
        result.extended |= extended;
        result.big_endian |= !le;
        let vertex_size = (2 + usize::from(has_z) + usize::from(has_m)) * 8;
        match type_id {
            1 => reader.skip(vertex_size)?,
            2 => {
                let count = reader.u32(le)? as usize;
                reader.skip(count * vertex_size)?;
            }
            3 => {
                for _ in 0..reader.u32(le)? {
                    let count = reader.u32(le)? as usize;
                    reader.skip(count * vertex_size)?;
                }
            }
            4..=7 => reader.skip(4)?,
            _ => {
                result.unknown = true;
                return Ok(result);
            }
        }
    }
    Ok(result)
}

/// `ConvertWKB`: any-endian (E)WKB to plain little-endian WKB.
fn convert(wkb: &[u8]) -> Result<Vec<u8>> {
    let mut reader = BlobReader::new(wkb);
    let mut writer = BlobWriter::default();
    while !reader.at_end() {
        let le = reader.u8()? == 1;
        let (type_id, has_z, has_m, _) = read_any_header(&mut reader, le)?;
        let width = 2 + usize::from(has_z) + usize::from(has_m);
        writer.u8(1);
        writer.u32(type_id + 1000 * u32::from(has_z) + 2000 * u32::from(has_m));
        let copy_vertices =
            |reader: &mut BlobReader<'_>, writer: &mut BlobWriter, count: usize| -> Result<()> {
                for _ in 0..count * width {
                    writer.f64(reader.f64(le)?);
                }
                Ok(())
            };
        match type_id {
            1 => copy_vertices(&mut reader, &mut writer, 1)?,
            2 => {
                let count = reader.u32(le)?;
                writer.u32(count);
                copy_vertices(&mut reader, &mut writer, count as usize)?;
            }
            3 => {
                let rings = reader.u32(le)?;
                writer.u32(rings);
                for _ in 0..rings {
                    let count = reader.u32(le)?;
                    writer.u32(count);
                    copy_vertices(&mut reader, &mut writer, count as usize)?;
                }
            }
            _ => {
                let parts = reader.u32(le)?;
                writer.u32(parts);
            }
        }
    }
    Ok(writer.buffer)
}

/// `Geometry::FromBinary`: validated little-endian WKB, or `None` for unknown geometry
/// types when not `strict`.
pub fn from_binary(wkb: &[u8], strict: bool) -> Result<Option<Vec<u8>>> {
    let analysis = analyze(wkb)?;
    if analysis.unknown {
        if strict {
            return Err(Error::invalid_input("Unsupported geometry type in WKB"));
        }
        return Ok(None);
    }
    if analysis.big_endian || analysis.extended {
        return convert(wkb).map(Some);
    }
    Ok(Some(wkb.to_vec()))
}

/// The bounding box of a geometry (`GeometryExtent`).
#[derive(Clone, Copy, Debug)]
pub struct GeometryExtent {
    pub x_min: f64,
    pub y_min: f64,
    pub z_min: f64,
    pub m_min: f64,
    pub x_max: f64,
    pub y_max: f64,
    pub z_max: f64,
    pub m_max: f64,
}

/// `MinValue`/`MaxValue`: the comparison picks the second operand on NaN.
fn min_value(a: f64, b: f64) -> f64 {
    if a < b { a } else { b }
}

fn max_value(a: f64, b: f64) -> f64 {
    if a > b { a } else { b }
}

impl GeometryExtent {
    /// `GeometryExtent::Unknown`: every axis spans everything.
    pub const fn unknown() -> Self {
        Self {
            x_min: f64::NEG_INFINITY,
            y_min: f64::NEG_INFINITY,
            z_min: f64::NEG_INFINITY,
            m_min: f64::NEG_INFINITY,
            x_max: f64::INFINITY,
            y_max: f64::INFINITY,
            z_max: f64::INFINITY,
            m_max: f64::INFINITY,
        }
    }

    /// `HasX`: the X range is neither empty nor unknown.
    pub fn has_x(&self) -> bool {
        self.x_min.is_finite() && self.x_max.is_finite()
    }

    /// `HasY`.
    pub fn has_y(&self) -> bool {
        self.y_min.is_finite() && self.y_max.is_finite()
    }

    /// `HasXY`.
    pub fn has_xy(&self) -> bool {
        self.has_x() && self.has_y()
    }

    /// `CanPruneXY`: one finite axis is enough to prune on.
    pub fn can_prune_xy(&self) -> bool {
        self.has_x() || self.has_y()
    }

    /// `HasZ`.
    pub fn has_z(&self) -> bool {
        self.z_min.is_finite() && self.z_max.is_finite()
    }

    /// `HasM`.
    pub fn has_m(&self) -> bool {
        self.m_min.is_finite() && self.m_max.is_finite()
    }

    /// `Merge`.
    pub fn merge(&mut self, other: &Self) {
        self.x_min = min_value(self.x_min, other.x_min);
        self.y_min = min_value(self.y_min, other.y_min);
        self.z_min = min_value(self.z_min, other.z_min);
        self.m_min = min_value(self.m_min, other.m_min);
        self.x_max = max_value(self.x_max, other.x_max);
        self.y_max = max_value(self.y_max, other.y_max);
        self.z_max = max_value(self.z_max, other.z_max);
        self.m_max = max_value(self.m_max, other.m_max);
    }

    /// `ContainsXY`.
    pub fn contains_xy(&self, other: &Self) -> bool {
        self.x_min <= other.x_min
            && self.x_max >= other.x_max
            && self.y_min <= other.y_min
            && self.y_max >= other.y_max
    }

    /// `GeometryExtent::Empty`.
    pub const fn empty() -> Self {
        Self {
            x_min: f64::INFINITY,
            y_min: f64::INFINITY,
            z_min: f64::INFINITY,
            m_min: f64::INFINITY,
            x_max: f64::NEG_INFINITY,
            y_max: f64::NEG_INFINITY,
            z_max: f64::NEG_INFINITY,
            m_max: f64::NEG_INFINITY,
        }
    }

    /// `Extend` with a vertex whose optional Z and M are given.
    pub fn extend(&mut self, x: f64, y: f64, z: Option<f64>, m: Option<f64>) {
        self.x_min = min_value(self.x_min, x);
        self.x_max = max_value(self.x_max, x);
        self.y_min = min_value(self.y_min, y);
        self.y_max = max_value(self.y_max, y);
        if let Some(z) = z {
            self.z_min = min_value(self.z_min, z);
            self.z_max = max_value(self.z_max, z);
        }
        if let Some(m) = m {
            self.m_min = min_value(self.m_min, m);
            self.m_max = max_value(self.m_max, m);
        }
    }

    /// `IntersectsXY`.
    pub fn intersects_xy(&self, other: &Self) -> bool {
        !(self.x_min > other.x_max
            || self.x_max < other.x_min
            || self.y_min > other.y_max
            || self.y_max < other.y_min)
    }
}

/// `ParseVertices`: extends `extent` with `count` vertices, skipping all-NaN ones when
/// `check_nan`; returns how many were used.
fn parse_vertices(
    reader: &mut BlobReader<'_>,
    extent: &mut GeometryExtent,
    count: u32,
    has_z: bool,
    has_m: bool,
    check_nan: bool,
) -> Result<u32> {
    let width = 2 + usize::from(has_z) + usize::from(has_m);
    if reader.pos + count as usize * width * 8 > reader.data.len() {
        return Err(Error::invalid_input(format!(
            "Reserving beyond end of binary data at position {}",
            reader.pos
        )));
    }
    let mut used = 0;
    for _ in 0..count {
        let x = reader.f64(true)?;
        let y = reader.f64(true)?;
        let z = if has_z { Some(reader.f64(true)?) } else { None };
        let m = if has_m { Some(reader.f64(true)?) } else { None };
        let all_nan = x.is_nan() && y.is_nan() && z.is_none_or(f64::is_nan) && m.is_none_or(f64::is_nan);
        if check_nan && all_nan {
            continue;
        }
        extent.extend(x, y, z, m);
        used += 1;
    }
    Ok(used)
}

/// `Geometry::GetExtent`: extends `extent` with every vertex and returns their count.
pub fn extent(wkb: &[u8], extent: &mut GeometryExtent) -> Result<u32> {
    let mut has_any_empty = false;
    extent_with_empty(wkb, extent, &mut has_any_empty)
}

/// `Geometry::GetExtent` with `has_any_empty`: also records whether any (part of the)
/// geometry is empty.
pub fn extent_with_empty(wkb: &[u8], extent: &mut GeometryExtent, has_any_empty: &mut bool) -> Result<u32> {
    let mut reader = BlobReader::new(wkb);
    let mut vertices = 0;
    while !reader.at_end() {
        let (type_id, has_z, has_m) = read_header(&mut reader)?;
        let flag = u32::from(has_z) + 2 * u32::from(has_m);
        if !(1..=7).contains(&type_id) {
            return Err(Error::invalid_input(format!(
                "Unsupported geometry type {type_id} in WKB"
            )));
        }
        if flag > 3 {
            return Err(Error::invalid_input(format!(
                "Unsupported geometry flag {flag} in WKB"
            )));
        }
        match type_id {
            1 => {
                let parsed = parse_vertices(&mut reader, extent, 1, has_z, has_m, true)?;
                if parsed == 0 {
                    *has_any_empty = true;
                }
                vertices += parsed;
            }
            2 => {
                let count = reader.u32(true)?;
                if count == 0 {
                    *has_any_empty = true;
                    continue;
                }
                vertices += parse_vertices(&mut reader, extent, count, has_z, has_m, false)?;
            }
            3 => {
                let rings = reader.u32(true)?;
                if rings == 0 {
                    *has_any_empty = true;
                    continue;
                }
                for _ in 0..rings {
                    let count = reader.u32(true)?;
                    if count == 0 {
                        *has_any_empty = true;
                        continue;
                    }
                    vertices += parse_vertices(&mut reader, extent, count, has_z, has_m, false)?;
                }
            }
            _ => {
                if reader.u32(true)? == 0 {
                    *has_any_empty = true;
                }
            }
        }
    }
    Ok(vertices)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(text: &str) -> String {
        let wkb = from_text(text.as_bytes()).unwrap();
        let mut out = Vec::new();
        to_text(&wkb, &mut out).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn wkt_round_trips() {
        assert_eq!(round_trip("POINT(0 1)"), "POINT (0 1)");
        assert_eq!(round_trip("point z (1.5 2 3)"), "POINT Z (1.5 2 3)");
        assert_eq!(round_trip("POINT EMPTY"), "POINT EMPTY");
        assert_eq!(round_trip("LINESTRING(0 0, 1 1)"), "LINESTRING (0 0, 1 1)");
        assert_eq!(round_trip("MULTIPOINT(1 1, (2 2))"), "MULTIPOINT (1 1, 2 2)");
        assert_eq!(
            round_trip("GEOMETRYCOLLECTION(POINT(1 2), POLYGON((0 0, 1 0, 0 0)))"),
            "GEOMETRYCOLLECTION (POINT (1 2), POLYGON ((0 0, 1 0, 0 0)))"
        );
        assert_eq!(round_trip("POINT(1e20 0)"), "POINT (1e+20 0)");
    }
}
