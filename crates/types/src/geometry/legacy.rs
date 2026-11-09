//! The spatial extension's pre-v1.5 GEOMETRY serialization and its conversion to and from
//! WKB (DuckDB's "LEGACY GEOMETRY CONVERSION" in `common/types/geometry.cpp`).
//!
//! A legacy geometry is an 8-byte header (0-based root type, Z/M/bbox flags, padding), an
//! optional float bounding box, then one record per (sub)geometry: a 0-based type and a
//! size, followed by the vertices of points and linestrings, the ring sizes (padded to 8
//! bytes) and vertices of polygons, or nothing for collections.

use roastedduck_common::{Error, Result};

use super::{BlobReader, GeometryExtent, GeometryType};
use crate::{LogicalType, LogicalTypeId};

/// `Geometry::GetSpatialGeometryType`: the spatial extension's GEOMETRY, a BLOB alias.
pub fn spatial_type() -> LogicalType {
    LogicalType::BLOB.with_alias("GEOMETRY")
}

/// `Geometry::IsSpatialGeometryType`.
pub fn is_spatial_type(ty: &LogicalType) -> bool {
    ty.id() == LogicalTypeId::Blob && ty.alias().is_some_and(|alias| alias == "GEOMETRY")
}

fn unsupported_type() -> Error {
    Error::invalid_input("Unsupported geometry type in legacy geometry!")
}

/// `Geometry::FromSpatialGeometry` (`FromLegacyGeometryConversion`): legacy bytes to WKB.
pub fn from_spatial(source: &[u8]) -> Result<Vec<u8>> {
    let mut reader = BlobReader::new(source);
    reader.skip(1)?; // type
    let flags = reader.u8()?;
    reader.skip(2)?;
    reader.skip(4)?; // padding
    let has_z = flags & 0x01 != 0;
    let has_m = flags & 0x02 != 0;
    let has_bbox = flags & 0x04 != 0;
    let format_v1 = flags & 0x40 != 0;
    let format_v0 = flags & 0x80 != 0;
    if format_v1 || format_v0 {
        return Err(Error::invalid_input(
            "Cannot import an unknown version of the spatial geometry format!",
        ));
    }
    let dims = 2 + usize::from(has_z) + usize::from(has_m);
    if has_bbox {
        reader.skip(4 * 2 * dims)?;
    }
    let vert_width = dims * 8;
    let mut out = Vec::with_capacity(source.len() + 16);
    while !reader.at_end() {
        let type_id = reader.u32(true)?.wrapping_add(1);
        let size = reader.u32(true)?;
        let meta = type_id + u32::from(has_z) * 1000 + if has_m { 2000 } else { 0 };
        out.push(1);
        out.extend_from_slice(&meta.to_le_bytes());
        match GeometryType::from_id(type_id) {
            Some(GeometryType::Point) => {
                if size == 0 {
                    for _ in 0..dims {
                        out.extend_from_slice(&f64::NAN.to_le_bytes());
                    }
                } else {
                    out.extend_from_slice(reader.reserve(vert_width)?);
                }
            }
            Some(GeometryType::LineString) => {
                out.extend_from_slice(&size.to_le_bytes());
                out.extend_from_slice(reader.reserve(vert_width * size as usize)?);
            }
            Some(GeometryType::Polygon) => {
                out.extend_from_slice(&size.to_le_bytes());
                let mut ring_reader = BlobReader {
                    data: reader.data,
                    pos: reader.pos,
                };
                reader.skip(size as usize * 4 + (size as usize % 2) * 4)?;
                for _ in 0..size {
                    let ring_size = ring_reader.u32(true)?;
                    out.extend_from_slice(&ring_size.to_le_bytes());
                    out.extend_from_slice(reader.reserve(vert_width * ring_size as usize)?);
                }
            }
            Some(
                GeometryType::MultiPoint
                | GeometryType::MultiLineString
                | GeometryType::MultiPolygon
                | GeometryType::GeometryCollection,
            ) => out.extend_from_slice(&size.to_le_bytes()),
            _ => return Err(unsupported_type()),
        }
    }
    Ok(out)
}

/// `DoubleToFloatDown`: the largest float not above `d`.
fn double_to_float_down(d: f64) -> f32 {
    if d > f64::from(f32::MAX) {
        return f32::MAX;
    }
    if d <= f64::from(f32::MIN) {
        return f32::MIN;
    }
    let f = d as f32;
    if f64::from(f) <= d {
        return f;
    }
    f.next_down()
}

/// `DoubleToFloatUp`: the smallest float not below `d`.
fn double_to_float_up(d: f64) -> f32 {
    if d >= f64::from(f32::MAX) {
        return f32::MAX;
    }
    if d < f64::from(f32::MIN) {
        return f32::MIN;
    }
    let f = d as f32;
    if f64::from(f) >= d {
        return f;
    }
    f.next_up()
}

/// What `ToSpatialGeometryAnalyze` learns about the WKB (`ToSpatialGeometryState`).
struct SpatialAnalysis {
    extent: GeometryExtent,
    root_type: u32,
    root_has_z: bool,
    root_has_m: bool,
    root_bbox: bool,
}

/// `ToSpatialGeometryAnalyze` (and `ToSpatialGeometryAnalyzeInternal`).
fn analyze_for_spatial(source: &[u8]) -> Result<SpatialAnalysis> {
    let mut reader = BlobReader::new(source);
    let mut state = SpatialAnalysis {
        extent: GeometryExtent::empty(),
        root_type: 0,
        root_has_z: false,
        root_has_m: false,
        root_bbox: false,
    };
    while !reader.at_end() {
        if reader.u8()? != 1 {
            return Err(Error::invalid_input(
                "Only little-endian legacy geometries are supported for conversion to spatial format!",
            ));
        }
        let type_id = reader.u32(true)?;
        let flag = type_id / 1000;
        let geometry_type = type_id % 1000;
        let has_z = flag & 0x01 != 0;
        let has_m = flag & 0x02 != 0;
        if state.root_type == 0 {
            state.root_type = geometry_type;
            state.root_has_z = has_z;
            state.root_has_m = has_m;
        } else {
            if state.root_has_z != has_z {
                return Err(Error::invalid_input(
                    "All geometries in a spatial blob must have the same Z presence",
                ));
            }
            if state.root_has_m != has_m {
                return Err(Error::invalid_input(
                    "All geometries in a spatial blob must have the same M presence",
                ));
            }
        }
        let vertex =
            |reader: &mut BlobReader<'_>, extent: &mut GeometryExtent, skip_nan: bool| -> Result<()> {
                let x = reader.f64(true)?;
                let y = reader.f64(true)?;
                let z = if has_z { Some(reader.f64(true)?) } else { None };
                let m = if has_m { Some(reader.f64(true)?) } else { None };
                let all_nan =
                    x.is_nan() && y.is_nan() && z.is_none_or(f64::is_nan) && m.is_none_or(f64::is_nan);
                if !(skip_nan && all_nan) {
                    extent.extend(x, y, z, m);
                }
                Ok(())
            };
        match GeometryType::from_id(geometry_type) {
            Some(GeometryType::Point) => vertex(&mut reader, &mut state.extent, true)?,
            Some(GeometryType::LineString) => {
                for _ in 0..reader.u32(true)? {
                    vertex(&mut reader, &mut state.extent, false)?;
                }
            }
            Some(GeometryType::Polygon) => {
                for _ in 0..reader.u32(true)? {
                    for _ in 0..reader.u32(true)? {
                        vertex(&mut reader, &mut state.extent, false)?;
                    }
                }
            }
            Some(
                GeometryType::MultiPoint
                | GeometryType::MultiLineString
                | GeometryType::MultiPolygon
                | GeometryType::GeometryCollection,
            ) => reader.skip(4)?,
            _ => return Err(unsupported_type()),
        }
    }
    state.root_bbox = state.root_type != GeometryType::Point as u32;
    Ok(state)
}

/// `Geometry::ToSpatialGeometry` (`ToSpatialGeometryConvert`): WKB to legacy bytes.
pub fn to_spatial(source: &[u8]) -> Result<Vec<u8>> {
    let state = analyze_for_spatial(source)?;
    let mut flags = 0u8;
    if state.root_has_z {
        flags |= 0x01;
    }
    if state.root_has_m {
        flags |= 0x02;
    }
    if state.root_bbox {
        flags |= 0x04;
    }
    let mut out = Vec::with_capacity(source.len() + 40);
    // The legacy format is 0-based.
    out.push((state.root_type as u8).wrapping_sub(1));
    out.push(flags);
    out.extend_from_slice(&[0; 2]);
    out.extend_from_slice(&[0; 4]);
    if state.root_bbox {
        let extent = &state.extent;
        for value in [
            double_to_float_down(extent.x_min),
            double_to_float_down(extent.y_min),
            double_to_float_up(extent.x_max),
            double_to_float_up(extent.y_max),
        ] {
            out.extend_from_slice(&value.to_le_bytes());
        }
        if state.root_has_z {
            out.extend_from_slice(&double_to_float_down(extent.z_min).to_le_bytes());
            out.extend_from_slice(&double_to_float_up(extent.z_max).to_le_bytes());
        }
        if state.root_has_m {
            out.extend_from_slice(&double_to_float_down(extent.m_min).to_le_bytes());
            out.extend_from_slice(&double_to_float_up(extent.m_max).to_le_bytes());
        }
    }
    let dims = 2 + usize::from(state.root_has_z) + usize::from(state.root_has_m);
    let vert_width = dims * 8;
    let mut reader = BlobReader::new(source);
    while !reader.at_end() {
        if reader.u8()? != 1 {
            return Err(Error::invalid_input(
                "Only little-endian geometries are supported for conversion to spatial format!",
            ));
        }
        let geometry_type = reader.u32(true)? % 1000;
        out.extend_from_slice(&geometry_type.wrapping_sub(1).to_le_bytes());
        match GeometryType::from_id(geometry_type) {
            Some(GeometryType::Point) => {
                let vertex = reader.reserve(vert_width)?;
                let is_empty = vertex
                    .as_chunks::<8>()
                    .0
                    .iter()
                    .all(|bytes| f64::from_le_bytes(*bytes).is_nan());
                if is_empty {
                    out.extend_from_slice(&0u32.to_le_bytes());
                } else {
                    out.extend_from_slice(&1u32.to_le_bytes());
                    out.extend_from_slice(vertex);
                }
            }
            Some(GeometryType::LineString) => {
                let count = reader.u32(true)?;
                out.extend_from_slice(&count.to_le_bytes());
                out.extend_from_slice(reader.reserve(vert_width * count as usize)?);
            }
            Some(GeometryType::Polygon) => {
                let rings = reader.u32(true)?;
                out.extend_from_slice(&rings.to_le_bytes());
                let sizes_at = out.len();
                let padded = rings as usize + rings as usize % 2;
                out.resize(sizes_at + padded * 4, 0);
                for ring in 0..rings as usize {
                    let count = reader.u32(true)?;
                    out[sizes_at + ring * 4..sizes_at + ring * 4 + 4].copy_from_slice(&count.to_le_bytes());
                    out.extend_from_slice(reader.reserve(vert_width * count as usize)?);
                }
            }
            Some(
                GeometryType::MultiPoint
                | GeometryType::MultiLineString
                | GeometryType::MultiPolygon
                | GeometryType::GeometryCollection,
            ) => {
                let items = reader.u32(true)?;
                out.extend_from_slice(&items.to_le_bytes());
            }
            _ => return Err(unsupported_type()),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::{from_text, to_text};

    fn round_trip(text: &str) -> String {
        let wkb = from_text(text.as_bytes()).unwrap();
        let legacy = to_spatial(&wkb).unwrap();
        let back = from_spatial(&legacy).unwrap();
        assert_eq!(back, wkb, "{text}");
        let mut out = Vec::new();
        to_text(&back, &mut out).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn legacy_round_trips() {
        assert_eq!(round_trip("POINT(1 2)"), "POINT (1 2)");
        assert_eq!(
            round_trip("POLYGON Z ((1 2 3, 3 4 5, 5 6 7, 1 2 3))"),
            "POLYGON Z ((1 2 3, 3 4 5, 5 6 7, 1 2 3))"
        );
        assert_eq!(
            round_trip(
                "MULTIPOLYGON M (((1 2 4, 3 4 6, 5 6 8, 1 2 4)), ((7 8 10, 9 10 12, 11 12 14, 7 8 10)))"
            ),
            "MULTIPOLYGON M (((1 2 4, 3 4 6, 5 6 8, 1 2 4)), ((7 8 10, 9 10 12, 11 12 14, 7 8 10)))"
        );
        assert_eq!(
            round_trip("GEOMETRYCOLLECTION ZM (POINT ZM (1 2 3 4), LINESTRING ZM (3 4 5 6, 5 6 7 8))"),
            "GEOMETRYCOLLECTION ZM (POINT ZM (1 2 3 4), LINESTRING ZM (3 4 5 6, 5 6 7 8))"
        );
    }
}
