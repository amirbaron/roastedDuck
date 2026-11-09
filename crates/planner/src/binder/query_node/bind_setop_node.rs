//! UNION / EXCEPT / INTERSECT (DuckDB's `bind_setop_node.cpp` and `plan_setop.cpp`).

use roastedduck_ast::{DistinctModifier, QueryNode, QueryNodeKind, ResultModifier, SetOperationType};
use roastedduck_common::{ByteString, CaseInsensitiveMap, CaseInsensitiveSet, Error, Result};
use roastedduck_types::{LogicalType, LogicalTypeId, Value};

use super::modifiers::{OrderTargets, bind_modifiers, prepare_modifiers, visit_query_node};
use super::{BoundResultModifier, bind_node};
use crate::ColumnBinding;
use crate::binder::{BindScope, Binder, BoundChildInfo, BoundStatement, ExtraBoundInfo};
use crate::expression::Expression;
use crate::expression_binder::{SelectBindState, contains_type, exchange_type};
use crate::function_binder::add_cast_to_type;
use crate::generated::enums::LogicalOperatorType;
use crate::operator::{LogicalKind, LogicalOperator};

/// Records the operands' select-list aliases and expressions so that ORDER BY can refer to
/// them (DuckDB's `SetOpAliasGatherer`).
fn gather_aliases(bind_state: &mut SelectBindState, child: &BoundChildInfo, reorder: &[usize]) -> Result<()> {
    if let Some(setop_type) = child.extra_info.setop_type {
        return gather_setop_aliases(
            bind_state,
            setop_type,
            &child.names,
            &child.extra_info.bound_children,
            reorder,
        );
    }
    for (name, &index) in child.names.iter().zip(reorder) {
        if !bind_state.alias_map.contains_key(name) {
            bind_state.alias_map.insert(name.clone(), index);
        }
    }
    for (expr, &index) in child.extra_info.original_expressions.iter().zip(reorder) {
        match bind_state.projection_map.get(expr).copied() {
            Some(existing) if existing != index => bind_state.projection_map.insert(expr, usize::MAX),
            Some(_) => {}
            None => bind_state.projection_map.insert(expr, index),
        }
    }
    Ok(())
}

fn gather_setop_aliases(
    bind_state: &mut SelectBindState,
    setop_type: SetOperationType,
    names: &[ByteString],
    children: &[BoundChildInfo],
    reorder: &[usize],
) -> Result<()> {
    if setop_type == SetOperationType::UnionByName {
        let mut reorder_map = CaseInsensitiveMap::<usize, ByteString>::default();
        for (name, &index) in names.iter().zip(reorder) {
            reorder_map.insert(name.clone(), index);
        }
        for child in children {
            let child_reorder = child
                .names
                .iter()
                .map(|name| {
                    reorder_map.get(name).copied().ok_or_else(|| {
                        Error::internal("SetOp - Column name not found in reorder_map in UNION BY NAME")
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            gather_aliases(bind_state, child, &child_reorder)?;
        }
    } else {
        for child in children {
            gather_aliases(bind_state, child, reorder)?;
        }
    }
    Ok(())
}

/// The binders of all operands, nested set operations included (DuckDB's `GatherSetOpBinders`).
fn gather_binders(binders: &[Binder], children: &[BoundChildInfo], result: &mut Vec<Binder>) {
    result.extend(binders.iter().cloned());
    for child in children {
        gather_binders(
            &child.extra_info.child_binders,
            &child.extra_info.bound_children,
            result,
        );
    }
}

/// The result names and types of UNION BY NAME, with projections reordering the operands
/// (DuckDB's `BuildUnionByNameInfo`).
fn build_union_by_name_info(
    binder: &Binder,
    children: &mut [BoundStatement],
) -> Result<(Vec<ByteString>, Vec<LogicalType>)> {
    let mut names: Vec<ByteString> = Vec::new();
    let mut global = CaseInsensitiveSet::<ByteString>::default();
    let mut name_maps = Vec::with_capacity(children.len());
    for child in children.iter() {
        let mut map = CaseInsensitiveMap::<usize, ByteString>::default();
        for (i, name) in child.names.iter().enumerate() {
            if map.contains_key(name) {
                return Err(Error::new_bytes(roastedduck_common::ErrorKind::Binder, roastedduck_common::byte_format!(
                    "UNION (ALL) BY NAME operation doesn't support duplicate names in the SELECT list - the name \
                     \"{name}\" occurs multiple times"
                ).into_bytes()));
            }
            if global.insert(name.clone()) {
                names.push(name.clone());
            }
            map.insert(name.clone(), i);
        }
        name_maps.push(map);
    }
    let mut need_reorder = false;
    let mut types = Vec::with_capacity(names.len());
    for (i, name) in names.iter().enumerate() {
        let mut result_type = LogicalType::INVALID;
        for (child, map) in children.iter().zip(&name_maps) {
            match map.get(name) {
                None => need_reorder = true,
                Some(&index) => {
                    let child_type = &child.types[index];
                    result_type = if result_type.id() == LogicalTypeId::Invalid {
                        child_type.clone()
                    } else {
                        LogicalType::force_max_logical_type(&result_type, child_type)
                    };
                    if i != index {
                        need_reorder = true;
                    }
                }
            }
        }
        if !binder.can_contain_nulls && contains_type(&result_type, LogicalTypeId::SqlNull) {
            result_type = exchange_type(&result_type, LogicalTypeId::SqlNull, &LogicalType::INTEGER);
        }
        types.push(result_type);
    }
    if !need_reorder {
        return Ok((names, types));
    }
    for (child, map) in children.iter_mut().zip(&name_maps) {
        let root_index = child.plan.root_index()?;
        let expressions: Vec<Expression> = names
            .iter()
            .zip(&types)
            .map(|(name, ty)| match map.get(name) {
                None => Expression::constant(Value::null(ty.clone())),
                Some(&index) => Expression::column_ref(
                    "",
                    child.types[index].clone(),
                    ColumnBinding::new(root_index, index as u64),
                    0,
                ),
            })
            .collect();
        child.types = expressions.iter().map(|e| e.return_type.clone()).collect();
        let plan = std::mem::replace(&mut child.plan, LogicalOperator::new(LogicalKind::CrossProduct));
        child.plan = LogicalOperator::projection(binder.generate_table_index(), expressions, plan);
    }
    Ok((names, types))
}

/// Casts an operand's output to the result types (DuckDB's `CastLogicalOperatorToTypes`).
/// A projection of plain column references over a table function scan whose
/// function can read the target types directly: the scan, reading them (DuckDB's
/// `type_pushdown` in `CastLogicalOperatorToTypes`).
fn try_type_pushdown(op: &mut LogicalOperator, target: &[LogicalType]) -> Result<Option<LogicalOperator>> {
    if op.children.len() != 1 {
        return Ok(None);
    }
    let LogicalKind::Get(get) = &op.children[0].kind else {
        return Ok(None);
    };
    let (Some(pushdown), Some(bind_data)) = (get.function.type_pushdown.clone(), get.bind_data.clone())
    else {
        return Ok(None);
    };
    let column_ids = match &get.shared_column_ids {
        Some(shared) => shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone(),
        None => get.column_ids.clone(),
    };
    let mut new_types: Vec<(usize, LogicalType)> = Vec::new();
    for (expression, ty) in op.expressions.iter().zip(target) {
        let Some(column) = expression.as_column_ref() else {
            return Ok(None);
        };
        let Some(column_id) = column_ids.get(column.binding.column_index as usize) else {
            return Ok(None);
        };
        let Ok(column_id) = usize::try_from(column_id.index) else {
            return Ok(None);
        };
        if new_types.iter().any(|(existing, _)| *existing == column_id) {
            return Ok(None);
        }
        new_types.push((column_id, ty.clone()));
    }
    let new_bind_data = pushdown(bind_data.as_ref(), &new_types)?;
    let mut child = op.children.remove(0);
    if let LogicalKind::Get(get) = &mut child.kind {
        get.bind_data = Some(new_bind_data);
        for (column, ty) in new_types {
            if let Some(slot) = get.returned_types.get_mut(column) {
                *slot = ty;
            }
        }
    }
    Ok(Some(child))
}

pub(crate) fn cast_operator_to_types(
    binder: &Binder,
    source: &[LogicalType],
    target: &[LogicalType],
    mut op: LogicalOperator,
) -> Result<LogicalOperator> {
    if source == target {
        return Ok(op);
    }
    let context = binder.context.clone();
    if let LogicalKind::Projection { .. } = op.kind {
        if let Some(get) = try_type_pushdown(&mut op, target)? {
            return Ok(get);
        }
        for (i, (source_type, target_type)) in source.iter().zip(target).enumerate() {
            if source_type != target_type {
                let expression =
                    std::mem::replace(&mut op.expressions[i], Expression::constant(Value::default()));
                let alias = expression.alias.clone();
                op.expressions[i] =
                    add_cast_to_type(&context, expression, target_type, false)?.with_alias(alias);
            }
        }
        return Ok(op);
    }
    let bindings = op.column_bindings();
    let mut select_list = Vec::with_capacity(target.len());
    for ((source_type, target_type), binding) in source.iter().zip(target).zip(bindings) {
        let mut expression = Expression::column_ref("", source_type.clone(), binding, 0);
        if source_type != target_type {
            expression = add_cast_to_type(&context, expression, target_type, false)?;
        }
        select_list.push(expression);
    }
    Ok(LogicalOperator::projection(
        binder.generate_table_index(),
        select_list,
        op,
    ))
}

/// DuckDB's `Binder::BindNode(SetOperationNode &)` and `CreatePlan(BoundSetOperationNode &)`.
pub fn bind_set_operation(scope: &mut BindScope<'_>, node: &mut QueryNode) -> Result<BoundStatement> {
    let QueryNodeKind::SetOperation(setop) = &mut node.kind else {
        return Err(Error::internal("expected a set operation"));
    };
    let setop_type = setop.setop_type;
    let mut setop_all = setop.setop_all;
    let setop_index = scope.generate_table_index();
    if setop.children.len() < 2 {
        return Err(Error::internal("Set Operations must have at least 2 children"));
    }
    if setop.children.len() != 2
        && !matches!(
            setop_type,
            SetOperationType::Union | SetOperationType::UnionByName
        )
    {
        return Err(Error::internal(
            "Set Operation type must have exactly 2 children - except for UNION/UNION_BY_NAME",
        ));
    }
    let mut bound_children = Vec::with_capacity(setop.children.len());
    let mut child_binders = Vec::with_capacity(setop.children.len());
    for child in &mut setop.children {
        let mut child_binder = scope.binder.child()?;
        child_binder.can_contain_nulls = true;
        let bound = {
            let mut child_scope = BindScope {
                binder: &mut child_binder,
                outer: Some(&mut *scope),
            };
            bind_node(&mut child_scope, child)?
        };
        scope.binder.move_correlated_expressions(&mut child_binder);
        bound_children.push(bound);
        child_binders.push(child_binder);
    }
    let (names, types) = if setop_type == SetOperationType::UnionByName {
        build_union_by_name_info(scope.binder, &mut bound_children)?
    } else {
        let names = bound_children[0].names.clone();
        let column_count = bound_children[0].types.len();
        if bound_children
            .iter()
            .any(|child| child.types.len() != column_count)
        {
            return Err(Error::binder(
                "Set operations can only apply to expressions with the same number of result columns",
            ));
        }
        let mut types = Vec::with_capacity(column_count);
        for i in 0..column_count {
            let mut result_type = bound_children[0].types[i].clone();
            for child in &bound_children[1..] {
                result_type = LogicalType::force_max_logical_type(&result_type, &child.types[i]);
            }
            if !scope.can_contain_nulls && contains_type(&result_type, LogicalTypeId::SqlNull) {
                result_type = exchange_type(&result_type, LogicalTypeId::SqlNull, &LogicalType::INTEGER);
            }
            types.push(result_type);
        }
        (names, types)
    };
    let mut modifiers = std::mem::take(&mut node.modifiers);
    if !setop_all {
        modifiers.insert(
            0,
            ResultModifier::Distinct(DistinctModifier {
                distinct_on_targets: Vec::new(),
            }),
        );
        if matches!(
            setop_type,
            SetOperationType::Union | SetOperationType::UnionByName
        ) {
            setop_all = true;
        }
    }
    let child_infos: Vec<BoundChildInfo> = bound_children
        .iter()
        .map(BoundChildInfo::from_statement)
        .collect();
    let mut bind_state = SelectBindState::default();
    let mut bound_modifiers: Vec<BoundResultModifier> = Vec::new();
    if !modifiers.is_empty() {
        let mut binders = Vec::new();
        gather_binders(&child_binders, &child_infos, &mut binders);
        let reorder: Vec<usize> = (0..names.len()).collect();
        gather_setop_aliases(&mut bind_state, setop_type, &names, &child_infos, &reorder)?;
        prepare_modifiers(
            scope,
            &mut modifiers,
            OrderTargets::SetOperation(&binders),
            &mut bind_state,
            names.len(),
            &mut bound_modifiers,
        )?;
    }
    let context = scope.binder.context.clone();
    bind_modifiers(
        &context,
        &mut bound_modifiers,
        setop_index,
        &names,
        &types,
        &bind_state,
    )?;

    // Plan: cast every operand to the result types.
    let operator_type = match setop_type {
        SetOperationType::Union | SetOperationType::UnionByName => LogicalOperatorType::LogicalUnion,
        SetOperationType::Except => LogicalOperatorType::LogicalExcept,
        SetOperationType::Intersect => LogicalOperatorType::LogicalIntersect,
        SetOperationType::None => {
            return Err(Error::internal(
                "Unsupported logical operator type for set-operation",
            ));
        }
    };
    let mut plans = Vec::with_capacity(bound_children.len());
    for (child, child_binder) in bound_children.into_iter().zip(&child_binders) {
        plans.push(cast_operator_to_types(
            scope.binder,
            &child.types,
            &types,
            child.plan,
        )?);
        if child_binder.has_unplanned_dependent_joins {
            scope.binder.has_unplanned_dependent_joins = true;
        }
    }
    let root = LogicalOperator::new(LogicalKind::SetOperation {
        operator_type,
        table_index: setop_index,
        column_count: types.len() as u64,
        setop_all,
        allow_out_of_order: true,
    })
    .with_children(plans);
    let plan = visit_query_node(bound_modifiers, root);
    let mut result = BoundStatement::new(plan, names, types);
    result.extra_info = ExtraBoundInfo {
        setop_type: Some(setop_type),
        child_binders,
        bound_children: child_infos,
        original_expressions: Vec::new(),
    };
    Ok(result)
}
