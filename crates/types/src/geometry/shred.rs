//! Shredding: GEOMETRY values of one geometry and vertex type to and from their vectorized
//! layout (DuckDB's "Shredding" section of `common/types/geometry.cpp`: `ToPoints`,
//! `FromPoints`, `ToLineStrings`, ... `ToVectorizedFormat`, `FromVectorizedFormat`).
//!
//! A point is a STRUCT of DOUBLE coordinates; a linestring or multipoint a LIST of them; a
//! polygon or multilinestring a LIST of LISTs; a multipolygon three LIST levels. The parts
//! of multi-geometries carry their own WKB header, which the layout drops.

use rustdb_common::{Error, Result};

use super::{BlobReader, GeometryType, VertexType, legacy, specialized_type, vectorized_type};
use crate::{GeometryStorageType, ListEntry, StringT, Vector};

/// The nesting of a shredded geometry type: whether the elements of each LIST level start
/// with their own WKB header (the parts of multi-geometries), and the type id of those
/// parts.
fn layout(geometry: GeometryType) -> Result<(&'static [bool], u32)> {
    Ok(match geometry {
        GeometryType::Point => (&[], 0),
        GeometryType::LineString => (&[false], 0),
        GeometryType::Polygon => (&[false, false], 0),
        GeometryType::MultiPoint => (&[true], GeometryType::Point as u32),
        GeometryType::MultiLineString => (&[true, false], GeometryType::LineString as u32),
        GeometryType::MultiPolygon => (&[true, false, false], GeometryType::Polygon as u32),
        GeometryType::GeometryCollection | GeometryType::Invalid => {
            return Err(Error::not_implemented(format!(
                "Unsupported geometry type {}",
                geometry as u32
            )));
        }
    })
}

fn skip_header(reader: &mut BlobReader<'_>) -> Result<()> {
    reader.skip(1 + 4)
}

/// Reads the vertices of one geometry into `coords` (one vector per dimension) and the
/// entries of its nested LIST levels into `entries[level]`; returns the entry of `level`.
fn parse_level(
    reader: &mut BlobReader<'_>,
    level: usize,
    headers: &[bool],
    entries: &mut [Vec<ListEntry>],
    coords: &mut [Vec<f64>],
) -> Result<ListEntry> {
    let count = reader.u32(true)? as usize;
    if level + 1 == headers.len() {
        let offset = coords[0].len();
        for _ in 0..count {
            if headers[level] {
                skip_header(reader)?;
            }
            for dim in coords.iter_mut() {
                dim.push(reader.f64(true)?);
            }
        }
        return Ok(ListEntry::new(offset, count));
    }
    let offset = entries[level + 1].len();
    // Reserve the slots of this level's children first: nested children are appended
    // after them.
    entries[level + 1].resize(offset + count, ListEntry::default());
    for index in 0..count {
        if headers[level] {
            skip_header(reader)?;
        }
        let child = parse_level(reader, level + 1, headers, entries, coords)?;
        entries[level + 1][offset + index] = child;
    }
    Ok(ListEntry::new(offset, count))
}

/// Writes the vertex coordinates into the fields of a vertex STRUCT vector.
fn write_vertices(target: &mut Vector, coords: &[Vec<f64>]) -> Result<()> {
    let count = coords.first().map_or(0, Vec::len);
    target.resize(count);
    let fields = target
        .struct_children_mut()
        .ok_or_else(|| Error::internal("shredded geometry vertices are not a STRUCT"))?;
    for (field, values) in fields.iter_mut().zip(coords) {
        field.values_mut::<f64>()[..values.len()].copy_from_slice(values);
    }
    Ok(())
}

/// Fills the child of LIST level `level` of `list` (the nested LIST levels and the
/// vertices, top down).
fn fill_levels(
    list: &mut Vector,
    level: usize,
    entries: &[Vec<ListEntry>],
    coords: &[Vec<f64>],
) -> Result<()> {
    let child = list
        .list_child_mut()
        .ok_or_else(|| Error::internal("shredded geometry level is not a LIST"))?;
    let size = if level + 1 == entries.len() {
        write_vertices(child, coords)?;
        coords.first().map_or(0, Vec::len)
    } else {
        let next = &entries[level + 1];
        child.resize(next.len());
        child.values_mut::<ListEntry>()[..next.len()].copy_from_slice(next);
        fill_levels(child, level + 1, entries, coords)?;
        next.len()
    };
    list.set_list_size(size);
    Ok(())
}

/// `Geometry::ToVectorizedFormat(source, target, count, geom_type, vert_type)`: the first
/// `count` rows of a GEOMETRY vector in the vectorized layout of `geometry`/`vertex`.
fn to_vectorized_typed(
    source: &Vector,
    count: usize,
    geometry: GeometryType,
    vertex: VertexType,
) -> Result<Vector> {
    let (headers, _) = layout(geometry)?;
    let ty = super::vectorized_type_of(geometry, vertex)?;
    let mut target = Vector::flat(ty, count);
    let width = vertex.width();
    let mut coords: Vec<Vec<f64>> = vec![Vec::new(); width];
    if headers.is_empty() {
        // `ToPoints`: one vertex per row, at the row.
        for dim in &mut coords {
            dim.resize(count, 0.0);
        }
        for row in 0..count {
            if source.is_null(row) {
                target.set_null(row);
                continue;
            }
            let mut reader = BlobReader::new(source.string_bytes(row));
            skip_header(&mut reader)?;
            for dim in &mut coords {
                dim[row] = reader.f64(true)?;
            }
        }
        write_vertices(&mut target, &coords)?;
        return Ok(target);
    }
    let mut entries: Vec<Vec<ListEntry>> = vec![Vec::new(); headers.len()];
    let mut top = vec![ListEntry::default(); count];
    for (row, entry) in top.iter_mut().enumerate() {
        if source.is_null(row) {
            *entry = ListEntry::new(entries.get(1).map_or(coords[0].len(), Vec::len), 0);
            target.set_null(row);
            continue;
        }
        let mut reader = BlobReader::new(source.string_bytes(row));
        skip_header(&mut reader)?;
        *entry = parse_level(&mut reader, 0, headers, &mut entries, &mut coords)?;
    }
    target.values_mut::<ListEntry>()[..count].copy_from_slice(&top);
    fill_levels(&mut target, 0, &entries, &coords)?;
    Ok(target)
}

/// `Geometry::ToVectorizedFormat(source, target, count, GeometryStorageType)`: the first
/// `count` rows of a GEOMETRY vector in the layout of storage type `storage` (legacy
/// SPATIAL bytes for `Spatial`).
pub fn to_vectorized(source: &Vector, count: usize, storage: GeometryStorageType) -> Result<Vector> {
    if storage == GeometryStorageType::Spatial {
        let mut target = Vector::flat(vectorized_type(storage)?, count);
        for row in 0..count {
            if source.is_null(row) {
                target.set_null(row);
                continue;
            }
            let bytes = legacy::to_spatial(source.string_bytes(row))?;
            let value = target.add_string(&bytes);
            target.values_mut::<StringT>()[row] = value;
        }
        return Ok(target);
    }
    let (geometry, vertex) = specialized_type(storage)
        .ok_or_else(|| Error::invalid_input(format!("Unsupported GeometryStorageType {}", storage as u8)))?;
    to_vectorized_typed(source, count, geometry, vertex)
}

/// The LIST entries and the vertex coordinates of a vectorized geometry vector, one LIST
/// level per entry slice.
struct VectorizedView<'a> {
    levels: Vec<&'a [ListEntry]>,
    coords: Vec<&'a [f64]>,
}

impl<'a> VectorizedView<'a> {
    fn new(source: &'a Vector, depth: usize) -> Result<Self> {
        let mut levels = Vec::with_capacity(depth);
        let mut current = source;
        for _ in 0..depth {
            levels.push(current.values::<ListEntry>());
            current = current
                .list_child()
                .ok_or_else(|| Error::internal("shredded geometry level is not a LIST"))?
                .0;
        }
        let coords = current
            .struct_children()
            .ok_or_else(|| Error::internal("shredded geometry vertices are not a STRUCT"))?
            .iter()
            .map(Vector::values::<f64>)
            .collect();
        Ok(Self { levels, coords })
    }

    fn write_vertex(&self, out: &mut Vec<u8>, index: usize) {
        for dim in &self.coords {
            out.extend_from_slice(&dim[index].to_le_bytes());
        }
    }

    /// Writes the count and the elements of LIST level `level` entry `index`.
    fn write_level(
        &self,
        out: &mut Vec<u8>,
        level: usize,
        entry: ListEntry,
        headers: &[bool],
        part_meta: u32,
    ) {
        out.extend_from_slice(&(entry.length as u32).to_le_bytes());
        for index in entry.range() {
            if headers[level] {
                out.push(1);
                out.extend_from_slice(&part_meta.to_le_bytes());
            }
            if level + 1 == self.levels.len() {
                self.write_vertex(out, index);
            } else {
                let child = self.levels[level + 1][index];
                self.write_level(out, level + 1, child, headers, part_meta);
            }
        }
    }
}

/// `Geometry::FromVectorizedFormat(source, target, count, geom_type, vert_type,
/// result_offset)`: rebuilds WKB from the vectorized layout into rows
/// `result_offset..result_offset + count` of `target`.
fn from_vectorized_typed(
    source: &Vector,
    target: &mut Vector,
    count: usize,
    geometry: GeometryType,
    vertex: VertexType,
    result_offset: usize,
) -> Result<()> {
    let (headers, part_type) = layout(geometry)?;
    let view = VectorizedView::new(source, headers.len())?;
    let meta = geometry as u32 + vertex.wkb_offset();
    let part_meta = part_type + vertex.wkb_offset();
    let sel = source.sel();
    let mut out = Vec::new();
    for row in 0..count {
        let target_row = result_offset + row;
        if source.is_null(row) {
            target.set_null(target_row);
            continue;
        }
        let slot = sel.slot(row);
        out.clear();
        out.push(1);
        out.extend_from_slice(&meta.to_le_bytes());
        if headers.is_empty() {
            view.write_vertex(&mut out, slot);
        } else {
            view.write_level(&mut out, 0, view.levels[0][slot], headers, part_meta);
        }
        let value = target.add_string(&out);
        target.values_mut::<StringT>()[target_row] = value;
    }
    Ok(())
}

/// `Geometry::FromVectorizedFormat(source, target, count, GeometryStorageType,
/// result_offset)`: rebuilds WKB from the layout of storage type `storage` (legacy
/// SPATIAL bytes for `Spatial`) into rows `result_offset..` of `target`.
pub fn from_vectorized(
    source: &Vector,
    target: &mut Vector,
    count: usize,
    storage: GeometryStorageType,
    result_offset: usize,
) -> Result<()> {
    if storage == GeometryStorageType::Spatial {
        for row in 0..count {
            let target_row = result_offset + row;
            if source.is_null(row) {
                target.set_null(target_row);
                continue;
            }
            let bytes = legacy::from_spatial(source.string_bytes(row))?;
            let value = target.add_string(&bytes);
            target.values_mut::<StringT>()[target_row] = value;
        }
        return Ok(());
    }
    let (geometry, vertex) = specialized_type(storage)
        .ok_or_else(|| Error::invalid_input(format!("Unsupported GeometryStorageType {}", storage as u8)))?;
    from_vectorized_typed(source, target, count, geometry, vertex, result_offset)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LogicalType;
    use crate::geometry::{from_text, get_type, storage_type, to_text};

    fn round_trip(texts: &[Option<&str>]) {
        let wkbs: Vec<Option<Vec<u8>>> = texts
            .iter()
            .map(|text| text.map(|text| from_text(text.as_bytes()).unwrap()))
            .collect();
        let first = wkbs.iter().flatten().next().unwrap();
        let (geometry, vertex) = get_type(first).unwrap();
        let storage = storage_type(geometry, vertex);
        let mut source = Vector::flat(LogicalType::GEOMETRY, texts.len());
        for (row, wkb) in wkbs.iter().enumerate() {
            match wkb {
                Some(wkb) => {
                    let value = source.add_string(wkb);
                    source.values_mut::<StringT>()[row] = value;
                }
                None => source.set_null(row),
            }
        }
        let shredded = to_vectorized(&source, texts.len(), storage).unwrap();
        let mut back = Vector::flat(LogicalType::GEOMETRY, texts.len() + 1);
        from_vectorized(&shredded, &mut back, texts.len(), storage, 1).unwrap();
        for (row, wkb) in wkbs.iter().enumerate() {
            match wkb {
                Some(wkb) => {
                    assert_eq!(back.string_bytes(row + 1), &wkb[..], "{:?}", texts[row]);
                    let mut text = Vec::new();
                    to_text(back.string_bytes(row + 1), &mut text).unwrap();
                }
                None => assert!(back.is_null(row + 1)),
            }
        }
    }

    #[test]
    fn shredding_round_trips() {
        round_trip(&[Some("POINT(1 2)"), None, Some("POINT(3 4)")]);
        round_trip(&[Some("POINT ZM (1 2 3 4)")]);
        round_trip(&[
            Some("LINESTRING(0 0, 1 1)"),
            None,
            Some("LINESTRING(2 2, 3 3, 4 4)"),
        ]);
        round_trip(&[
            Some("POLYGON((0 0, 1 0, 1 1, 0 0), (0 0, 1 1, 0 0))"),
            Some("POLYGON((5 5, 6 6, 5 5))"),
        ]);
        round_trip(&[Some("MULTIPOINT M (1 2 3, 4 5 6)"), None]);
        round_trip(&[
            Some("MULTILINESTRING((0 0, 1 1), (2 2, 3 3, 4 4))"),
            Some("MULTILINESTRING((9 9, 8 8))"),
        ]);
        round_trip(&[
            Some("MULTIPOLYGON(((0 0, 1 0, 1 1, 0 0)), ((5 5, 6 6, 5 5), (1 1, 2 2, 1 1)))"),
            None,
            Some("MULTIPOLYGON(((7 7, 8 8, 7 7)))"),
        ]);
    }
}
