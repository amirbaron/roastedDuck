//! GEOMETRY columns (DuckDB's `parquet_geometry.cpp` and `GeometryStatsData`): the
//! statistics of written geometries, the `GeoParquet` `geo` metadata and the coordinate
//! reference systems they carry.

use rustdb_common::{CaseInsensitiveMap, Error, ErrorKind, Result};
use rustdb_types::geometry::{self, GeometryExtent};
use rustdb_types::{CoordinateReferenceSystem, GeoTypeInfo, LogicalType, LogicalTypeId, TypeInfo};

use crate::thrift::{BoundingBox, FileMetaData, GeospatialStatistics};

/// The default CRS of `GeoParquet`, not written to the metadata.
pub const DEFAULT_CRS: &str = "OGC:CRS84";

/// Which `GeoParquet` metadata and Parquet geometry types a write produces
/// (`GeoParquetVersion`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GeoParquetVersion {
    /// `GeoParquet` 1.0 metadata over plain BLOB columns.
    #[default]
    V1,
    /// `GeoParquet` 2.0 metadata over native Parquet geometry columns.
    V2,
    /// `GeoParquet` 1.0 metadata over native Parquet geometry columns.
    Both,
    /// Native Parquet geometry columns without `GeoParquet` metadata.
    None,
}

impl GeoParquetVersion {
    /// Whether columns get the native Parquet GEOMETRY type and statistics.
    pub fn native(self) -> bool {
        self != Self::V1
    }

    /// Whether the `geo` key/value metadata is written.
    pub fn json(self) -> bool {
        self != Self::None
    }
}

/// The geometry and vertex types seen in a column (`GeometryTypeSet`): one bit per
/// geometry type in a byte per vertex type (XY, XYZ, XYM, XYZM).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GeometryTypeSet {
    sets: [u8; 4],
}

impl GeometryTypeSet {
    pub fn add(&mut self, geometry_type: u32, vertex_type: u32) {
        if let (Some(set), true) = (self.sets.get_mut(vertex_type as usize), geometry_type < 8) {
            *set |= 1 << geometry_type;
        }
    }

    pub fn merge(&mut self, other: &Self) {
        for (set, other) in self.sets.iter_mut().zip(other.sets) {
            *set |= other;
        }
    }

    pub fn is_empty(&self) -> bool {
        self.sets.iter().all(|&set| set == 0)
    }

    /// The WKB type codes (`ToWKBList`).
    pub fn to_wkb_list(&self) -> Vec<i32> {
        let mut result = Vec::new();
        for (vertex, set) in self.sets.iter().enumerate() {
            for geometry in 1..8 {
                if set & (1 << geometry) != 0 {
                    result.push(geometry + vertex as i32 * 1000);
                }
            }
        }
        result
    }

    /// The type names (`ToString`): `Point Z` style, or `point_z` with `snake_case`.
    pub fn to_strings(&self, snake_case: bool) -> Vec<String> {
        let mut result = Vec::new();
        for (vertex, set) in self.sets.iter().enumerate() {
            for geometry in 0..8 {
                if set & (1 << geometry) == 0 {
                    continue;
                }
                let (snake, camel) = match geometry {
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
                name.push_str(match (vertex, snake_case) {
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

/// `GeometryExtent::HasX() && HasY()`.
pub fn has_xy(extent: &GeometryExtent) -> bool {
    extent.x_min.is_finite()
        && extent.x_max.is_finite()
        && extent.y_min.is_finite()
        && extent.y_max.is_finite()
}

/// `GeometryExtent::HasZ`.
pub fn has_z(extent: &GeometryExtent) -> bool {
    extent.z_min.is_finite() && extent.z_max.is_finite()
}

/// `GeometryExtent::HasM`.
pub fn has_m(extent: &GeometryExtent) -> bool {
    extent.m_min.is_finite() && extent.m_max.is_finite()
}

/// `GeometryExtent::Merge`.
fn merge_extent(extent: &mut GeometryExtent, other: &GeometryExtent) {
    extent.x_min = extent.x_min.min(other.x_min);
    extent.y_min = extent.y_min.min(other.y_min);
    extent.z_min = extent.z_min.min(other.z_min);
    extent.m_min = extent.m_min.min(other.m_min);
    extent.x_max = extent.x_max.max(other.x_max);
    extent.y_max = extent.y_max.max(other.y_max);
    extent.z_max = extent.z_max.max(other.z_max);
    extent.m_max = extent.m_max.max(other.m_max);
}

/// The statistics of the geometries of a column chunk (`GeometryStatsData`).
#[derive(Clone, Copy, Debug)]
pub struct GeoStats {
    pub types: GeometryTypeSet,
    pub extent: GeometryExtent,
}

impl Default for GeoStats {
    fn default() -> Self {
        Self {
            types: GeometryTypeSet::default(),
            extent: GeometryExtent::empty(),
        }
    }
}

impl GeoStats {
    /// `GeometryStatsData::Update` with a little-endian WKB value.
    pub fn update(&mut self, wkb: &[u8]) {
        if let Some((geometry_type, vertex_type)) = root_type(wkb) {
            self.types.add(geometry_type, vertex_type);
        }
        let _ = geometry::extent(wkb, &mut self.extent);
    }

    pub fn merge(&mut self, other: &Self) {
        self.types.merge(&other.types);
        merge_extent(&mut self.extent, &other.extent);
    }

    /// `GeoStatisticsState::WriteGeoStats`: the native Parquet statistics.
    pub fn to_thrift(&self) -> GeospatialStatistics {
        let extent = &self.extent;
        let bbox = has_xy(extent).then(|| {
            let (z, m) = (has_z(extent), has_m(extent));
            BoundingBox {
                xmin: extent.x_min,
                xmax: extent.x_max,
                ymin: extent.y_min,
                ymax: extent.y_max,
                zmin: z.then_some(extent.z_min),
                zmax: z.then_some(extent.z_max),
                mmin: m.then_some(extent.m_min),
                mmax: m.then_some(extent.m_max),
            }
        });
        GeospatialStatistics {
            bbox,
            geospatial_types: Some(self.types.to_wkb_list()),
        }
    }
}

/// The geometry and vertex type of a WKB value's root (`Geometry::GetType`).
fn root_type(wkb: &[u8]) -> Option<(u32, u32)> {
    let header: [u8; 4] = wkb.get(1..5)?.try_into().ok()?;
    let meta = match wkb.first()? {
        1 => u32::from_le_bytes(header),
        _ => u32::from_be_bytes(header),
    } & 0xFFFF;
    Some((meta % 1000, meta / 1000))
}

/// The CRS definition of a GEOMETRY type (`GeoType::GetCRS`), if it has one.
pub fn type_crs(ty: &LogicalType) -> Option<&str> {
    match ty.info() {
        Some(TypeInfo::Geo(info)) if !info.crs.definition.is_empty() => Some(info.crs.definition.as_str()),
        _ => None,
    }
}

/// The GEOMETRY type with the CRS `crs` (`CoordinateReferenceSystem::TryIdentify`): the
/// `GeoParquet` default is identified by its name; other definitions are kept as given.
pub fn geometry_type(crs: &str) -> LogicalType {
    let definition = identify_crs(crs);
    if definition.is_empty() {
        return LogicalType::GEOMETRY;
    }
    LogicalType::with_info(
        LogicalTypeId::Geometry,
        TypeInfo::Geo(GeoTypeInfo {
            crs: CoordinateReferenceSystem { definition },
        }),
    )
}

/// The shortest name of a CRS: `OGC:CRS84` for its PROJJSON form, the compact JSON for
/// other PROJJSON definitions, and anything else as is.
fn identify_crs(crs: &str) -> String {
    let trimmed = crs.trim();
    if !trimmed.starts_with('{') {
        return trimmed.to_owned();
    }
    let Ok(json) = serde_json::from_str::<serde_json::Value>(trimmed) else {
        return trimmed.to_owned();
    };
    let id = json.get("id");
    let authority = id
        .and_then(|id| id.get("authority"))
        .and_then(serde_json::Value::as_str);
    let code = id.and_then(|id| id.get("code")).map(|code| match code {
        serde_json::Value::String(code) => code.clone(),
        other => other.to_string(),
    });
    if let (Some(authority), Some(code)) = (authority, code)
        && authority.eq_ignore_ascii_case("OGC")
        && code.eq_ignore_ascii_case("CRS84")
    {
        return DEFAULT_CRS.to_owned();
    }
    serde_json::to_string(&json).unwrap_or_else(|_| trimmed.to_owned())
}

/// One geometry column of the `GeoParquet` metadata (`GeoParquetColumnMetadata`).
#[derive(Clone, Debug, Default)]
pub struct GeoColumn {
    pub stats: GeoStats,
    /// The CRS in PROJJSON (empty for the default).
    pub projjson: String,
    pub insertion_index: usize,
}

/// The `GeoParquet` `geo` metadata of a file (`GeoParquetFileMetadata`).
#[derive(Clone, Debug, Default)]
pub struct GeoParquetMetadata {
    columns: CaseInsensitiveMap<GeoColumn>,
    order: Vec<String>,
}

impl GeoParquetMetadata {
    /// `GeoParquetFileMetadata::TryRead`: the metadata of `key_value_metadata`, if valid.
    pub fn read(metadata: &FileMetaData) -> Result<Option<Self>> {
        let Some(value) = metadata
            .key_value_metadata
            .iter()
            .flatten()
            .find(|kv| kv.key.0 == b"geo")
            .and_then(|kv| kv.value.as_ref())
        else {
            return Ok(None);
        };
        let Ok(root) = serde_json::from_slice::<serde_json::Value>(&value.0) else {
            return Ok(None);
        };
        let Some(root) = root.as_object() else {
            return Err(Error::invalid_input("Geoparquet metadata is not an object"));
        };
        let Some(version) = root.get("version").and_then(serde_json::Value::as_str) else {
            return Err(Error::invalid_input(
                "Geoparquet metadata does not have a version",
            ));
        };
        if version.starts_with('3') {
            return Err(Error::invalid_input(format!(
                "Geoparquet version {version} is not supported"
            )));
        }
        let Some(columns) = root.get("columns").and_then(serde_json::Value::as_object) else {
            return Err(Error::invalid_input(
                "Geoparquet metadata does not have a columns object",
            ));
        };
        let mut result = Self::default();
        for (name, column) in columns {
            let Some(column) = column.as_object() else {
                return Err(Error::invalid_input(format!(
                    "Geoparquet column '{name}' is not an object"
                )));
            };
            let Some(encoding) = column.get("encoding").and_then(serde_json::Value::as_str) else {
                return Err(Error::invalid_input(format!(
                    "Geoparquet column '{name}' does not have an encoding"
                )));
            };
            if !matches!(
                encoding,
                "WKB"
                    | "point"
                    | "linestring"
                    | "polygon"
                    | "multipoint"
                    | "multilinestring"
                    | "multipolygon"
            ) {
                return Err(Error::invalid_input(format!(
                    "Geoparquet column '{name}' has an unsupported encoding"
                )));
            }
            if !column
                .get("geometry_types")
                .is_some_and(serde_json::Value::is_array)
            {
                return Err(Error::invalid_input(format!(
                    "Geoparquet column '{name}' does not have geometry types"
                )));
            }
            let projjson = match column.get("crs") {
                None => DEFAULT_CRS.to_owned(),
                Some(serde_json::Value::Null) => String::new(),
                Some(crs @ serde_json::Value::Object(_)) => serde_json::to_string(crs).unwrap_or_default(),
                Some(_) => {
                    return Err(Error::invalid_input(format!(
                        "Geoparquet column '{name}' has invalid CRS"
                    )));
                }
            };
            result.insert(name, projjson);
        }
        Ok(Some(result))
    }

    fn insert(&mut self, name: &str, projjson: String) {
        if !self.columns.contains_key(name) {
            self.order.push(name.to_owned());
        }
        let insertion_index = self.order.len() - 1;
        self.columns.insert(
            name.to_owned(),
            GeoColumn {
                stats: GeoStats::default(),
                projjson,
                insertion_index,
            },
        );
    }

    /// The CRS of a geometry column, `None` if `name` is not one.
    pub fn column_crs(&self, name: &str) -> Option<&str> {
        self.columns.get(name).map(|column| column.projjson.as_str())
    }

    /// `AddGeoParquetStats`: merges the statistics of a column chunk.
    pub fn add_stats(
        &mut self,
        name: &str,
        ty: &LogicalType,
        stats: &GeoStats,
        version: GeoParquetVersion,
    ) -> Result<()> {
        if let Some(column) = self.columns.get_mut(name) {
            column.stats.merge(stats);
            return Ok(());
        }
        let mut projjson = String::new();
        if let Some(crs) = type_crs(ty)
            && !crs.eq_ignore_ascii_case(DEFAULT_CRS)
        {
            // `ConvertCRS`: only PROJJSON can be written to GeoParquet 1.
            if crs.trim_start().starts_with('{') || version == GeoParquetVersion::V2 {
                crs.clone_into(&mut projjson);
            } else {
                return Err(Error::invalid_input(format!(
                    "Cannot write GeoParquet V1 metadata for column '{name}': GeoParquet V1 only supports PROJJSON CRS definitions"
                )));
            }
        }
        self.order.push(name.to_owned());
        let mut column = GeoColumn {
            stats: GeoStats::default(),
            projjson,
            insertion_index: self.order.len() - 1,
        };
        column.stats.merge(stats);
        self.columns.insert(name.to_owned(), column);
        Ok(())
    }

    /// `GeoParquetFileMetadata::Write`: the `geo` JSON, `None` when no column remains.
    pub fn to_json(&self, version: GeoParquetVersion) -> Result<Option<String>> {
        use serde_json::{Map, Value, json};
        // GeoParquet has no M coordinates: such columns are left out.
        let columns: Vec<(&String, &GeoColumn)> = self
            .order
            .iter()
            .filter_map(|name| self.columns.get(name).map(|column| (name, column)))
            .filter(|(_, column)| !has_m(&column.stats.extent))
            .collect();
        let Some((primary, _)) = columns.iter().min_by_key(|(_, column)| column.insertion_index) else {
            return Ok(None);
        };
        let version_text = match version {
            GeoParquetVersion::V1 | GeoParquetVersion::Both => "1.0.0",
            GeoParquetVersion::V2 => "2.0.0",
            GeoParquetVersion::None => {
                return Err(Error::internal(
                    "GeoParquetVersion::NONE should not write metadata",
                ));
            }
        };
        let mut json_columns = Map::new();
        for (name, column) in &columns {
            let mut entry = Map::new();
            entry.insert("encoding".to_owned(), json!("WKB"));
            entry.insert(
                "geometry_types".to_owned(),
                Value::Array(
                    column
                        .stats
                        .types
                        .to_strings(false)
                        .into_iter()
                        .map(Value::String)
                        .collect(),
                ),
            );
            let extent = &column.stats.extent;
            if has_xy(extent) {
                let bbox = if has_z(extent) {
                    vec![
                        extent.x_min,
                        extent.y_min,
                        extent.z_min,
                        extent.x_max,
                        extent.y_max,
                        extent.z_max,
                    ]
                } else {
                    vec![extent.x_min, extent.y_min, extent.x_max, extent.y_max]
                };
                entry.insert("bbox".to_owned(), json!(bbox));
            }
            if !column.projjson.is_empty() {
                match serde_json::from_str::<Value>(&column.projjson) {
                    Ok(crs) => {
                        entry.insert("crs".to_owned(), crs);
                    }
                    Err(_) if version == GeoParquetVersion::V2 => {}
                    Err(_) => {
                        return Err(Error::new(
                            ErrorKind::Serialization,
                            format!("Failed to parse CRS PROJJSON for column '{name}'"),
                        ));
                    }
                }
            }
            json_columns.insert((*name).clone(), Value::Object(entry));
        }
        let mut root = Map::new();
        root.insert("version".to_owned(), json!(version_text));
        root.insert("primary_column".to_owned(), json!(primary));
        root.insert("columns".to_owned(), Value::Object(json_columns));
        serde_json::to_string(&Value::Object(root))
            .map(Some)
            .map_err(|error| {
                Error::new(
                    ErrorKind::Serialization,
                    format!("Failed to write JSON string: {error}"),
                )
            })
    }
}
