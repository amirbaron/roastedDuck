//! Helpers shared by the statement transforms (`transform/helpers/*.cpp` and a few
//! small functions from the statement files).

use std::collections::{BTreeSet, HashMap};

use rustdb_ast::{
    AggregateHandling, ExpressionType, OnCreateConflict, OnEntryNotFound, OrderByNode, OrderByNullType,
    OrderType, ParsedExpression, ParsedExpressionKind, SampleMethod, SampleOptions, SelectNode,
};
use rustdb_common::{ByteString, Error, Result};
use rustdb_function::cast::ValueCast;
use rustdb_pg_parser::nodes::{
    Alias, GroupingSet, GroupingSetKind, List, Node, OnCreateConflict as PgOnCreateConflict, RangeVar,
    SampleOptions as PgSampleOptions, SampleSize, SortBy, SortByDir, SortByNulls,
};
use rustdb_types::{LogicalType, Value};

use super::star::is_empty_star;
use super::{Transformer, cast_node, parser_error_at, required, string_list};
use crate::equality::{expression_hash, expressions_equal};

/// `SampleOptions::MAX_SAMPLE_ROWS`.
const MAX_SAMPLE_ROWS: u64 = 1_000_000_000;
/// The most grouping sets a GROUP BY may produce.
const MAX_GROUPING_SETS: usize = 65535;

/// A schema-qualified name (DuckDB's `QualifiedName`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct QualifiedName<K = ByteString> {
    pub(crate) catalog: K,
    pub(crate) schema: K,
    pub(crate) name: K,
}

/// `TransformQualifiedName`.
pub(crate) fn transform_qualified_name(root: &RangeVar) -> Result<QualifiedName> {
    let Some(name) = &root.relname else {
        return Err(Error::parser("Empty table name not supported"));
    };
    Ok(QualifiedName {
        catalog: root.catalogname.clone().unwrap_or_default(),
        schema: root.schemaname.clone().unwrap_or_default(),
        name: name.clone(),
    })
}

/// `TransformAlias`: the alias and its column names.
pub(crate) fn transform_alias(root: Option<&Alias>) -> Result<(ByteString, Vec<ByteString>)> {
    let Some(alias) = root else {
        return Ok((ByteString::new(), Vec::new()));
    };
    Ok((
        alias.aliasname.clone().unwrap_or_default(),
        string_list(&alias.colnames)?,
    ))
}

/// `TransformOnConflict` (of CREATE).
pub(crate) const fn transform_on_conflict(conflict: PgOnCreateConflict) -> OnCreateConflict {
    match conflict {
        PgOnCreateConflict::Error => OnCreateConflict::ErrorOnConflict,
        PgOnCreateConflict::Ignore => OnCreateConflict::IgnoreOnConflict,
        PgOnCreateConflict::Replace => OnCreateConflict::ReplaceOnConflict,
    }
}

/// `TransformOnEntryNotFound`.
pub(crate) const fn transform_on_entry_not_found(missing_ok: bool) -> OnEntryNotFound {
    if missing_ok {
        OnEntryNotFound::ReturnNull
    } else {
        OnEntryNotFound::ThrowException
    }
}

fn check_grouping_set_max(count: usize) -> Result<()> {
    if count > MAX_GROUPING_SETS {
        return Err(Error::parser(rustdb_common::byte_format!(
            "Maximum grouping set count of {MAX_GROUPING_SETS} exceeded"
        )));
    }
    Ok(())
}

fn check_grouping_set_cubes(current_count: usize, cube_count: usize) -> Result<()> {
    let mut combinations: usize = 1;
    for _ in 0..cube_count {
        combinations = combinations.saturating_mul(2);
        check_grouping_set_max(current_count.saturating_add(combinations))?;
    }
    Ok(())
}

type GroupingSetT = BTreeSet<u64>;

fn merge_grouping_set(result: &mut GroupingSetT, other: &GroupingSetT) -> Result<()> {
    check_grouping_set_max(result.len() + other.len())?;
    result.extend(other.iter().copied());
    Ok(())
}

fn add_cube_sets(
    current: &GroupingSetT,
    cube_sets: &[GroupingSetT],
    result_sets: &mut Vec<GroupingSetT>,
    start: usize,
) -> Result<()> {
    check_grouping_set_max(result_sets.len())?;
    result_sets.push(current.clone());
    for k in start..cube_sets.len() {
        let mut child = current.clone();
        merge_grouping_set(&mut child, &cube_sets[k])?;
        add_cube_sets(&child, cube_sets, result_sets, k + 1)?;
    }
    Ok(())
}

/// The GROUP BY expressions seen so far, for deduplication (`GroupingExpressionMap`).
#[derive(Default)]
struct GroupingExpressionMap {
    /// Indexes of the group expressions, by [`expression_hash`].
    by_hash: HashMap<u64, Vec<u64>>,
}

impl GroupingExpressionMap {
    fn find(&self, groups: &[ParsedExpression], expression: &ParsedExpression, hash: u64) -> Option<u64> {
        self.by_hash.get(&hash)?.iter().copied().find(|&index| {
            usize::try_from(index)
                .ok()
                .and_then(|index| groups.get(index))
                .is_some_and(|group| expressions_equal(group, expression))
        })
    }
}

/// `AddGroupByExpression`: `ROW(a, b)` groups by `a` and `b`; repeated expressions
/// share an index.
fn add_group_by_expression(
    expression: ParsedExpression,
    map: &mut GroupingExpressionMap,
    groups: &mut Vec<ParsedExpression>,
    result_set: &mut Vec<u64>,
) {
    if expression.expression_type == ExpressionType::Function
        && let ParsedExpressionKind::Function(function) = &expression.kind
        && function.function_name == "row"
    {
        let ParsedExpressionKind::Function(function) = expression.kind else {
            return;
        };
        for child in function.children {
            add_group_by_expression(child, map, groups, result_set);
        }
        return;
    }
    let hash = expression_hash(&expression);
    let index = if let Some(index) = map.find(groups, &expression, hash) {
        index
    } else {
        let index = groups.len() as u64;
        map.by_hash.entry(hash).or_default().push(index);
        groups.push(expression);
        index
    };
    result_set.push(index);
}

impl<'a> Transformer<'a> {
    /// `TransformOrderBy`.
    pub(crate) fn transform_order_by(
        &mut self,
        order: &'a List,
        result: &mut Vec<OrderByNode>,
    ) -> Result<()> {
        for node in order {
            let Node::SortBy(sort) = node else {
                return Err(Error::not_implemented(rustdb_common::byte_format!(
                    "ORDER BY list member type {}\n",
                    super::node_tag(node).0
                )));
            };
            let sort: &SortBy = sort;
            let order_type = match sort.sortby_dir {
                SortByDir::Default => OrderType::OrderDefault,
                SortByDir::Asc => OrderType::Ascending,
                SortByDir::Desc => OrderType::Descending,
                SortByDir::Using => return Err(Error::not_implemented("Unimplemented order by type")),
            };
            let null_order = match sort.sortby_nulls {
                SortByNulls::Default => OrderByNullType::OrderDefault,
                SortByNulls::First => OrderByNullType::NullsFirst,
                SortByNulls::Last => OrderByNullType::NullsLast,
            };
            let expression = self.transform_required(sort.node.as_ref(), "ORDER BY expression")?;
            result.push(OrderByNode {
                order_type,
                null_order,
                expression: Box::new(expression),
            });
        }
        Ok(())
    }

    /// `TransformSampleOptions`: `USING SAMPLE 10%`, `TABLESAMPLE reservoir(100 ROWS)`, ...
    pub(crate) fn transform_sample_options(
        &mut self,
        options: Option<&'a Node>,
    ) -> Result<Option<Box<SampleOptions>>> {
        let Some(options) = options else {
            return Ok(None);
        };
        let sample_options: &PgSampleOptions = cast_node(options)?;
        let sample_size: &SampleSize =
            cast_node(required(sample_options.sample_size.as_ref(), "sample size")?)?;
        let expression = self.transform_required(sample_size.sample_size.as_ref(), "sample size")?;
        let ParsedExpressionKind::Constant(constant) = &expression.kind else {
            return Err(parser_error_at(
                expression.query_location,
                "Only constants are supported in sample clause currently".to_owned(),
            ));
        };
        let value = &constant.value;
        let mut result = SampleOptions {
            sample_size: Value::default(),
            is_percentage: sample_size.is_percentage,
            method: SampleMethod::SystemSample,
            seed: None,
            repeatable: false,
        };
        if sample_size.is_percentage {
            let percentage = get_value_f64(value)?;
            if !(0.0..=100.0).contains(&percentage) {
                return Err(Error::parser(format!(
                    "Sample sample_size {percentage:.6} out of range, must be between 0 and 100"
                )));
            }
            result.sample_size = Value::double(percentage);
            result.method = SampleMethod::SystemSample;
        } else {
            let rows = get_value_i64(value)?;
            if rows < 0 || rows as u64 > MAX_SAMPLE_ROWS {
                return Err(Error::parser(rustdb_common::byte_format!(
                    "Sample rows {rows} out of range, must be between 0 and {MAX_SAMPLE_ROWS}"
                )));
            }
            result.sample_size = Value::bigint(rows);
            result.method = SampleMethod::ReservoirSample;
        }
        if let Some(method) = &sample_options.method {
            result.method = match method.to_ascii_lowercase().as_bytes() {
                b"system" => SampleMethod::SystemSample,
                b"bernoulli" => SampleMethod::BernoulliSample,
                b"reservoir" => SampleMethod::ReservoirSample,
                _ => {
                    return Err(Error::parser(rustdb_common::byte_format!(
                        "Unrecognized sampling method {method}, expected system, bernoulli or reservoir"
                    )));
                }
            };
        }
        if sample_options.has_seed && sample_options.seed >= 0 {
            result.seed = Some(sample_options.seed as u64);
            result.repeatable = true;
        }
        Ok(Some(Box::new(result)))
    }

    /// `TransformGroupBy`: GROUP BY items, combined as the cross product of their
    /// grouping sets.
    pub(crate) fn transform_group_by(&mut self, group: &'a List, select: &mut SelectNode) -> Result<()> {
        let mut map = GroupingExpressionMap::default();
        for node in group {
            let mut result_sets = Vec::new();
            self.transform_group_by_node(node, &mut map, select, &mut result_sets)?;
            check_grouping_set_max(result_sets.len())?;
            if select.group_sets.is_empty() {
                select.group_sets = result_sets;
            } else {
                let count = select.group_sets.len().saturating_mul(result_sets.len());
                check_grouping_set_max(count)?;
                let mut new_sets = Vec::with_capacity(count);
                for current in &select.group_sets {
                    for new_set in &result_sets {
                        let mut set = current.clone();
                        set.extend(new_set.iter().copied());
                        new_sets.push(set);
                    }
                }
                select.group_sets = new_sets;
            }
        }
        if select.group_expressions.len() == 1
            && select.group_sets.len() == 1
            && is_empty_star(&select.group_expressions[0])
        {
            select.group_expressions.clear();
            select.group_sets.clear();
            select.aggregate_handling = AggregateHandling::ForceAggregates;
        }
        Ok(())
    }

    fn transform_group_by_expression(
        &mut self,
        node: &'a Node,
        map: &mut GroupingExpressionMap,
        select: &mut SelectNode,
        indexes: &mut Vec<u64>,
    ) -> Result<()> {
        let expression = self.transform_expression(node)?;
        add_group_by_expression(expression, map, &mut select.group_expressions, indexes);
        Ok(())
    }

    fn transform_group_by_node(
        &mut self,
        node: &'a Node,
        map: &mut GroupingExpressionMap,
        select: &mut SelectNode,
        result_sets: &mut Vec<GroupingSetT>,
    ) -> Result<()> {
        let Node::GroupingSet(grouping_set) = node else {
            let mut indexes = Vec::new();
            self.transform_group_by_expression(node, map, select, &mut indexes)?;
            result_sets.push(indexes.into_iter().collect());
            return Ok(());
        };
        let grouping_set: &GroupingSet = grouping_set;
        match grouping_set.kind {
            GroupingSetKind::Empty => result_sets.push(GroupingSetT::new()),
            GroupingSetKind::All => select.aggregate_handling = AggregateHandling::ForceAggregates,
            GroupingSetKind::Sets => {
                for child in &grouping_set.content {
                    self.transform_group_by_node(child, map, select, result_sets)?;
                }
            }
            GroupingSetKind::Rollup => {
                let mut rollup_sets = Vec::new();
                for child in &grouping_set.content {
                    let mut indexes = Vec::new();
                    self.transform_group_by_expression(child, map, select, &mut indexes)?;
                    rollup_sets.push(indexes.into_iter().collect::<GroupingSetT>());
                }
                let mut current = GroupingSetT::new();
                result_sets.push(current.clone());
                for set in &rollup_sets {
                    merge_grouping_set(&mut current, set)?;
                    result_sets.push(current.clone());
                }
            }
            GroupingSetKind::Cube => {
                let mut cube_sets = Vec::new();
                for child in &grouping_set.content {
                    let mut indexes = Vec::new();
                    self.transform_group_by_expression(child, map, select, &mut indexes)?;
                    cube_sets.push(indexes.into_iter().collect::<GroupingSetT>());
                }
                check_grouping_set_cubes(result_sets.len(), cube_sets.len())?;
                add_cube_sets(&GroupingSetT::new(), &cube_sets, result_sets, 0)?;
            }
            GroupingSetKind::Simple => {
                return Err(Error::internal(rustdb_common::byte_format!(
                    "Unsupported GROUPING SET type {}",
                    grouping_set.kind as i32
                )));
            }
        }
        Ok(())
    }
}

/// `Value::GetValue<double>()`.
pub fn get_value_f64(value: &Value) -> Result<f64> {
    if value.is_null() {
        return Err(Error::internal(
            "Calling GetValueInternal on a value that is NULL",
        ));
    }
    let cast = value.default_cast_as(&LogicalType::DOUBLE)?;
    cast.as_f64()
        .ok_or_else(|| Error::internal("Value cast to DOUBLE is not a double"))
}

/// `Value::GetValue<int64_t>()`.
pub fn get_value_i64(value: &Value) -> Result<i64> {
    if value.is_null() {
        return Err(Error::internal(
            "Calling GetValueInternal on a value that is NULL",
        ));
    }
    let cast = value.default_cast_as(&LogicalType::BIGINT)?;
    cast.as_i128()
        .and_then(|v| i64::try_from(v).ok())
        .ok_or_else(|| Error::internal("Value cast to BIGINT is not an integer"))
}
