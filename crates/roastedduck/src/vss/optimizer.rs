//! Pinned VSS expression/scan/top-k matching with genuine HNSW candidate scans.

use std::sync::Arc;

use roastedduck_ast::OrderType;
use roastedduck_catalog::{EntryKind, TableEntry};
use roastedduck_common::{Error, Result, STANDARD_VECTOR_SIZE};
use roastedduck_optimizer::{Optimizer, OptimizerRule};
use roastedduck_planner::operator::LogicalGet;
use roastedduck_planner::{Expression, ExpressionKind, LogicalKind, LogicalOperator};
use roastedduck_storage::index::IndexKeyExpressions;
use roastedduck_storage::index::hnsw::HnswIndex;
use roastedduck_types::LogicalType;

use super::scan::{ScanData, scan_function};

fn normalize_index_expression(expression: &mut Expression, get: &LogicalGet, table: &TableEntry) -> bool {
    if let ExpressionKind::ColumnRef(column) = &expression.kind {
        if column.binding.table_index != get.table_index || column.depth != 0 {
            return false;
        }
        let Some(column) = get.column_ids.get(column.binding.column_index as usize) else {
            return false;
        };
        if column.is_row_id_column() || !column.child_indexes.is_empty() {
            return false;
        }
        let Some(physical) = table.logical_to_physical(column.index as usize) else {
            return false;
        };
        *expression = Expression::reference(
            expression.alias.clone(),
            expression.return_type.clone(),
            physical as u64,
        );
        return true;
    }
    let mut valid = true;
    expression.for_each_child_mut(|child| valid = normalize_index_expression(child, get, table) && valid);
    valid
}

pub(super) fn matches_index_expression(
    expression: &Expression,
    get: &LogicalGet,
    table: &TableEntry,
    index: &HnswIndex,
) -> bool {
    let mut normalized = expression.clone();
    if !normalize_index_expression(&mut normalized, get, table) {
        return false;
    }
    match &index.spec().expressions {
        IndexKeyExpressions::Columns(_) => matches!(normalized.kind, ExpressionKind::Reference(reference)
            if index.spec().column_ids.len() == 1 && reference.index == index.spec().column_ids[0] as u64),
        IndexKeyExpressions::Custom(executor) => executor.matches_expression(&normalized),
    }
}

fn metric_matches(function: &[u8], metric: &str) -> bool {
    match metric {
        "l2sq" => matches!(function, b"array_distance" | b"<->"),
        "cosine" => matches!(function, b"array_cosine_distance" | b"<=>"),
        "ip" => matches!(function, b"array_negative_inner_product" | b"<#>"),
        _ => false,
    }
}

fn eligible_get(get: &LogicalGet) -> bool {
    get.function.name.as_ref() == "seq_scan"
        && get.table.is_some()
        && get.sample_options.is_none()
        && !get
            .dynamic_filters
            .as_ref()
            .is_some_and(|filters| filters.has_filters())
        && get
            .column_ids
            .iter()
            .all(|column| column.child_indexes.is_empty())
}

fn match_scan(
    optimizer: &Optimizer,
    get: &LogicalGet,
    expression: &Expression,
    limit: usize,
) -> Result<Option<ScanData>> {
    if !eligible_get(get) {
        return Ok(None);
    }
    let Some(entry) = get.table.as_ref() else {
        return Ok(None);
    };
    let EntryKind::Table(table) = &entry.kind else {
        return Ok(None);
    };
    let Some(storage) = table.storage.as_ref() else {
        return Ok(None);
    };
    let ExpressionKind::Function(function) = &expression.kind else {
        return Ok(None);
    };
    if function.children.len() != 2 {
        return Ok(None);
    }
    for index in storage.indexes().entries() {
        let state = index.lock();
        let Some(graph) = state
            .index
            .as_secondary()
            .and_then(|index| index.as_any().downcast_ref::<HnswIndex>())
        else {
            continue;
        };
        if !metric_matches(&function.function.name, graph.metric()) {
            continue;
        }
        for (constant, key) in [(0, 1), (1, 0)] {
            let constant = &function.children[constant];
            let key = &function.children[key];
            if constant.return_type != LogicalType::array(LogicalType::FLOAT, graph.dimensions() as u32)
                || key.return_type != constant.return_type
                || !matches_index_expression(key, get, table, graph)
            {
                continue;
            }
            let ExpressionKind::Constant(constant) = &constant.kind else {
                continue;
            };
            if constant.value.is_null() {
                continue;
            }
            let Some(children) = constant.value.children() else {
                continue;
            };
            let query = children
                .iter()
                .map(|value| {
                    if value.is_null() {
                        return Err(Error::internal(
                            "Calling GetValueInternal on a value that is NULL",
                        ));
                    }
                    value
                        .as_f32()
                        .ok_or_else(|| Error::internal("HNSW FLOAT query contains a non-FLOAT value"))
                })
                .collect::<Result<Vec<f32>>>()?;
            if query.iter().any(|value| !value.is_finite()) {
                continue;
            }
            return Ok(Some(ScanData {
                table_entry: Arc::clone(entry),
                table: Arc::clone(storage),
                index: Arc::clone(&index),
                query,
                limit,
                client: Arc::clone(&optimizer.context().function_client),
                filters: None,
            }));
        }
    }
    Ok(None)
}

fn install_scan(get: &mut LogicalGet, data: ScanData) {
    get.function = scan_function();
    get.bind_data = Some(Arc::new(data));
    // The base-table entry is retained by ScanData. Clearing this discriminator
    // makes physical planning select the actual table-function scan, not seq_scan.
    get.table = None;
}

#[derive(Debug)]
struct ExpressionRule;

fn rewrite_expression(optimizer: &Optimizer, expression: &mut Expression) -> Result<()> {
    expression.try_for_each_child_mut(|child| rewrite_expression(optimizer, child))?;
    let ExpressionKind::Function(subtract) = &expression.kind else {
        return Ok(());
    };
    if subtract.function.name.as_bytes() != b"-"
        || expression.return_type != LogicalType::FLOAT
        || subtract.children.len() != 2
    {
        return Ok(());
    }
    let ExpressionKind::Constant(one) = &subtract.children[0].kind else {
        return Ok(());
    };
    if one.value.as_f32() != Some(1.0) {
        return Ok(());
    }
    let ExpressionKind::Function(similarity) = &subtract.children[1].kind else {
        return Ok(());
    };
    if similarity.function.name.as_bytes() != b"array_cosine_similarity" || similarity.children.len() != 2 {
        return Ok(());
    }
    let mut distance =
        optimizer.bind_scalar_function("array_cosine_distance", similarity.children.clone())?;
    distance.alias = expression.alias.clone();
    *expression = distance;
    Ok(())
}

impl OptimizerRule for ExpressionRule {
    fn name(&self) -> &'static str {
        "vss.expression"
    }
    fn post_optimize(&self, optimizer: &mut Optimizer, mut plan: LogicalOperator) -> Result<LogicalOperator> {
        fn visit(optimizer: &Optimizer, plan: &mut LogicalOperator) -> Result<()> {
            plan.try_for_each_expression_mut(|expression| rewrite_expression(optimizer, expression))?;
            for child in &mut plan.children {
                visit(optimizer, child)?;
            }
            Ok(())
        }
        visit(optimizer, &mut plan)?;
        Ok(plan)
    }
}

#[derive(Debug)]
struct ScanRule;

fn rewrite_scan(optimizer: &Optimizer, plan: &mut LogicalOperator) -> Result<()> {
    let mut rewritten = false;
    if let LogicalKind::TopN { orders, limit, .. } = &plan.kind
        && orders.len() == 1
        && orders[0].order_type == OrderType::Ascending
        && let ExpressionKind::ColumnRef(order) = &orders[0].expression.kind
        && plan.children.len() == 1
    {
        let requested = usize::try_from(*limit).ok();
        let projection = &mut plan.children[0];
        if let LogicalKind::Projection { table_index } = projection.kind
            && order.binding.table_index == table_index
            && projection.children.len() == 1
            && let Some(expression) = projection.expressions.get(order.binding.column_index as usize)
            && let LogicalKind::Get(get) = &mut projection.children[0].kind
            && let Some(limit) = requested.filter(|limit| *limit > 0)
            && let Some(data) = match_scan(optimizer, get, expression, limit)?
        {
            install_scan(get, data);
            projection.children[0].estimated_cardinality = Some(limit as u64);
            rewritten = true;
            // Keep TopN for SQL score evaluation and the explicit local fallback.
            // The pinned VSS rewrite drops TopN and ignores OFFSET: preserving
            // that observed behavior means searching limit rows and clearing it.
        }
    }
    if rewritten && let LogicalKind::TopN { offset, .. } = &mut plan.kind {
        *offset = 0;
    }
    for child in &mut plan.children {
        rewrite_scan(optimizer, child)?;
    }
    Ok(())
}

impl OptimizerRule for ScanRule {
    fn name(&self) -> &'static str {
        "vss.scan"
    }
    fn post_optimize(&self, optimizer: &mut Optimizer, mut plan: LogicalOperator) -> Result<LogicalOperator> {
        rewrite_scan(optimizer, &mut plan)?;
        Ok(plan)
    }
}

#[derive(Debug)]
struct TopKRule;

fn rewrite_topk(optimizer: &Optimizer, plan: &mut LogicalOperator) -> Result<()> {
    if let LogicalKind::Aggregate(aggregate) = &plan.kind
        && aggregate.groups.is_empty()
        && plan.expressions.len() == 1
        && plan.children.len() == 1
        && let ExpressionKind::Aggregate(aggregate) = &plan.expressions[0].kind
        && aggregate.function.name.as_ref() == "min_by"
        && aggregate.children.len() == 3
        && let ExpressionKind::Constant(limit) = &aggregate.children[2].kind
        && let Some(limit) = limit
            .value
            .as_i128()
            .and_then(|limit| usize::try_from(limit).ok())
        && limit > 0
        && limit < STANDARD_VECTOR_SIZE
        && let LogicalKind::Get(get) = &mut plan.children[0].kind
        && let Some(data) = match_scan(optimizer, get, &aggregate.children[1], limit)?
    {
        install_scan(get, data);
        plan.children[0].estimated_cardinality = Some(limit as u64);
        // The original aggregate computes the SQL scores and owns its k/filter
        // semantics on both the graph candidates and transaction-local fallback.
    }
    for child in &mut plan.children {
        rewrite_topk(optimizer, child)?;
    }
    Ok(())
}

impl OptimizerRule for TopKRule {
    fn name(&self) -> &'static str {
        "vss.topk"
    }
    fn post_optimize(&self, optimizer: &mut Optimizer, mut plan: LogicalOperator) -> Result<LogicalOperator> {
        rewrite_topk(optimizer, &mut plan)?;
        Ok(plan)
    }
}

pub(crate) fn optimizer_rules() -> Vec<Arc<dyn OptimizerRule>> {
    vec![
        Arc::new(ExpressionRule),
        Arc::new(ScanRule),
        Arc::new(TopKRule),
        Arc::new(super::join::JoinRule),
    ]
}
