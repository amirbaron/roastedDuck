//! VARIANT functions (DuckDB's `function/scalar/variant`): `variant_extract`,
//! `variant_typeof` and `variant_normalize`.

use std::sync::Arc;

use rustdb_common::{Error, Result};
use rustdb_function::{BindInput, FunctionData, NullHandling, ScalarFunction, ScalarFunctionSet};
use rustdb_types::variant::{VariantData, VariantLogicalType, VariantPathComponent};
use rustdb_types::{LogicalType, LogicalTypeId, Value, Vector};

/// The path step of a `variant_extract` call (DuckDB's `VariantExtractBindData`).
#[derive(Debug, Clone, PartialEq, Eq)]
struct ExtractPath(VariantPathComponent);

impl FunctionData for ExtractPath {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// Applies `kernel` to every row of a VARIANT column, producing a vector of `ty`; NULL
/// rows stay NULL unless `kernel` handles them.
fn map_rows(
    input: &Vector,
    count: usize,
    ty: &LogicalType,
    mut kernel: impl FnMut(Option<VariantData>) -> Result<Option<Value>>,
) -> Result<Vector> {
    let rows = if input.is_constant() { count.min(1) } else { count };
    let mut out = Vector::flat(ty.clone(), rows.max(1));
    for row in 0..rows {
        match kernel(VariantData::from_value(&input.get_value(row))?)? {
            Some(value) if !value.is_null() => out.set_value(row, &value)?,
            _ => out.set_null(row),
        }
    }
    if input.is_constant() {
        let value = out.get_value(0);
        return if value.is_null() {
            Ok(Vector::constant_null(ty.clone()))
        } else {
            Vector::constant(&value)
        };
    }
    Ok(out)
}

/// `VariantExtractBind`: the path must be a constant VARCHAR (field) or UINTEGER
/// (1-based index).
fn bind_extract(_: &mut ScalarFunction, input: &BindInput<'_>) -> Result<Option<Arc<dyn FunctionData>>> {
    if input.argument_types.len() != 2 {
        return Err(Error::binder(
            "'variant_extract' expects two arguments, VARIANT column and VARCHAR path",
        ));
    }
    let path_type = &input.argument_types[1];
    if !matches!(path_type.id(), LogicalTypeId::Varchar | LogicalTypeId::UInteger) {
        return Err(Error::binder(format!(
            "'variant_extract' expects the second argument to be of type VARCHAR or UINTEGER, not {path_type}"
        )));
    }
    let Some(constant) = input.constants[1].as_ref().filter(|value| !value.is_null()) else {
        return Err(Error::binder(
            "'variant_extract' expects the second argument to be a constant expression",
        ));
    };
    let component = if path_type.id() == LogicalTypeId::Varchar {
        VariantPathComponent::Key(constant.to_varchar()?)
    } else {
        let index = constant.as_u32().unwrap_or_default();
        if index == 0 {
            return Err(Error::binder(
                "Extracting index 0 from VARIANT(ARRAY) is invalid, indexes are 1-based",
            ));
        }
        VariantPathComponent::Index(index - 1)
    };
    Ok(Some(Arc::new(ExtractPath(component))))
}

/// `VariantUtils::VariantExtract` for one step: the child as a VARIANT rooted at it, or
/// NULL if the value is not of the right kind, has no such child, or the child is NULL.
fn extract_child(data: &VariantData, component: &VariantPathComponent) -> Result<Option<Value>> {
    let expected = match component {
        VariantPathComponent::Index(_) => VariantLogicalType::Array,
        VariantPathComponent::Key(_) => VariantLogicalType::Object,
    };
    if data.type_id(0)? != expected {
        return Ok(None);
    }
    let Some(child) = data.find_child(data.nested(0)?, component)? else {
        return Ok(None);
    };
    if data.type_id(child)? == VariantLogicalType::VariantNull {
        return Ok(None);
    }
    Ok(Some(data.rooted_at(child).to_value()))
}

fn extract_overload(path: LogicalType) -> ScalarFunction {
    ScalarFunction::new(
        "variant_extract",
        vec![LogicalType::variant(), path],
        LogicalType::variant(),
        |args, state, result| {
            let path = state
                .bind_data
                .as_ref()
                .and_then(|data| data.as_any().downcast_ref::<ExtractPath>())
                .ok_or_else(|| Error::internal("variant_extract without bind data"))?;
            let ty = result.logical_type().clone();
            *result = map_rows(args.column(0), args.len(), &ty, |data| match data {
                Some(data) => extract_child(&data, &path.0),
                None => Ok(None),
            })?;
            Ok(())
        },
    )
    .with_bind(bind_extract)
}

/// `variant_extract(variant, field)` and `variant_extract(variant, index)`.
pub fn variant_extract_functions() -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new("variant_extract");
    set.add(extract_overload(LogicalType::VARCHAR));
    set.add(extract_overload(LogicalType::UINTEGER));
    set
}

/// `variant_typeof(variant)`: the type of the root value; NULL rows are `VARIANT_NULL`.
pub fn variant_typeof_functions() -> ScalarFunctionSet {
    let mut function = ScalarFunction::new(
        "variant_typeof",
        vec![LogicalType::variant()],
        LogicalType::VARCHAR,
        |args, _, result| {
            *result = map_rows(args.column(0), args.len(), &LogicalType::VARCHAR, |data| {
                Ok(Some(Value::varchar(match data {
                    Some(data) => data.type_name(0)?,
                    None => VariantLogicalType::VariantNull.name().to_owned(),
                })))
            })?;
            Ok(())
        },
    );
    function.null_handling = NullHandling::Special;
    let mut set = ScalarFunctionSet::new("variant_typeof");
    set.add(function);
    set
}

/// `variant_normalize(variant)`: the canonical representation (fields sorted by key),
/// used as the collation of VARIANT.
pub fn variant_normalize_functions() -> ScalarFunctionSet {
    let function = ScalarFunction::new(
        "variant_normalize",
        vec![LogicalType::variant()],
        LogicalType::variant(),
        |args, _, result| {
            let ty = result.logical_type().clone();
            *result = map_rows(args.column(0), args.len(), &ty, |data| {
                data.map(|data| Ok(data.normalize()?.to_value())).transpose()
            })?;
            Ok(())
        },
    );
    let mut set = ScalarFunctionSet::new("variant_normalize");
    set.add(function);
    set
}

/// Every VARIANT function.
pub fn variant_functions() -> Vec<ScalarFunctionSet> {
    vec![
        variant_extract_functions(),
        variant_typeof_functions(),
        variant_normalize_functions(),
    ]
}
