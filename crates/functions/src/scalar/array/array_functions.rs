//! `array_distance`, `array_inner_product`, `array_negative_inner_product`,
//! `array_cosine_similarity`, `array_cosine_distance`, `array_cross_product` (DuckDB's
//! `array_functions.cpp`) and the `list_*` equivalents (`list_distance.cpp`).

use rustdb_common::{Error, Result};
use rustdb_function::{BindInput, FunctionData, ScalarFunction, ScalarFunctionSet};
use rustdb_types::{DataChunk, ListEntry, LogicalType, LogicalTypeId, Vector};

use super::kernels::{Float, Fold, cross_product};
use crate::scalar::executor::addressable;
use crate::scalar::nested::ListInput;

type BindResult = Result<Option<std::sync::Arc<dyn FunctionData>>>;

fn null_argument(name: &str, side: &str) -> Error {
    Error::invalid_input(format!("{name}: {side} argument can not contain NULL values"))
}

/// The values of a flat child vector.
fn values<T: Float>(child: &Vector, size: usize) -> Vec<T> {
    let unified = child.unified::<T>();
    (0..size).map(|row| unified.get(row).unwrap_or(T::ZERO)).collect()
}

/// DuckDB's `ArrayGenericFold`.
fn array_fold<T: Float>(args: &DataChunk, result: &mut Vector, name: &str, fold: Fold) -> Result<()> {
    let count = args.len();
    let left = args.column(0).flattened(count);
    let right = args.column(1).flattened(count);
    let size = left
        .logical_type()
        .array_child()
        .map_or(0, |(_, size)| size as usize);
    let (Some(left_child), Some(right_child)) = (left.array_child(), right.array_child()) else {
        return Err(Error::internal("array function on a non-array vector"));
    };
    let left_values = values::<T>(left_child, count * size);
    let right_values = values::<T>(right_child, count * size);
    result.reset_flat(count);
    let mut nulls = Vec::new();
    for row in 0..count {
        if left.is_null(row) || right.is_null(row) {
            nulls.push(row);
            continue;
        }
        let range = row * size..(row + 1) * size;
        if range.clone().any(|index| left_child.is_null(index)) {
            return Err(null_argument(name, "left"));
        }
        if range.clone().any(|index| right_child.is_null(index)) {
            return Err(null_argument(name, "right"));
        }
        result.values_mut::<T>()[row] = fold.apply(&left_values[range.clone()], &right_values[range]);
    }
    let validity = result.validity_mut();
    for row in nulls {
        validity.set_invalid(row, count);
    }
    if count == 1 {
        result.make_constant();
    }
    Ok(())
}

/// DuckDB's `ArrayGenericBinaryBind`: both arguments become arrays of a common float type.
fn bind_array_binary(function: &mut ScalarFunction, input: &BindInput<'_>) -> BindResult {
    let (left, right) = (&input.argument_types[0], &input.argument_types[1]);
    let unknown = |ty: &LogicalType| ty.id() == LogicalTypeId::Unknown;
    if unknown(left) && unknown(right) {
        function.arguments = vec![right.clone(), left.clone()];
        function.return_type = LogicalType::simple(LogicalTypeId::Unknown);
        return Ok(None);
    }
    let left = if unknown(left) { right.clone() } else { left.clone() };
    let right = if unknown(right) { left.clone() } else { right.clone() };
    let name = function.name.clone();
    let (Some((left_child, left_size)), Some((right_child, right_size))) = (left.array_child(), right.array_child())
    else {
        return Err(Error::invalid_input(format!(
            "{name}: Arguments must be arrays of FLOAT or DOUBLE"
        )));
    };
    if left_size != right_size {
        return Err(Error::binder(format!("{name}: Array arguments must be of the same size")));
    }
    let common = LogicalType::try_max_logical_type(left_child, right_child).ok_or_else(|| {
        Error::binder(format!(
            "{name}: Cannot infer common element type (left = '{left_child}', right = '{right_child}')"
        ))
    })?;
    if !matches!(common.id(), LogicalTypeId::Float | LogicalTypeId::Double) {
        return Err(Error::binder(format!(
            "{name}: Arguments must be arrays of FLOAT or DOUBLE"
        )));
    }
    function.arguments = vec![
        LogicalType::array(common.clone(), left_size),
        LogicalType::array(common, right_size),
    ];
    Ok(None)
}

fn array_fold_set(name: &'static str, fold: Fold) -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new(name);
    for ty in [LogicalType::FLOAT, LogicalType::DOUBLE] {
        let array = LogicalType::array(ty.clone(), 0);
        let float = ty.id() == LogicalTypeId::Float;
        set.add(
            ScalarFunction::new(name, vec![array.clone(), array], ty, move |args, _, result| {
                if float {
                    array_fold::<f32>(args, result, name, fold)
                } else {
                    array_fold::<f64>(args, result, name, fold)
                }
            })
            .with_bind(bind_array_binary),
        );
    }
    set
}

/// DuckDB's `ArrayFixedCombine` with the cross product.
fn array_cross<T: Float>(args: &DataChunk, result: &mut Vector) -> Result<()> {
    const NAME: &str = "array_cross_product";
    let count = args.len();
    let left = args.column(0).flattened(count);
    let right = args.column(1).flattened(count);
    let (Some(left_child), Some(right_child)) = (left.array_child(), right.array_child()) else {
        return Err(Error::internal("array function on a non-array vector"));
    };
    let left_values = values::<T>(left_child, count * 3);
    let right_values = values::<T>(right_child, count * 3);
    let element = result
        .logical_type()
        .array_child()
        .map(|(child, _)| child.clone())
        .ok_or_else(|| Error::internal("array_cross_product result is not an ARRAY"))?;
    let mut child = Vector::flat(element, (count * 3).max(1));
    child.reset_flat(count * 3);
    let mut nulls = Vec::new();
    for row in 0..count {
        if left.is_null(row) || right.is_null(row) {
            nulls.push(row);
            continue;
        }
        let range = row * 3..row * 3 + 3;
        if range.clone().any(|index| left_child.is_null(index)) {
            return Err(null_argument(NAME, "left"));
        }
        if range.clone().any(|index| right_child.is_null(index)) {
            return Err(null_argument(NAME, "right"));
        }
        let product = cross_product(&left_values[range.clone()], &right_values[range.clone()]);
        child.values_mut::<T>()[range].copy_from_slice(&product);
    }
    let mut output = Vector::flat(result.logical_type().clone(), count.max(1));
    output.set_array_child(child);
    let validity = output.validity_mut();
    for row in nulls {
        validity.set_invalid(row, count);
    }
    if count == 1 {
        output.make_constant();
    }
    *result = output;
    Ok(())
}

fn array_cross_product_set() -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new("array_cross_product");
    let float = LogicalType::array(LogicalType::FLOAT, 3);
    let double = LogicalType::array(LogicalType::DOUBLE, 3);
    set.add(ScalarFunction::new(
        "array_cross_product",
        vec![float.clone(), float.clone()],
        float,
        |args, _, result| array_cross::<f32>(args, result),
    ));
    set.add(ScalarFunction::new(
        "array_cross_product",
        vec![double.clone(), double.clone()],
        double,
        |args, _, result| array_cross::<f64>(args, result),
    ));
    set
}

/// DuckDB's `ListGenericFold`.
fn list_fold<T: Float>(args: &DataChunk, result: &mut Vector, name: &str, fold: Fold) -> Result<()> {
    let count = args.len();
    let constant = args.columns().iter().all(Vector::is_constant);
    let rows = if constant { 1 } else { count };
    let left_lists = addressable(args.column(0), count);
    let right_lists = addressable(args.column(1), count);
    let left = ListInput::new(&left_lists)?;
    let right = ListInput::new(&right_lists)?;
    if (0..left.child_size).any(|index| left.child.is_null(index)) {
        return Err(null_argument(name, "left"));
    }
    if (0..right.child_size).any(|index| right.child.is_null(index)) {
        return Err(null_argument(name, "right"));
    }
    let left_values = values::<T>(left.child, left.child_size);
    let right_values = values::<T>(right.child, right.child_size);
    result.reset_flat(rows);
    let mut nulls = Vec::new();
    for row in 0..rows {
        let (Some(l), Some(r)) = (left.get(row), right.get(row)) else {
            nulls.push(row);
            continue;
        };
        if l.length != r.length {
            return Err(Error::invalid_input(format!(
                "{name}: list dimensions must be equal, got left length '{}' and right length '{}'",
                l.length, r.length
            )));
        }
        if !fold.allows_empty() && l.length == 0 {
            nulls.push(row);
            continue;
        }
        let slice = |values: &[T], entry: ListEntry| values[entry.offset as usize..(entry.offset + entry.length) as usize].to_vec();
        result.values_mut::<T>()[row] = fold.apply(&slice(&left_values, l), &slice(&right_values, r));
    }
    let validity = result.validity_mut();
    for row in nulls {
        validity.set_invalid(row, rows);
    }
    if constant {
        result.make_constant();
    }
    Ok(())
}

fn list_fold_set(name: &'static str, fold: Fold) -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new(name);
    for ty in [LogicalType::FLOAT, LogicalType::DOUBLE] {
        let list = LogicalType::list(ty.clone());
        let float = ty.id() == LogicalTypeId::Float;
        set.add(ScalarFunction::new(name, vec![list.clone(), list], ty, move |args, _, result| {
            if float {
                list_fold::<f32>(args, result, name, fold)
            } else {
                list_fold::<f64>(args, result, name, fold)
            }
        }));
    }
    set
}

/// Every vector distance function set with its aliases.
pub fn distance_functions() -> Vec<(ScalarFunctionSet, &'static [&'static str])> {
    vec![
        (array_cross_product_set(), &[]),
        (array_fold_set("array_cosine_similarity", Fold::CosineSimilarity), &[]),
        (array_fold_set("array_cosine_distance", Fold::CosineDistance), &[]),
        (array_fold_set("array_distance", Fold::Distance), &[]),
        (array_fold_set("array_inner_product", Fold::InnerProduct), &["array_dot_product"]),
        (
            array_fold_set("array_negative_inner_product", Fold::NegativeInnerProduct),
            &["array_negative_dot_product"],
        ),
        (list_fold_set("list_cosine_distance", Fold::CosineDistance), &["<=>"]),
        (list_fold_set("list_cosine_similarity", Fold::CosineSimilarity), &[]),
        (list_fold_set("list_distance", Fold::Distance), &["<->"]),
        (list_fold_set("list_inner_product", Fold::InnerProduct), &["list_dot_product"]),
        (
            list_fold_set("list_negative_inner_product", Fold::NegativeInnerProduct),
            &["list_negative_dot_product"],
        ),
    ]
}
