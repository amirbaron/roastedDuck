//! Comparisons of single values (DuckDB's `ValueOperations` and `Value::ValuesAreEqual`).
//!
//! Values of different types are first cast to their common type
//! ([`LogicalType::force_max_logical_type`]); values that cannot be cast compare unequal.

use std::cmp::Ordering;

use rustdb_common::{Error, Result};
use rustdb_types::{LogicalType, LogicalTypeId, Value};

use super::{CastFunctionSet, ValueCast};

/// Orders two non-NULL-at-top-level values after unifying their types, or `None` if
/// either cannot be cast to the common type.
fn unified_order(left: &Value, right: &Value) -> Option<Ordering> {
    if left.logical_type() == right.logical_type() {
        return same_type_order(left, right);
    }
    let common = LogicalType::force_max_logical_type(left.logical_type(), right.logical_type());
    let left = left.try_cast(&common).ok()?;
    let right = right.try_cast(&common).ok()?;
    same_type_order(&left, &right)
}

/// Whether `ty` is or contains VARIANT.
fn contains_variant(ty: &LogicalType) -> bool {
    match ty.id() {
        LogicalTypeId::Variant => true,
        LogicalTypeId::List | LogicalTypeId::Map => ty.child_type().is_some_and(contains_variant),
        LogicalTypeId::Array => ty.array_child().is_some_and(|(child, _)| contains_variant(child)),
        LogicalTypeId::Struct | LogicalTypeId::Union => ty
            .struct_children()
            .is_some_and(|fields| fields.iter().any(|(_, field)| contains_variant(field))),
        _ => false,
    }
}

/// Orders two values of the same type; VARIANTs compare by the values they hold
/// (`TemplatedBooleanOperation` converts them with `ConvertVariantToValue`).
fn same_type_order(left: &Value, right: &Value) -> Option<Ordering> {
    if !contains_variant(left.logical_type()) {
        return left.compare_not_distinct(right);
    }
    match left.logical_type().id() {
        LogicalTypeId::Variant => {
            let decode = |value: &Value| -> Option<Value> {
                rustdb_types::variant::VariantData::from_value(value)
                    .ok()?
                    .map_or(Some(Value::null(LogicalType::SQLNULL)), |data| {
                        data.to_sql_value(0).ok()
                    })
            };
            null_last_order(&decode(left)?, &decode(right)?)
        }
        LogicalTypeId::List | LogicalTypeId::Array | LogicalTypeId::Map | LogicalTypeId::Struct => {
            let (left, right) = (left.children()?, right.children()?);
            for (left, right) in left.iter().zip(right) {
                match null_last_order(left, right)? {
                    Ordering::Equal => {}
                    other => return Some(other),
                }
            }
            Some(left.len().cmp(&right.len()))
        }
        _ => left.compare_not_distinct(right),
    }
}

/// [`unified_order`] where NULLs sort after every value and equal each other.
fn null_last_order(left: &Value, right: &Value) -> Option<Ordering> {
    match (left.is_null(), right.is_null()) {
        (true, true) => Some(Ordering::Equal),
        (true, false) => Some(Ordering::Greater),
        (false, true) => Some(Ordering::Less),
        (false, false) => unified_order(left, right),
    }
}

/// Compares two non-NULL VARIANT values row-wise (DuckDB's `DistinctSelectVariant`): the
/// stored values are converted and must have a common type; `None` if they cannot be
/// cast to it (they then compare neither equal nor ordered).
pub fn variant_order(left: &Value, right: &Value) -> Result<Option<Ordering>> {
    let decode = |value: &Value| -> Result<Value> {
        match rustdb_types::variant::VariantData::from_value(value)? {
            Some(data) => data.to_sql_value(0),
            None => Ok(Value::null(LogicalType::SQLNULL)),
        }
    };
    let (left, right) = (decode(left)?, decode(right)?);
    if LogicalType::try_max_logical_type(left.logical_type(), right.logical_type()).is_none() {
        return Err(Error::invalid_input(format!(
            "Can't compare values of type {} ({}) and type {} ({}) - an explicit cast is required",
            left.logical_type(),
            left.to_varchar()?,
            right.logical_type(),
            right.to_varchar()?
        )));
    }
    Ok(null_last_order(&left, &right))
}

fn reject_nulls(left: &Value, right: &Value) -> Result<()> {
    if left.is_null() || right.is_null() {
        return Err(Error::internal("Comparison on NULL values"));
    }
    Ok(())
}

/// `left = right` (DuckDB's `ValueOperations::Equals`); comparing NULLs is an error.
pub fn values_equal(left: &Value, right: &Value) -> Result<bool> {
    reject_nulls(left, right)?;
    Ok(unified_order(left, right) == Some(Ordering::Equal))
}

/// `left > right` (DuckDB's `ValueOperations::GreaterThan`); comparing NULLs is an error.
pub fn values_greater_than(left: &Value, right: &Value) -> Result<bool> {
    reject_nulls(left, right)?;
    Ok(unified_order(left, right) == Some(Ordering::Greater))
}

/// `left IS NOT DISTINCT FROM right` (DuckDB's `ValueOperations::NotDistinctFrom`).
pub fn values_not_distinct(left: &Value, right: &Value) -> bool {
    match (left.is_null(), right.is_null()) {
        (true, true) => true,
        (false, false) => unified_order(left, right) == Some(Ordering::Equal),
        _ => false,
    }
}

/// DuckDB's `ValueOperations::DistinctGreaterThan`: NULL is greater than every value.
pub fn values_distinct_greater_than(left: &Value, right: &Value) -> bool {
    match (left.is_null(), right.is_null()) {
        (_, true) => false,
        (true, false) => true,
        (false, false) => unified_order(left, right) == Some(Ordering::Greater),
    }
}

/// DuckDB's `ApproxEqual` for floating point results: within 1% (plus 1e-8).
fn approx_equal(left: f64, right: f64, single: bool) -> bool {
    if left.is_nan() && right.is_nan() {
        return true;
    }
    if !left.is_finite() || !right.is_finite() {
        // Equal only as the same infinity.
        return left.is_infinite()
            && right.is_infinite()
            && left.is_sign_negative() == right.is_sign_negative();
    }
    if single {
        let epsilon = (f64::from(right as f32).abs() * 0.01 + 0.000_000_01) as f32;
        return (left as f32 - right as f32).abs() <= epsilon;
    }
    (left - right).abs() <= right.abs() * 0.01 + 0.000_000_01
}

/// Trailing spaces are ignored and NUL bytes compare as `\0` (DuckDB's `SanitizeValue`).
fn sanitize(text: &str) -> String {
    text.trim_end_matches([' ', '\t', '\n', '\r', '\x0b', '\x0c'])
        .replace('\0', "\\0")
}

/// Whether a query result matches an expected value (DuckDB's `Value::ValuesAreEqual`, used
/// by the test runner): floats compare approximately, strings ignoring trailing spaces,
/// everything else with SQL equality after casting `result_value` as needed.
pub fn values_are_equal(set: &CastFunctionSet, result_value: &Value, value: &Value) -> Result<bool> {
    if result_value.is_null() != value.is_null() {
        return Ok(false);
    }
    if value.is_null() {
        return Ok(true);
    }
    let cast = |target: &LogicalType| result_value.try_cast_as(set, target, false);
    match value.logical_type().id() {
        LogicalTypeId::Float => {
            let other = cast(&LogicalType::FLOAT)?;
            Ok(approx_equal(
                value.as_f64().unwrap_or_default(),
                other.as_f64().unwrap_or_default(),
                true,
            ))
        }
        LogicalTypeId::Double => {
            let other = cast(&LogicalType::DOUBLE)?;
            Ok(approx_equal(
                value.as_f64().unwrap_or_default(),
                other.as_f64().unwrap_or_default(),
                false,
            ))
        }
        LogicalTypeId::Varchar => {
            let other = cast(&LogicalType::VARCHAR)?;
            Ok(sanitize(other.as_str().unwrap_or_default()) == sanitize(value.as_str().unwrap_or_default()))
        }
        _ if matches!(
            result_value.logical_type().id(),
            LogicalTypeId::Float | LogicalTypeId::Double
        ) =>
        {
            values_are_equal(set, value, result_value)
        }
        _ => values_equal(value, result_value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comparisons_follow_duckdb() {
        assert!(values_equal(&Value::integer(1), &Value::bigint(1)).unwrap());
        assert!(values_greater_than(&Value::double(f64::NAN), &Value::double(1e300)).unwrap());
        assert!(values_equal(&Value::double(f64::NAN), &Value::double(f64::NAN)).unwrap());
        assert!(values_equal(&Value::double(-0.0), &Value::double(0.0)).unwrap());
        assert!(values_greater_than(&Value::varchar("b"), &Value::varchar("abc")).unwrap());
        assert!(values_equal(&Value::integer(1), &Value::null(LogicalType::INTEGER)).is_err());
        let short = Value::list(LogicalType::INTEGER, vec![Value::integer(1)]);
        let long = Value::list(LogicalType::INTEGER, vec![Value::integer(1), Value::integer(0)]);
        assert!(values_greater_than(&long, &short).unwrap());
        let with_null = Value::list(LogicalType::INTEGER, vec![Value::null(LogicalType::INTEGER)]);
        assert!(values_greater_than(&with_null, &short).unwrap());
        assert!(values_not_distinct(&with_null, &with_null.clone()));
        assert!(values_distinct_greater_than(
            &Value::null(LogicalType::INTEGER),
            &Value::integer(1)
        ));
        // Values that cannot be cast to the common type are unequal.
        assert!(!values_equal(&Value::varchar("abc"), &Value::integer(1)).unwrap());
    }

    #[test]
    fn result_checking_is_approximate() {
        let set = CastFunctionSet::new();
        assert!(values_are_equal(&set, &Value::varchar("1.005"), &Value::double(1.0)).unwrap());
        assert!(!values_are_equal(&set, &Value::varchar("1.1"), &Value::double(1.0)).unwrap());
        assert!(values_are_equal(&set, &Value::varchar("ab  "), &Value::varchar("ab")).unwrap());
        assert!(values_are_equal(&set, &Value::varchar("42"), &Value::integer(42)).unwrap());
    }
}
