//! Statistics of GEOMETRY columns (DuckDB's `GeometryStats`, `GeometryStatsData`,
//! `GeometryTypeSet` and `GeometryStatsFlags`): the bounding box of all vertices, the set
//! of geometry/vertex types present and whether (parts of) geometries are empty.

use std::fmt;

use rustdb_types::geometry::{self, GeometryExtent, GeometryType, VertexType};

/// `GeometryTypeSet`: one bit per geometry type (bit = WKB type id) per vertex type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GeometryTypeSet {
    pub sets: [u8; 4],
}

impl GeometryTypeSet {
    /// `GeometryTypeSet::Unknown`.
    pub const fn unknown() -> Self {
        Self { sets: [0xFF; 4] }
    }

    /// `GeometryTypeSet::Empty`.
    pub const fn empty() -> Self {
        Self { sets: [0; 4] }
    }

    /// `Add`.
    pub fn add(&mut self, geometry: GeometryType, vertex: VertexType) {
        self.sets[vertex as usize] |= 1 << (geometry as u32);
    }

    /// `AddWKBType`.
    pub fn add_wkb_type(&mut self, wkb_type: i32) {
        let vertex = ((wkb_type / 1000) % 10) as usize;
        let geometry = (wkb_type % 1000) as u32;
        if vertex < 4 && geometry < 8 {
            self.sets[vertex] |= 1 << geometry;
        }
    }

    pub fn merge(&mut self, other: &Self) {
        for (own, theirs) in self.sets.iter_mut().zip(other.sets) {
            *own |= theirs;
        }
    }

    pub fn clear(&mut self) {
        self.sets = [0; 4];
    }

    /// `TryGetSingleType`: the only (geometry, vertex) type present, ignoring the invalid
    /// type bit 0; `None` when none or several are.
    pub fn try_get_single_type(&self) -> Option<(GeometryType, VertexType)> {
        let mut found = None;
        for (vertex_index, &set) in self.sets.iter().enumerate() {
            for geometry_index in 1..8u32 {
                if set & (1 << geometry_index) != 0 {
                    if found.is_some() {
                        return None;
                    }
                    found = Some((geometry_index, vertex_index as u32));
                }
            }
        }
        let (geometry_index, vertex_index) = found?;
        Some((
            GeometryType::from_id(geometry_index)?,
            VertexType::from_flag(vertex_index)?,
        ))
    }

    /// `ToWKBList`.
    pub fn to_wkb_list(&self) -> Vec<i32> {
        let mut result = Vec::new();
        for (vertex_index, &set) in self.sets.iter().enumerate() {
            for geometry_index in 1..8 {
                if set & (1 << geometry_index) != 0 {
                    result.push(geometry_index + vertex_index as i32 * 1000);
                }
            }
        }
        result
    }

    /// `ToString(snake_case)`.
    pub fn names(&self, snake_case: bool) -> Vec<String> {
        let mut result = Vec::new();
        for (vertex_index, &set) in self.sets.iter().enumerate() {
            for geometry_index in 0..8 {
                if set & (1 << geometry_index) == 0 {
                    continue;
                }
                let (snake, camel) = match geometry_index {
                    1 => ("point", "Point"),
                    2 => ("linestring", "LineString"),
                    3 => ("polygon", "Polygon"),
                    4 => ("multipoint", "MultiPoint"),
                    5 => ("multilinestring", "MultiLineString"),
                    6 => ("multipolygon", "MultiPolygon"),
                    7 => ("geometrycollection", "GeometryCollection"),
                    _ => ("unknown", "Unknown"),
                };
                let mut name = if snake_case { snake } else { camel }.to_owned();
                name.push_str(match (vertex_index, snake_case) {
                    (1, true) => "_z",
                    (1, false) => " Z",
                    (2, true) => "_m",
                    (2, false) => " M",
                    (3, true) => "_zm",
                    (3, false) => " ZM",
                    _ => "",
                });
                result.push(name);
            }
        }
        result
    }
}

/// `GeometryStatsFlags`: whether empty geometries and geometries with empty parts occur.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GeometryStatsFlags {
    pub flags: u8,
}

impl GeometryStatsFlags {
    const HAS_EMPTY_GEOM: u8 = 0x1;
    const HAS_NON_EMPTY_GEOM: u8 = 0x2;
    const HAS_EMPTY_PART: u8 = 0x4;
    const HAS_NON_EMPTY_PART: u8 = 0x8;

    pub const fn unknown() -> Self {
        Self { flags: 0xF }
    }

    pub const fn empty() -> Self {
        Self { flags: 0 }
    }

    pub fn clear(&mut self) {
        self.flags = 0;
    }

    pub fn has_empty_geometry(&self) -> bool {
        self.flags & Self::HAS_EMPTY_GEOM != 0
    }

    pub fn has_non_empty_geometry(&self) -> bool {
        self.flags & Self::HAS_NON_EMPTY_GEOM != 0
    }

    pub fn has_empty_part(&self) -> bool {
        self.flags & Self::HAS_EMPTY_PART != 0
    }

    pub fn has_non_empty_part(&self) -> bool {
        self.flags & Self::HAS_NON_EMPTY_PART != 0
    }

    pub fn set_has_empty_geometry(&mut self) {
        self.flags |= Self::HAS_EMPTY_GEOM;
    }

    pub fn set_has_non_empty_geometry(&mut self) {
        self.flags |= Self::HAS_NON_EMPTY_GEOM;
    }

    pub fn set_has_empty_part(&mut self) {
        self.flags |= Self::HAS_EMPTY_PART;
    }

    pub fn set_has_non_empty_part(&mut self) {
        self.flags |= Self::HAS_NON_EMPTY_PART;
    }
}

/// `GeometryStatsData`.
#[derive(Clone, Copy, Debug)]
pub struct GeometryStats {
    pub types: GeometryTypeSet,
    pub extent: GeometryExtent,
    pub flags: GeometryStatsFlags,
}

impl PartialEq for GeometryStats {
    fn eq(&self, other: &Self) -> bool {
        let extent = |e: &GeometryExtent| {
            [
                e.x_min, e.y_min, e.z_min, e.m_min, e.x_max, e.y_max, e.z_max, e.m_max,
            ]
            .map(f64::to_bits)
        };
        self.types == other.types
            && self.flags == other.flags
            && extent(&self.extent) == extent(&other.extent)
    }
}

impl GeometryStats {
    /// `SetEmpty`.
    pub const fn empty() -> Self {
        Self {
            types: GeometryTypeSet::empty(),
            extent: GeometryExtent::empty(),
            flags: GeometryStatsFlags::empty(),
        }
    }

    /// `SetUnknown`.
    pub const fn unknown() -> Self {
        Self {
            types: GeometryTypeSet::unknown(),
            extent: GeometryExtent::unknown(),
            flags: GeometryStatsFlags::unknown(),
        }
    }

    pub fn merge(&mut self, other: &Self) {
        self.types.merge(&other.types);
        self.extent.merge(&other.extent);
        self.flags.flags |= other.flags.flags;
    }

    /// `GeometryStatsData::Update`: accounts for one WKB value. Invalid WKB (which a
    /// GEOMETRY column never holds) leaves the statistics unchanged.
    #[inline]
    pub fn update(&mut self, wkb: &[u8]) {
        let Ok((geometry, vertex)) = geometry::get_type(wkb) else {
            return;
        };
        self.types.add(geometry, vertex);
        let mut has_any_empty = false;
        let Ok(vertices) = geometry::extent_with_empty(wkb, &mut self.extent, &mut has_any_empty) else {
            return;
        };
        if has_any_empty {
            self.flags.set_has_empty_part();
        } else {
            self.flags.set_has_non_empty_part();
        }
        if vertices == 0 {
            self.flags.set_has_empty_geometry();
        } else {
            self.flags.set_has_non_empty_geometry();
        }
    }
}

impl fmt::Display for GeometryStats {
    /// `GeometryStats::ToString` (without the base statistics).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let e = &self.extent;
        write!(
            f,
            "[Extent: [X: [{}, {}], Y: [{}, {}], Z: [{}, {}], M: [{}, {}]]",
            fixed(e.x_min),
            fixed(e.x_max),
            fixed(e.y_min),
            fixed(e.y_max),
            fixed(e.z_min),
            fixed(e.z_max),
            fixed(e.m_min),
            fixed(e.m_max)
        )?;
        write!(f, ", Types: [{}]", self.types.names(true).join(", "))?;
        let b = |v: bool| if v { "true" } else { "false" };
        write!(
            f,
            ", Flags: [Has Empty Geom: {}, Has No Empty Geom: {}, Has Empty Part: {}, Has No Empty Part: {}]]",
            b(self.flags.has_empty_geometry()),
            b(self.flags.has_non_empty_geometry()),
            b(self.flags.has_empty_part()),
            b(self.flags.has_non_empty_part())
        )
    }
}

/// C's `%f`: six decimals, `inf`/`-inf`/`nan` for non-finite values.
fn fixed(value: f64) -> String {
    if value.is_nan() {
        if value.is_sign_negative() { "-nan" } else { "nan" }.to_owned()
    } else if value.is_infinite() {
        if value > 0.0 { "inf" } else { "-inf" }.to_owned()
    } else {
        format!("{value:.6}")
    }
}
