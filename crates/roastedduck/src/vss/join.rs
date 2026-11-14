//! Lateral nearest-neighbor joins backed by actual HNSW graph traversal.
//! Operator pattern and projection replacement follow duckdb-vss b833341.

use roastedduck_ast::{JoinType, OrderType};
use roastedduck_catalog::EntryKind;
use roastedduck_common::{ByteString, Error, Result, STANDARD_VECTOR_SIZE};
use roastedduck_function::{FunctionClient, FunctionData, InOutResult, TableFunction, TableScanState};
use roastedduck_optimizer::column_binding_replacer::{ColumnBindingReplacer, ReplacementBinding};
use roastedduck_optimizer::{Optimizer, OptimizerRule};
use roastedduck_planner::LogicalOperatorType;
use roastedduck_planner::operator::{COLUMN_IDENTIFIER_EMPTY, COLUMN_IDENTIFIER_ROW_ID, LogicalGet};
use roastedduck_planner::{
    ClientContext, ColumnBinding, Expression, ExpressionKind, ExpressionType, LogicalKind, LogicalOperator,
};
use roastedduck_storage::index::hnsw::HnswIndex;
use roastedduck_storage::{DataTable, index::IndexEntry, table::StorageIndex};
use roastedduck_types::{ColumnIndex, DataChunk, LogicalType, Value, Vector};
use std::any::Any;
use std::sync::Arc;

#[derive(Debug)]
struct JoinData {
    table: Arc<DataTable>,
    index: Arc<IndexEntry>,
    columns: Vec<StorageIndex>,
    empty_columns: Vec<bool>,
    types: Vec<LogicalType>,
    vector_column: usize,
    dimensions: usize,
    limit: usize,
    client: Arc<FunctionClient>,
}
impl FunctionData for JoinData {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[derive(Debug, Default)]
struct JoinState {
    input_row: usize,
    match_row: usize,
    matches: Option<DataChunk>,
    graph: bool,
    fallback: bool,
}

fn execute(
    bind: Option<&dyn FunctionData>,
    scan: &mut TableScanState,
    input: &DataChunk,
    output: &mut DataChunk,
) -> Result<InOutResult> {
    let data = bind
        .and_then(|data| data.as_any().downcast_ref::<JoinData>())
        .ok_or_else(|| Error::internal("HNSW join without bind data"))?;
    let context = data
        .client
        .transaction_context()
        .ok_or_else(|| Error::internal("HNSW join without active transaction"))?;
    let context = context
        .downcast_ref::<ClientContext>()
        .ok_or_else(|| Error::internal("HNSW join transaction context type"))?;
    let state = scan
        .local
        .get_or_insert_with(|| Box::new(JoinState::default()))
        .downcast_mut::<JoinState>()
        .ok_or_else(|| Error::internal("HNSW join state type"))?;
    let mut columns: Vec<Vector> = data
        .types
        .iter()
        .map(|ty| Vector::flat(ty.clone(), STANDARD_VECTOR_SIZE))
        .collect();
    let vectors = input.column(data.vector_column).flattened(input.len());
    let child = vectors
        .array_child()
        .ok_or_else(|| Error::internal("HNSW join input is not an array"))?
        .flattened(input.len() * data.dimensions);
    let values = child.values::<f32>();
    let mut count = 0;
    while state.input_row < input.len() && count < STANDARD_VECTOR_SIZE {
        if state.matches.is_none() {
            let start = state.input_row * data.dimensions;
            let (matches, fallback) = super::scan::search_rows(
                context,
                &data.table,
                &data.index,
                &values[start..start + data.dimensions],
                data.limit,
                &data.columns,
            )?;
            state.graph |= !fallback;
            state.fallback |= fallback;
            state.matches = Some(matches);
            state.match_row = 0;
        }
        let matches = state
            .matches
            .as_ref()
            .ok_or_else(|| Error::internal("missing HNSW join matches"))?;
        while state.match_row < matches.len() && count < STANDARD_VECTOR_SIZE {
            for (column, target) in columns.iter_mut().enumerate() {
                let value = match column.cmp(&data.columns.len()) {
                    std::cmp::Ordering::Less => {
                        if data.empty_columns[column] {
                            Value::boolean(true)
                        } else {
                            matches.column(column).get_value(state.match_row)
                        }
                    }
                    std::cmp::Ordering::Equal => Value::bigint((state.match_row + 1) as i64),
                    std::cmp::Ordering::Greater => input
                        .column(column - data.columns.len() - 1)
                        .get_value(state.input_row),
                };
                target.set_value(count, &value)?;
            }
            state.match_row += 1;
            count += 1;
        }
        if state.match_row == matches.len() {
            state.matches = None;
            state.input_row += 1;
        }
    }
    *output = DataChunk::new(columns, count);
    if state.input_row == input.len() {
        state.input_row = 0;
        Ok(InOutResult::NeedMoreInput)
    } else {
        Ok(InOutResult::HaveMoreOutput)
    }
}

fn function() -> TableFunction {
    let mut function = TableFunction::new("hnsw_index_join", Vec::new()).with_in_out(execute);
    function.in_out = true;
    function.in_out_explain = Some(Arc::new(|scan| {
        let state = scan
            .local
            .as_ref()
            .and_then(|state| state.downcast_ref::<JoinState>());
        let mode = match state.map(|state| (state.graph, state.fallback)) {
            Some((true, false)) => "HNSW graph traversal",
            Some((false, true)) => "Exact scan (local changes or stale index)",
            Some((true, true)) => "Mixed HNSW traversal and exact fallback",
            _ => "No input",
        };
        vec![("Execution".into(), mode.into())]
    }));
    function
}

fn projection_index(node: &LogicalOperator) -> Option<u64> {
    match node.kind {
        LogicalKind::Projection { table_index } => Some(table_index),
        _ => None,
    }
}
fn column(expression: &Expression) -> Option<ColumnBinding> {
    match &expression.kind {
        ExpressionKind::ColumnRef(reference) if reference.depth == 0 => Some(reference.binding),
        _ => None,
    }
}

struct Inline<'a> {
    inner: &'a LogicalOperator,
    outer: Option<&'a LogicalOperator>,
    inner_get: &'a LogicalGet,
    window: u64,
    delim: u64,
    correlated: &'a [Expression],
    outer_bindings: &'a [ColumnBinding],
    output_index: u64,
}
impl Inline<'_> {
    fn expression(&self, mut expression: Expression) -> Option<Expression> {
        if let Some(binding) = column(&expression) {
            if let Some(outer) = self.outer
                && projection_index(outer) == Some(binding.table_index)
            {
                return self.expression(outer.expressions.get(binding.column_index as usize)?.clone());
            }
            if projection_index(self.inner) == Some(binding.table_index) {
                return self.expression(self.inner.expressions.get(binding.column_index as usize)?.clone());
            }
            if binding.table_index == self.delim {
                return self.expression(self.correlated.get(binding.column_index as usize)?.clone());
            }
            let index = if binding.table_index == self.inner_get.table_index {
                binding.column_index
            } else if binding.table_index == self.window && binding.column_index == 0 {
                self.inner_get.column_ids.len() as u64
            } else {
                (self.inner_get.column_ids.len()
                    + 1
                    + self.outer_bindings.iter().position(|entry| *entry == binding)?) as u64
            };
            return Some(Expression::column_ref(
                expression.alias,
                expression.return_type,
                ColumnBinding::new(self.output_index, index),
                0,
            ));
        }
        let mut valid = true;
        expression.for_each_child_mut(|child| {
            if let Some(replacement) = self.expression(child.clone()) {
                *child = replacement;
            } else {
                valid = false;
            }
        });
        valid.then_some(expression)
    }
}

type Rewritten = (LogicalOperator, Vec<ReplacementBinding>);

fn rewrite(optimizer: &mut Optimizer, plan: &LogicalOperator) -> Result<Option<Rewritten>> {
    let LogicalKind::ComparisonJoin(join) = &plan.kind else {
        return Ok(None);
    };
    if join.operator_type != LogicalOperatorType::LogicalDelimJoin
        || join.join_type != JoinType::Inner
        || plan.children.len() != 2
        || join.predicate.is_some()
    {
        return Ok(None);
    }
    let (outer, branch) = if matches!(plan.children[1].kind, LogicalKind::Get(_)) {
        (&plan.children[1], &plan.children[0])
    } else if matches!(plan.children[0].kind, LogicalKind::Get(_)) {
        (&plan.children[0], &plan.children[1])
    } else {
        return Ok(None);
    };
    let LogicalKind::Get(_) = &outer.kind else {
        return Ok(None);
    };
    let (outer_projection, filter) = if projection_index(branch).is_some() && branch.children.len() == 1 {
        (Some(branch), &branch.children[0])
    } else {
        (None, branch)
    };
    if !matches!(filter.kind, LogicalKind::Filter { .. })
        || filter.children.len() != 1
        || filter.expressions.len() != 1
    {
        return Ok(None);
    }
    let window = &filter.children[0];
    let LogicalKind::Window { window_index } = window.kind else {
        return Ok(None);
    };
    if window.children.len() != 1 || window.expressions.len() != 1 {
        return Ok(None);
    }
    let inner = &window.children[0];
    let Some(inner_index) = projection_index(inner) else {
        return Ok(None);
    };
    if inner.children.len() != 1 {
        return Ok(None);
    }
    let cross = &inner.children[0];
    if !matches!(cross.kind, LogicalKind::CrossProduct) || cross.children.len() != 2 {
        return Ok(None);
    }
    let (delim, scan) = if matches!(cross.children[0].kind, LogicalKind::DelimGet { .. }) {
        (&cross.children[0], &cross.children[1])
    } else {
        (&cross.children[1], &cross.children[0])
    };
    let LogicalKind::DelimGet {
        table_index: delim_index,
        ..
    } = delim.kind
    else {
        return Ok(None);
    };
    let LogicalKind::Get(inner_get) = &scan.kind else {
        return Ok(None);
    };
    if inner_get.function.name.as_ref() != "seq_scan"
        || !inner_get.table_filters.filters.is_empty()
        || inner_get.sample_options.is_some()
    {
        return Ok(None);
    }
    let predicate = &filter.expressions[0];
    let ExpressionKind::Comparison(compare) = &predicate.kind else {
        return Ok(None);
    };
    if predicate.expression_type != ExpressionType::CompareLessthanorequalto
        || column(&compare.left) != Some(ColumnBinding::new(window_index, 0))
    {
        return Ok(None);
    }
    let ExpressionKind::Constant(limit) = &compare.right.kind else {
        return Ok(None);
    };
    if limit.value.logical_type() != &LogicalType::BIGINT {
        return Ok(None);
    }
    let Some(limit) = limit
        .value
        .as_i64()
        .filter(|&n| n > 0 && n < STANDARD_VECTOR_SIZE as i64)
    else {
        return Ok(None);
    };
    let expression = &window.expressions[0];
    if expression.expression_type != ExpressionType::WindowRowNumber {
        return Ok(None);
    }
    let ExpressionKind::Window(window_expression) = &expression.kind else {
        return Ok(None);
    };
    if window_expression.orders.len() != 1 || window_expression.orders[0].order_type != OrderType::Ascending {
        return Ok(None);
    }
    let Some(order) = column(&window_expression.orders[0].expression) else {
        return Ok(None);
    };
    if order.table_index != inner_index {
        return Ok(None);
    }
    let Some(distance) = inner.expressions.get(order.column_index as usize) else {
        return Ok(None);
    };
    let ExpressionKind::Function(distance) = &distance.kind else {
        return Ok(None);
    };
    if distance.children.len() != 2 {
        return Ok(None);
    }
    let Some(table_entry) = &inner_get.table else {
        return Ok(None);
    };
    let EntryKind::Table(table_entry) = &table_entry.kind else {
        return Ok(None);
    };
    let Some(table) = &table_entry.storage else {
        return Ok(None);
    };
    let outer_bindings = outer.column_bindings();
    let mut matched = None;
    for index in table.indexes().entries() {
        let locked = index.lock();
        let Some(hnsw) = locked
            .index
            .as_secondary()
            .and_then(|index| index.as_any().downcast_ref::<HnswIndex>())
        else {
            continue;
        };
        let metric = match distance.function.name.as_bytes() {
            b"array_distance" => "l2sq",
            b"array_cosine_distance" => "cosine",
            b"array_negative_inner_product" => "ip",
            _ => continue,
        };
        if metric != hnsw.metric() {
            continue;
        }
        for (key, query) in [
            (&distance.children[0], &distance.children[1]),
            (&distance.children[1], &distance.children[0]),
        ] {
            let (Some(key_ref), Some(mut query_ref)) = (column(key), column(query)) else {
                continue;
            };
            if key_ref.table_index != inner_get.table_index || key.return_type != query.return_type {
                continue;
            }
            if !super::optimizer::matches_index_expression(key, inner_get, table_entry, hnsw) {
                continue;
            }
            if query_ref.table_index == delim_index {
                let Some(reference) = join
                    .duplicate_eliminated_columns
                    .get(query_ref.column_index as usize)
                    .and_then(column)
                else {
                    continue;
                };
                query_ref = reference;
            }
            if let Some(position) = outer_bindings.iter().position(|binding| *binding == query_ref) {
                matched = Some((Arc::clone(&index), hnsw.dimensions(), position));
                break;
            }
        }
        if matched.is_some() {
            break;
        }
    }
    let Some((index, dimensions, vector_column)) = matched else {
        return Ok(None);
    };
    let columns = inner_get
        .column_ids
        .iter()
        .map(|column| {
            if column.index == COLUMN_IDENTIFIER_ROW_ID || column.index == COLUMN_IDENTIFIER_EMPTY {
                return Ok(StorageIndex::row_id());
            }
            let physical = table_entry
                .logical_to_physical(column.index as usize)
                .ok_or_else(|| Error::internal("HNSW join generated column"))?;
            Ok(StorageIndex::from_column_index(column, physical as u64))
        })
        .collect::<Result<Vec<_>>>()?;
    let output_index = optimizer.generate_table_index();
    let projection = optimizer.generate_table_index();
    let inline = Inline {
        inner,
        outer: outer_projection,
        inner_get,
        window: window_index,
        delim: delim_index,
        correlated: &join.duplicate_eliminated_columns,
        outer_bindings: &outer_bindings,
        output_index,
    };
    let old_bindings = plan.column_bindings();
    let mut expressions = Vec::new();
    let mut replacements = Vec::new();
    for (position, binding) in old_bindings.iter().enumerate() {
        let Some(ty) = plan.types.get(position) else {
            return Ok(None);
        };
        let Some(expression) = inline.expression(Expression::column_ref("", ty.clone(), *binding, 0)) else {
            return Ok(None);
        };
        expressions.push(expression);
        replacements.push(ReplacementBinding::new(
            *binding,
            ColumnBinding::new(projection, position as u64),
        ));
    }
    let mut types: Vec<_> = inner_get
        .column_ids
        .iter()
        .map(|column| inner_get.column_type(column))
        .collect();
    types.push(LogicalType::BIGINT);
    types.extend(outer.types.iter().cloned());
    let data = JoinData {
        table: Arc::clone(table),
        index,
        columns,
        empty_columns: inner_get
            .column_ids
            .iter()
            .map(|column| column.index == COLUMN_IDENTIFIER_EMPTY)
            .collect(),
        types: types.clone(),
        vector_column,
        dimensions,
        limit: limit as usize,
        client: Arc::clone(&optimizer.context().function_client),
    };
    let mut get = (**inner_get).clone();
    get.table_index = output_index;
    get.function = function();
    get.bind_data = Some(Arc::new(data));
    get.returned_types = types;
    get.names = (0..get.returned_types.len())
        .map(|index| ByteString::from(format!("column{index}")))
        .collect();
    get.column_ids = (0..get.returned_types.len() as u64)
        .map(ColumnIndex::new)
        .collect();
    get.projection_ids.clear();
    get.projected_input.clear();
    get.parameters.clear();
    get.named_parameters.clear();
    get.input_table_types.clone_from(&outer.types);
    get.input_table_names = Vec::new();
    get.ordinality_index = None;
    get.shared_column_ids = None;
    get.table = None;
    get.table_schema = None;
    get.table_filters = roastedduck_storage::TableFilterSet::default();
    get.dynamic_filters = None;
    get.sample_options = None;
    let mut joined = LogicalOperator::new(LogicalKind::Get(Box::new(get))).with_children(vec![outer.clone()]);
    joined.estimated_cardinality = outer
        .estimated_cardinality
        .map(|count| count.saturating_mul(limit as u64));
    let mut result = LogicalOperator::new(LogicalKind::Projection {
        table_index: projection,
    })
    .with_expressions(expressions)
    .with_children(vec![joined]);
    result.resolve_operator_types()?;
    Ok(Some((result, replacements)))
}

fn find(
    optimizer: &mut Optimizer,
    node: &LogicalOperator,
    path: &mut Vec<usize>,
) -> Result<Option<(Vec<usize>, Rewritten)>> {
    if let Some(rewritten) = rewrite(optimizer, node)? {
        return Ok(Some((path.clone(), rewritten)));
    }
    for (position, child) in node.children.iter().enumerate() {
        path.push(position);
        if let Some(found) = find(optimizer, child, path)? {
            return Ok(Some(found));
        }
        path.pop();
    }
    Ok(None)
}

#[derive(Debug)]
pub(super) struct JoinRule;
impl OptimizerRule for JoinRule {
    fn name(&self) -> &'static str {
        "vss_join"
    }
    fn post_optimize(&self, optimizer: &mut Optimizer, mut plan: LogicalOperator) -> Result<LogicalOperator> {
        plan.resolve_operator_types()?;
        while let Some((path, (replacement, bindings))) = find(optimizer, &plan, &mut Vec::new())? {
            let stop = replacement.node_id();
            let mut slot = &mut plan;
            for index in path {
                slot = &mut slot.children[index];
            }
            *slot = replacement;
            ColumnBindingReplacer {
                replacement_bindings: bindings,
            }
            .visit_operator_until(&mut plan, Some(stop));
        }
        plan.resolve_operator_types()?;
        Ok(plan)
    }
}
