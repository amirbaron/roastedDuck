//! Special argument types used in function signatures (DuckDB's `LogicalType::ANY_PARAMS`,
//! `LogicalType::TEMPLATE`, ...).

use rustdb_types::{AnyTypeInfo, LogicalType, LogicalTypeId, TemplateTypeInfo, TypeInfo};

/// `ANY` that the binder casts to `target` (DuckDB's `LogicalType::ANY_PARAMS`).
pub(crate) fn any_params(target: LogicalType) -> LogicalType {
    LogicalType::with_info(
        LogicalTypeId::Any,
        TypeInfo::Any(AnyTypeInfo {
            target_type: target,
            cast_score: 5,
        }),
    )
}

/// A template type resolved from the arguments at bind time (DuckDB's
/// `LogicalType::TEMPLATE`).
pub(crate) fn template(name: &str) -> LogicalType {
    LogicalType::with_info(
        LogicalTypeId::Template,
        TypeInfo::Template(TemplateTypeInfo {
            name: name.to_owned(),
        }),
    )
}

/// The concrete type of a literal type (DuckDB's `LogicalType::NormalizeType`).
pub(crate) fn normalize_type(ty: &LogicalType) -> LogicalType {
    match ty.id() {
        LogicalTypeId::StringLiteral => LogicalType::VARCHAR,
        LogicalTypeId::IntegerLiteral => ty.integer_literal_type().cloned().unwrap_or(LogicalType::INTEGER),
        _ => ty.clone(),
    }
}

/// A type id without type parameters (`DECIMAL`, `LIST`, ... as in `LogicalTypeId::DECIMAL`
/// signatures), matched by any concrete type with that id.
pub(crate) fn type_id(id: LogicalTypeId) -> LogicalType {
    LogicalType::simple(id)
}
