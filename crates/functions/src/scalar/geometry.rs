//! The GEOMETRY functions of the core (DuckDB's `function/scalar/geometry`):
//! `st_geomfromwkb`, `st_aswkb`, `st_astext`, `st_intersects_extent`, `st_crs` and
//! `st_setcrs`.

use std::sync::Arc;

use rustdb_common::{Error, Result};
use rustdb_function::{BindInput, FunctionData, NullHandling, ScalarFunction, ScalarFunctionSet};
use rustdb_types::geometry::{self, GeometryExtent};
use rustdb_types::{LogicalType, LogicalTypeId, TypeInfo, Value, Vector};

use crate::scalar::executor::{Str, StrOut, binary, unary};

fn single(function: ScalarFunction) -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new(Arc::clone(&function.name));
    set.add(function);
    set
}

/// `st_geomfromwkb(blob)`: any-endian (E)WKB to GEOMETRY (`Geometry::FromBinary`, strict).
pub fn st_geomfromwkb_functions() -> ScalarFunctionSet {
    single(ScalarFunction::new(
        "st_geomfromwkb",
        vec![LogicalType::BLOB],
        LogicalType::GEOMETRY,
        |args, _, result| {
            unary::<Str, StrOut>(args, result, |wkb, out| {
                let converted = geometry::from_binary(wkb, true)?.unwrap_or_default();
                out.extend_from_slice(&converted);
                Ok(true)
            })
        },
    ))
}

/// `st_aswkb(geometry)`: the stored WKB, shared with the input (`ToWKBFunction` adds a
/// heap reference instead of copying).
pub fn st_aswkb_functions() -> ScalarFunctionSet {
    single(ScalarFunction::new(
        "st_aswkb",
        vec![LogicalType::GEOMETRY],
        LogicalType::BLOB,
        |args, _, result| {
            *result = args.column(0).reinterpret(LogicalType::BLOB)?;
            Ok(())
        },
    ))
}

/// `st_astext(geometry)`: WKT (`Geometry::ToString`).
pub fn st_astext_functions() -> ScalarFunctionSet {
    single(ScalarFunction::new(
        "st_astext",
        vec![LogicalType::GEOMETRY],
        LogicalType::VARCHAR,
        |args, _, result| {
            unary::<Str, StrOut>(args, result, |wkb, out| {
                geometry::to_text(wkb, out)?;
                Ok(true)
            })
        },
    ))
}

/// `st_intersects_extent(a, b)`: whether the XY bounding boxes intersect; false if
/// either geometry is empty.
pub fn st_intersects_extent_functions() -> ScalarFunctionSet {
    single(ScalarFunction::new(
        "st_intersects_extent",
        vec![LogicalType::GEOMETRY, LogicalType::GEOMETRY],
        LogicalType::BOOLEAN,
        |args, _, result| {
            binary::<Str, Str, bool>(args, result, |left, right, out| {
                let mut left_extent = GeometryExtent::empty();
                let mut right_extent = GeometryExtent::empty();
                let left_empty = geometry::extent(left, &mut left_extent)? == 0;
                let right_empty = geometry::extent(right, &mut right_extent)? == 0;
                *out = !left_empty && !right_empty && left_extent.intersects_xy(&right_extent);
                Ok(true)
            })
        },
    ))
}

/// The CRS of a GEOMETRY type as a VARCHAR value, NULL without one (`GetCRSValue`).
fn crs_value(ty: &LogicalType) -> Value {
    match ty.info() {
        Some(TypeInfo::Geo(info)) if !info.crs.definition.is_empty() => Value::varchar(&info.crs.definition),
        _ => Value::null(LogicalType::VARCHAR),
    }
}

/// `st_crs(geometry)`: the type's CRS, the same for every row (DuckDB folds the call
/// into a constant when binding).
pub fn st_crs_functions() -> ScalarFunctionSet {
    let mut function = ScalarFunction::new(
        "st_crs",
        vec![LogicalType::GEOMETRY],
        LogicalType::VARCHAR,
        |args, _, result| {
            *result = Vector::constant(&crs_value(args.column(0).logical_type()))?;
            Ok(())
        },
    )
    .with_bind(|function, input: &BindInput<'_>| {
        // `BindCRSFunction`: the argument keeps its CRS.
        if input.argument_types[0].id() == LogicalTypeId::Geometry {
            function.arguments[0] = input.argument_types[0].clone();
        }
        Ok(None)
    })
    .with_bind_expression(|_, argument_types| {
        // `BindCRSFunctionExpression`, which sees the argument cast to GEOMETRY: literals
        // and NULLs become a GEOMETRY without CRS; parameters are not known yet.
        let ty = argument_types.first()?;
        match ty.id() {
            LogicalTypeId::Geometry => Some(crs_value(ty)),
            LogicalTypeId::StringLiteral | LogicalTypeId::Varchar | LogicalTypeId::SqlNull => {
                Some(Value::null(LogicalType::VARCHAR))
            }
            _ => None,
        }
    });
    function.null_handling = NullHandling::Special;
    single(function)
}

/// `st_setcrs(geometry, crs)`: the same data typed with another CRS.
pub fn st_setcrs_functions() -> ScalarFunctionSet {
    let function = ScalarFunction::new(
        "st_setcrs",
        vec![LogicalType::GEOMETRY, LogicalType::VARCHAR],
        LogicalType::GEOMETRY,
        |args, _, result| {
            *result = args.column(0).reinterpret(result.logical_type().clone())?;
            Ok(())
        },
    )
    .with_bind(
        |function, input: &BindInput<'_>| -> Result<Option<Arc<dyn FunctionData>>> {
            if input.argument_types[1].id() == LogicalTypeId::Unknown {
                return Err(crate::parameter_not_resolved());
            }
            let Some(crs) = input.constants.get(1).cloned().flatten() else {
                return Err(Error::binder("ST_SetCRS: CRS argument must be constant!"));
            };
            if !crs.is_null() {
                // The identified CRS, else the raw string (better than nothing).
                let crs = crs.to_varchar()?;
                function.return_type = match geometry::try_identify(&crs)? {
                    Some(identified) => LogicalType::geometry_with_crs(identified.definition)?,
                    None => LogicalType::geometry_with_crs(crs)?,
                };
            }
            Ok(None)
        },
    );
    single(function)
}

/// Every GEOMETRY function set with its aliases.
pub fn geometry_functions() -> Vec<(ScalarFunctionSet, &'static [&'static str])> {
    vec![
        (st_geomfromwkb_functions(), &[]),
        (st_aswkb_functions(), &["st_asbinary"]),
        (st_astext_functions(), &["st_aswkt"]),
        (st_intersects_extent_functions(), &["&&"]),
        (st_crs_functions(), &[]),
        (st_setcrs_functions(), &[]),
    ]
}
