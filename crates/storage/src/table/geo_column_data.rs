//! GEOMETRY column storage helpers (DuckDB's `GeoColumnData`): the statistics of a
//! shredded column come from the statistics of its vectorized layout.

use rustdb_types::GeometryStorageType;
use rustdb_types::geometry::{self, GeometryType, VertexType};

use crate::statistics::{BaseStatistics, GeometryStats, StatsData};

/// `GetVertexStats`: the statistics of the vertex STRUCT fields inside the layout
/// statistics of `geometry`.
fn vertex_stats(stats: &BaseStatistics, geometry: GeometryType) -> Option<&[BaseStatistics]> {
    let depth = match geometry {
        GeometryType::Point => 0,
        GeometryType::LineString | GeometryType::MultiPoint => 1,
        GeometryType::Polygon | GeometryType::MultiLineString => 2,
        GeometryType::MultiPolygon => 3,
        GeometryType::GeometryCollection | GeometryType::Invalid => return None,
    };
    let mut current = stats;
    for _ in 0..depth {
        current = current.child()?;
    }
    current.struct_children()
}

/// The minimum and maximum of DOUBLE statistics (`NumericStats::GetMin/GetMax<double>`),
/// an empty range when they are unknown.
fn double_range(stats: Option<&BaseStatistics>) -> (f64, f64) {
    let min = stats
        .and_then(BaseStatistics::numeric_min)
        .and_then(|value| value.as_f64());
    let max = stats
        .and_then(BaseStatistics::numeric_max)
        .and_then(|value| value.as_f64());
    (min.unwrap_or(f64::INFINITY), max.unwrap_or(f64::NEG_INFINITY))
}

/// `GeoColumnData::InterpretStats`: geometry statistics of a column whose every value is
/// a non-empty `geometry`/`vertex` geometry, from the statistics of its layout.
pub(crate) fn interpret_stats(
    source: &BaseStatistics,
    target: &mut BaseStatistics,
    geometry: GeometryType,
    vertex: VertexType,
) {
    target.copy_base(source);
    let fields = vertex_stats(source, geometry).unwrap_or_default();
    let Some(data) = target.geometry_mut() else {
        return;
    };
    let extent = &mut data.extent;
    (extent.x_min, extent.x_max) = double_range(fields.first());
    (extent.y_min, extent.y_max) = double_range(fields.get(1));
    match vertex {
        VertexType::Xyz => (extent.z_min, extent.z_max) = double_range(fields.get(2)),
        VertexType::Xym => (extent.m_min, extent.m_max) = double_range(fields.get(2)),
        VertexType::Xyzm => {
            (extent.z_min, extent.z_max) = double_range(fields.get(2));
            (extent.m_min, extent.m_max) = double_range(fields.get(3));
        }
        VertexType::Xy => {}
    }
    data.types.clear();
    data.types.add(geometry, vertex);
    data.flags.clear();
    data.flags.set_has_non_empty_geometry();
    data.flags.set_has_non_empty_part();
}

/// Merges the statistics of the stored layout of a GEOMETRY column into its geometry
/// statistics `target` (`GeoColumnData::InitializeColumn`): shredded layouts are
/// interpreted; the legacy format keeps the stored (unknown) geometry statistics.
pub(crate) fn interpret_layout_statistics(
    layout: &BaseStatistics,
    storage: GeometryStorageType,
    target: &mut BaseStatistics,
) {
    if let Some((geometry, vertex)) = geometry::specialized_type(storage) {
        interpret_stats(layout, target, geometry, vertex);
        return;
    }
    match layout.data() {
        StatsData::Geometry(_) => target.merge(layout),
        _ => {
            // Just written in the legacy format (a BLOB with string statistics).
            target.copy_base(layout);
            if let Some(data) = target.geometry_mut() {
                *data = GeometryStats::unknown();
            }
        }
    }
}
