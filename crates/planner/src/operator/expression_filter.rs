//! Arbitrary single-column predicates pushed into table scans (DuckDB's
//! `ExpressionFilter`). The planner stores the predicate; the physical planner swaps in
//! an implementation that evaluates it.

use rustdb_common::{Error, Result};
use rustdb_storage::statistics::{BaseStatistics, FilterPropagateResult, GeometryStats};
use rustdb_storage::table_filter::ExpressionFilter;
use rustdb_types::geometry::{self, GeometryExtent};
use rustdb_types::{LogicalType, LogicalTypeId, Value, Vector};

use crate::expression::{Expression, ExpressionKind};

/// A predicate over one scanned column, which it reads as `BOUND_REF #0` (DuckDB's
/// `ExpressionFilter::expr`).
#[derive(Clone, Debug)]
pub struct ExpressionTableFilter {
    pub expression: Expression,
}

impl ExpressionTableFilter {
    /// The predicate with the column reference named `column_name` (DuckDB's
    /// `ExpressionFilter::ToExpression`).
    pub fn to_expression(&self, column_name: impl AsRef<[u8]>) -> Expression {
        fn replace(expression: &mut Expression, column_name: &[u8]) {
            if matches!(expression.kind, ExpressionKind::Reference(_)) {
                expression.alias = column_name.into();
                return;
            }
            expression.for_each_child_mut(|child| replace(child, column_name));
        }
        let mut expression = self.expression.clone();
        replace(&mut expression, column_name.as_ref());
        expression
    }
}

/// `CheckIntersectionFilter` (`geometry_stats.cpp`): whether a bounding-box predicate
/// with the constant `constant` can match rows of geometry statistics `data`.
fn check_intersection_filter(data: &GeometryStats, constant: &Value) -> FilterPropagateResult {
    if constant.is_null() || constant.logical_type().id() != LogicalTypeId::Geometry {
        return FilterPropagateResult::NoPruningPossible;
    }
    let Some(wkb) = constant.as_bytes() else {
        return FilterPropagateResult::NoPruningPossible;
    };
    let mut extent = GeometryExtent::empty();
    match geometry::extent(wkb, &mut extent) {
        // An empty geometry never intersects anything.
        Ok(0) => return FilterPropagateResult::FilterAlwaysFalse,
        Ok(_) => {}
        Err(_) => return FilterPropagateResult::NoPruningPossible,
    }
    if !extent.intersects_xy(&data.extent) {
        return FilterPropagateResult::FilterAlwaysFalse;
    }
    if extent.contains_xy(&data.extent) {
        return FilterPropagateResult::FilterAlwaysTrue;
    }
    FilterPropagateResult::NoPruningPossible
}

/// A GEOMETRY -> GEOMETRY cast only changes the CRS, not the coordinates: the expression
/// it casts.
fn strip_geometry_cast(child: &Expression) -> &Expression {
    if let ExpressionKind::Cast(cast) = &child.kind
        && cast.child.return_type.id() == LogicalTypeId::Geometry
    {
        return &cast.child;
    }
    child
}

/// The value of `constant` when it is a constant compared with the column reference
/// `reference`.
fn constant_and_reference<'a>(constant: &'a Expression, reference: &Expression) -> Option<&'a Value> {
    match (&constant.kind, &reference.kind) {
        (ExpressionKind::Constant(constant), ExpressionKind::Reference(_)) => Some(&constant.value),
        _ => None,
    }
}

/// `GeometryStats::CheckZonemap`: what the statistics of a GEOMETRY column say about a
/// bounding-box predicate (`&&`, `st_intersects_extent`) between the column and a
/// constant.
pub fn geometry_check_zonemap(stats: &BaseStatistics, expression: &Expression) -> FilterPropagateResult {
    let ExpressionKind::Function(function) = &expression.kind else {
        return FilterPropagateResult::NoPruningPossible;
    };
    if expression.return_type != LogicalType::BOOLEAN || function.children.len() != 2 {
        return FilterPropagateResult::NoPruningPossible;
    }
    if function
        .children
        .iter()
        .any(|child| child.return_type.id() != LogicalTypeId::Geometry)
    {
        return FilterPropagateResult::NoPruningPossible;
    }
    let name = function.function.name.as_ref();
    if !["&&", "st_intersects_extent"]
        .iter()
        .any(|predicate| predicate.as_bytes().eq_ignore_ascii_case(name))
    {
        return FilterPropagateResult::NoPruningPossible;
    }
    let lhs = strip_geometry_cast(&function.children[0]);
    let rhs = strip_geometry_cast(&function.children[1]);
    let constant = constant_and_reference(lhs, rhs).or_else(|| constant_and_reference(rhs, lhs));
    if !stats.can_have_no_null() {
        return FilterPropagateResult::FilterAlwaysFalse;
    }
    let Some(data) = stats.geometry() else {
        return FilterPropagateResult::NoPruningPossible;
    };
    if !data.extent.can_prune_xy() {
        return FilterPropagateResult::NoPruningPossible;
    }
    let result = match constant {
        Some(constant) => check_intersection_filter(data, constant),
        None => FilterPropagateResult::NoPruningPossible,
    };
    // NULLs and empty geometries are not in the extent, but never match either.
    if result == FilterPropagateResult::FilterAlwaysTrue
        && (stats.can_have_null() || data.flags.has_empty_geometry())
    {
        return FilterPropagateResult::NoPruningPossible;
    }
    result
}

impl ExpressionFilter for ExpressionTableFilter {
    fn select(&self, _: &Vector, _: usize, _: &mut Vec<u32>) -> Result<()> {
        Err(Error::internal(
            "expression table filter evaluated before physical planning",
        ))
    }

    /// `ExpressionFilter::CheckStatistics`: only GEOMETRY statistics can prune.
    fn check_statistics(&self, stats: &BaseStatistics) -> FilterPropagateResult {
        if stats.geometry().is_some() {
            return geometry_check_zonemap(stats, &self.expression);
        }
        FilterPropagateResult::NoPruningPossible
    }

    fn to_sql(&self, column_name: &[u8]) -> rustdb_common::ByteString {
        self.to_expression(column_name).to_bytes()
    }

    /// `ExpressionFilter::Serialize`.
    fn serialize(&self, serializer: &mut dyn rustdb_serde::Serializer) -> Result<()> {
        serializer.write_property(
            100,
            "filter_type",
            &rustdb_storage::TableFilterType::ExpressionFilter,
        )?;
        serializer.write_property_with_default(200, "expr", &Some(&self.expression))
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
