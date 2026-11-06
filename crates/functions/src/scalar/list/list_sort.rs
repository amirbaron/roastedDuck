//! `list_sort`/`array_sort`, `list_reverse_sort`/`array_reverse_sort` and
//! `list_grade_up`/`array_grade_up`/`grade_up` (DuckDB's `list_sort.cpp`).

use std::any::Any;
use std::sync::Arc;

use rustdb_common::string_util::{candidates_message, top_n_jaro_winkler};
use rustdb_common::{Error, ErrorKind, Result};
use rustdb_function::{BindInput, FunctionData, FunctionState, ScalarFunction, ScalarFunctionSet};
use rustdb_row::sort::{NullOrder, OrderModifiers, OrderType, create_sort_keys};
use rustdb_types::kernels::scatter;
use rustdb_types::{DataChunk, ListEntry, LogicalType, LogicalTypeId, Vector};

use crate::scalar::executor::addressable;
use crate::scalar::nested::{ListInput, all_constant, list_child_type};

type BindResult = Result<Option<Arc<dyn FunctionData>>>;

/// How a list is sorted.
#[derive(Clone, Copy, Debug)]
struct SortBindData {
    order: OrderType,
    nulls: NullOrder,
    grade_up: bool,
}

impl FunctionData for SortBindData {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// DuckDB's `StringUtil::StringToEnum` failure.
fn enum_error(value: &str, name: &str, candidates: &[&str]) -> Error {
    let candidates: Vec<String> = candidates.iter().map(|&c| c.to_owned()).collect();
    let message = candidates_message(&top_n_jaro_winkler(&candidates, value), "Candidates");
    Error::new(
        ErrorKind::NotImplemented,
        format!("Enum value: unrecognized value \"{value}\" for enum \"{name}\"\n{message}"),
    )
}

/// The constant text of a sorting argument (DuckDB's `GetOrder`), uppercased.
fn order_text(input: &BindInput<'_>, index: usize) -> Result<String> {
    let Some(Some(value)) = input.constants.get(index) else {
        return Err(Error::invalid_input("Sorting order must be a constant"));
    };
    let text = if value.is_null() {
        "NULL".to_owned()
    } else {
        crate::values::as_text(value)?
    };
    Ok(text.to_uppercase())
}

/// `EnumUtil::FromString<OrderType>`; `None` is `ORDER_DEFAULT`.
fn parse_order(text: &str) -> Result<Option<OrderType>> {
    const NAMES: [&str; 7] = [
        "INVALID",
        "ORDER_DEFAULT",
        "DEFAULT",
        "ASCENDING",
        "ASC",
        "DESCENDING",
        "DESC",
    ];
    match text {
        "ORDER_DEFAULT" | "DEFAULT" => Ok(None),
        "ASCENDING" | "ASC" => Ok(Some(OrderType::Ascending)),
        "DESCENDING" | "DESC" => Ok(Some(OrderType::Descending)),
        _ => Err(enum_error(text, "OrderType", &NAMES)),
    }
}

/// `EnumUtil::FromString<OrderByNullType>`; `None` is `ORDER_DEFAULT`.
fn parse_null_order(text: &str) -> Result<Option<NullOrder>> {
    const NAMES: [&str; 7] = [
        "INVALID",
        "ORDER_DEFAULT",
        "DEFAULT",
        "NULLS FIRST",
        "NULLS_FIRST",
        "NULLS LAST",
        "NULLS_LAST",
    ];
    match text {
        "ORDER_DEFAULT" | "DEFAULT" => Ok(None),
        "NULLS FIRST" | "NULLS_FIRST" => Ok(Some(NullOrder::NullsFirst)),
        "NULLS LAST" | "NULLS_LAST" => Ok(Some(NullOrder::NullsLast)),
        _ => Err(enum_error(text, "OrderByNullType", &NAMES)),
    }
}

/// The list argument type, with ARRAY cast to LIST (`AddArrayCastToList`).
fn list_argument(ty: &LogicalType) -> Result<LogicalType> {
    if ty.id() == LogicalTypeId::Array {
        return Ok(LogicalType::list(list_child_type(ty)?));
    }
    Ok(ty.clone())
}

/// The upper-cased text of setting `name`, if the connection has settings.
fn setting_text(input: &BindInput<'_>, name: &str) -> Result<Option<String>> {
    let Some(settings) = input.client.settings() else {
        return Ok(None);
    };
    let value = settings.current_setting(name)?;
    Ok(value.as_str().map(str::to_uppercase))
}

/// `DBConfig::ResolveOrder`: the `default_order` setting for `ORDER_DEFAULT`.
fn resolve_order(input: &BindInput<'_>, order: Option<OrderType>) -> Result<OrderType> {
    if let Some(order) = order {
        return Ok(order);
    }
    Ok(match setting_text(input, "default_order")?.as_deref() {
        Some("DESC" | "DESCENDING") => OrderType::Descending,
        _ => OrderType::Ascending,
    })
}

/// `DBConfig::ResolveNullOrder`: the `default_null_order` setting for `ORDER_DEFAULT`.
fn resolve_null_order(
    input: &BindInput<'_>,
    order: OrderType,
    nulls: Option<NullOrder>,
) -> Result<NullOrder> {
    if let Some(nulls) = nulls {
        return Ok(nulls);
    }
    let ascending = order == OrderType::Ascending;
    Ok(match setting_text(input, "default_null_order")?.as_deref() {
        Some("NULLS_FIRST" | "NULLS FIRST") => NullOrder::NullsFirst,
        Some("NULLS_FIRST_ON_ASC_LAST_ON_DESC") if ascending => NullOrder::NullsFirst,
        Some("NULLS_LAST_ON_ASC_FIRST_ON_DESC") if !ascending => NullOrder::NullsFirst,
        _ => NullOrder::NullsLast,
    })
}

/// DuckDB's `ListNormalSortBind`/`ListReverseSortBind`/`ListGradeUpBind`: unspecified
/// orders come from the `default_order` and `default_null_order` settings.
fn bind_sort(reverse: bool, grade_up: bool) -> impl Fn(&mut ScalarFunction, &BindInput<'_>) -> BindResult {
    move |function, input| {
        let (order_index, null_index) = if reverse {
            (None, Some(1))
        } else {
            (Some(1), Some(2))
        };
        let mut order = None;
        let mut nulls = None;
        if let Some(index) = order_index.filter(|&index| index < input.argument_types.len()) {
            order = parse_order(&order_text(input, index)?)?;
        }
        if let Some(index) = null_index.filter(|&index| index < input.argument_types.len()) {
            nulls = parse_null_order(&order_text(input, index)?)?;
        }
        let mut order = resolve_order(input, order)?;
        if reverse {
            order = match order {
                OrderType::Ascending => OrderType::Descending,
                OrderType::Descending => OrderType::Ascending,
            };
        }
        let nulls = resolve_null_order(input, order, nulls)?;
        let ty = &input.argument_types[0];
        if ty.id() == LogicalTypeId::Unknown && !grade_up {
            function.arguments[0] = LogicalType::simple(LogicalTypeId::Unknown);
            function.return_type = LogicalType::SQLNULL;
        } else {
            let list = list_argument(ty)?;
            function.arguments[0] = list.clone();
            function.return_type = if grade_up {
                LogicalType::list(LogicalType::BIGINT)
            } else {
                list
            };
        }
        Ok(Some(Arc::new(SortBindData {
            order,
            nulls,
            grade_up,
        })))
    }
}

/// DuckDB's `ListSortFunction`: each list's elements are sorted by sort key (ties keep
/// their order); `list_grade_up` returns the 1-based source positions instead.
fn list_sort(args: &DataChunk, state: &mut FunctionState, result: &mut Vector) -> Result<()> {
    let data = *state
        .bind_data
        .as_ref()
        .and_then(|data| data.as_any().downcast_ref::<SortBindData>())
        .ok_or_else(|| Error::internal("list_sort without bind data"))?;
    if args.column(0).logical_type().id() == LogicalTypeId::SqlNull {
        *result = Vector::constant_null(result.logical_type().clone());
        return Ok(());
    }
    let count = args.len();
    let constant = all_constant(args.columns());
    let rows = if constant { 1 } else { count };
    let lists = addressable(args.column(0), count);
    let input = ListInput::new(&lists)?;
    let modifiers = [OrderModifiers::new(data.order, data.nulls)];
    let keys = create_sort_keys(&[input.child], &modifiers, input.child_size)?;
    let mut entries = Vec::with_capacity(rows);
    let mut nulls = Vec::new();
    let mut selection: Vec<Option<u32>> = Vec::new();
    let mut positions: Vec<i64> = Vec::new();
    for row in 0..rows {
        let Some(entry) = input.get(row) else {
            nulls.push(row);
            entries.push(ListEntry::new(selection.len(), 0));
            continue;
        };
        let (offset, length) = (entry.offset as usize, entry.length as usize);
        let mut children: Vec<usize> = (offset..offset + length).collect();
        children.sort_by(|&a, &b| keys.key(a).cmp(keys.key(b)));
        entries.push(ListEntry::new(selection.len(), length));
        for child in children {
            selection.push(Some(child as u32));
            positions.push((child - offset + 1) as i64);
        }
    }
    let size = selection.len();
    let child = if data.grade_up {
        let mut child = Vector::flat(LogicalType::BIGINT, size.max(1));
        child.reset_flat(size);
        child.values_mut::<i64>()[..size].copy_from_slice(&positions);
        child
    } else {
        let mut child = Vector::flat(list_child_type(result.logical_type())?, size.max(1));
        if size > 0 {
            let targets: Vec<u32> = (0..size as u32).collect();
            scatter(&input.child.select_rows(&selection), &mut child, &targets)?;
        }
        child
    };
    let mut output = Vector::flat(result.logical_type().clone(), rows.max(1));
    output.values_mut::<ListEntry>()[..rows].copy_from_slice(&entries);
    output.set_list_child(child, size);
    let validity = output.validity_mut();
    for row in nulls {
        validity.set_invalid(row, rows);
    }
    if constant {
        output.make_constant();
    }
    *result = output;
    Ok(())
}

fn sort_set(name: &str, reverse: bool, grade_up: bool) -> ScalarFunctionSet {
    let list = LogicalType::list(LogicalType::ANY);
    let mut set = ScalarFunctionSet::new(name);
    let arities = if reverse { 1..=2 } else { 1..=3 };
    for arity in arities {
        let mut arguments = vec![list.clone()];
        arguments.extend(std::iter::repeat_n(LogicalType::VARCHAR, arity - 1));
        set.add(
            ScalarFunction::new(name, arguments, list.clone(), list_sort)
                .with_bind(bind_sort(reverse, grade_up)),
        );
    }
    set
}

/// `list_sort`, `list_reverse_sort` and `list_grade_up`, with their aliases.
pub fn list_sort_functions() -> Vec<(ScalarFunctionSet, &'static [&'static str])> {
    vec![
        (sort_set("list_sort", false, false), &["array_sort"]),
        (
            sort_set("list_reverse_sort", true, false),
            &["array_reverse_sort"],
        ),
        (
            sort_set("list_grade_up", false, true),
            &["array_grade_up", "grade_up"],
        ),
    ]
}
