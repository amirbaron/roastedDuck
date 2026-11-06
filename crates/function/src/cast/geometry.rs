//! Casts of the GEOMETRY type (DuckDB's `geo_casts.cpp` and `TryCastToGeometry`):
//! VARCHAR parses WKT, GEOMETRY prints WKT, and GEOMETRY types with the same (or no)
//! coordinate reference system share their data.

use rustdb_common::{Error, Result};
use rustdb_types::{LogicalType, LogicalTypeId, TypeInfo};

use super::BoundCastInfo;
use super::executor::{RowError, try_map_strings_to_string};
use rustdb_types::geometry;

/// The CRS definition of a GEOMETRY type, if it has one (`GeoType::GetCRS`).
fn crs(ty: &LogicalType) -> Option<&str> {
    match ty.info() {
        Some(TypeInfo::Geo(info)) if !info.crs.definition.is_empty() => Some(info.crs.definition.as_str()),
        _ => None,
    }
}

/// `TryCastToGeometry`: WKT to WKB. Parse errors are raised even by `TRY_CAST`, as in
/// DuckDB (`Geometry::FromString` throws).
pub(super) fn from_string() -> BoundCastInfo {
    BoundCastInfo::new(|source, result, count, params| {
        try_map_strings_to_string(source, result, count, params, |text, out| {
            let wkb = geometry::from_text(text).map_err(RowError::Fatal)?;
            out.extend_from_slice(&wkb);
            Ok(())
        })
    })
}

/// `DefaultCasts::GeoCastSwitch`.
pub(super) fn cast_switch(source: &LogicalType, target: &LogicalType) -> Result<Option<BoundCastInfo>> {
    Ok(match target.id() {
        LogicalTypeId::Varchar => Some(BoundCastInfo::new(|source, result, count, params| {
            try_map_strings_to_string(source, result, count, params, |wkb, out| {
                geometry::to_text(wkb, out).map_err(RowError::Fatal)
            })
        })),
        LogicalTypeId::Geometry => {
            if let (Some(source_crs), Some(target_crs)) = (crs(source), crs(target)) {
                let source_crs = geometry::Crs::parse_lenient(source_crs);
                let target_crs = geometry::Crs::parse_lenient(target_crs);
                if !source_crs.equals(&target_crs) {
                    return Err(Error::binder(format!(
                        "Cannot cast GEOMETRY with CRS '{}' to GEOMETRY with different CRS '{}'",
                        source_crs.identifier(),
                        target_crs.identifier()
                    )));
                }
            }
            Some(BoundCastInfo::reinterpret())
        }
        _ => None,
    })
}
