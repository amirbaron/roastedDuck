//! Conversion of resolved logical plans into physical plans (DuckDB's
//! `PhysicalPlanGenerator`). Logical operators without a ported physical counterpart
//! raise `Not implemented`.

use std::collections::HashMap;
use std::sync::Arc;

use crate::operators::{
    MergeIntoOperator, PhysicalMergeInto, PhysicalPartitionedAggregate, PhysicalPerfectHashAggregate,
    SharedAppend,
};
use roastedduck_ast::MergeActionType;
use roastedduck_ast::{JoinType, OrderByNullType, OrderType};
use roastedduck_catalog::{Catalog, CatalogEntry, EntryKind, Transaction};
use roastedduck_common::{ByteString, Error, Result, STANDARD_VECTOR_SIZE};
use roastedduck_function::TablePartitionInfo;
use roastedduck_functions::aggregate::distributive::first_last_any::first_function;
use roastedduck_functions::aggregate::sorted::{
    self, NullOrder, OrderModifiers, OrderType as SortOrderType, SortedOrder,
};
use roastedduck_planner::expression::{
    AggregateType, BoundAggregateExpression, BoundConjunctionExpression, BoundOrderByNode, BoundOrderModifier,
};
use roastedduck_planner::operator::{
    COLUMN_IDENTIFIER_ROW_ID, GroupingSet, JoinCondition, LogicalAggregate, LogicalComparisonJoin,
    LogicalDelete, LogicalGet, LogicalInsert, LogicalMergeInto, LogicalUpdate,
};
use roastedduck_planner::{
    BoundLimitNode, Expression, ExpressionKind, ExpressionType, LogicalKind, LogicalOperator,
    LogicalOperatorType,
};
use roastedduck_storage::{DataTable, StorageIndex, TableConstraints};
use roastedduck_types::{ColumnIndex, DataChunk, LogicalType, LogicalTypeId, PhysicalType, Value};

use crate::constraints::table_constraints;
mod asof_join;
mod comparison_join;
pub(crate) mod explain;
pub mod order_preservation;
mod window;

pub use comparison_join::JoinPlanSettings;
pub use explain::ExplainOutput;

use crate::expression_executor::ExpressionExecutor;
use crate::operator::{PhysicalOperator, PhysicalRef, SinkData};
use crate::operators::{
    AlterEntryAction, CatalogTableCreator, CreateIndexAction, CreateSchemaAction, CreateSequenceAction,
    CreateViewAction, DdlAction, DropAction, InsertTarget, PhysicalBatchInsert, PhysicalCreateTable,
    PhysicalDdl, PhysicalDelete, PhysicalInsert, PhysicalSeqScan, PhysicalUpdate, ScanExplainInfo,
};
use crate::operators::{
    BATCH_LIMIT_THRESHOLD, PhysicalColumnDataScan, PhysicalDummyScan, PhysicalFilter, PhysicalHashAggregate,
    PhysicalLimit, PhysicalLimitPercent, PhysicalOrder, PhysicalProjection, PhysicalStreamingLimit,
    PhysicalTableScan, PhysicalTopN, PhysicalUngroupedAggregate,
};
use crate::operators::{CreateEntryAction, PhysicalCreateType, PhysicalCrossProduct, bind_table_indexes};
use crate::operators::{CteData, PhysicalCte, PhysicalCteScan, PhysicalRecursiveCte};
use crate::operators::{
    DelimShared, PhysicalBlockwiseNLJoin, PhysicalDelimJoin, PhysicalDelimScan, PhysicalHashJoin,
    PhysicalUnion,
};
use crate::operators::{
    PhysicalExpressionScan, PhysicalPivot, PhysicalReservoirSample, PhysicalStreamingSample, PhysicalUnnest,
};
use roastedduck_optimizer::rule::{RuleOutcome, apply_ordered_aggregate};

/// Resolve aggregate state-size callbacks at physical preparation, after optimization
/// has removed unused expressions. This also serves prepared statements, which bind
/// again when executed and do not allocate aggregate states during preparation.
pub fn verify_aggregate_state_layouts(plan: &mut LogicalOperator) -> Result<()> {
    fn expression(expr: &mut Expression) -> Result<()> {
        expr.try_for_each_child_mut(expression)?;
        if let ExpressionKind::Aggregate(aggregate) = &expr.kind {
            aggregate.function.ops.verify_state_layout()?;
        }
        Ok(())
    }
    for child in &mut plan.children {
        verify_aggregate_state_layouts(child)?;
    }
    plan.try_for_each_expression_mut(expression)
}

/// Converts a logical plan whose column references are resolved into a physical plan.
/// Plans that read or change tables or the catalog need [`create_physical_plan_with`].
pub fn create_physical_plan(mut op: LogicalOperator) -> Result<PhysicalRef> {
    verify_aggregate_state_layouts(&mut op)?;
    op.estimate_cardinality();
    PlanGenerator::default().plan(op)
}

/// Converts a logical plan into a physical plan running in `context`'s transaction
/// (DuckDB's `PhysicalPlanGenerator::PlanInternal`).
pub fn create_physical_plan_with(mut op: LogicalOperator, context: &PlanContext) -> Result<PhysicalPlan> {
    verify_aggregate_state_layouts(&mut op)?;
    let estimate = op.estimate_cardinality();
    op.estimated_cardinality = Some(estimate);
    let mut generator = PlanGenerator {
        context: Some(context.clone()),
        ..PlanGenerator::default()
    };
    let root = generator.plan(op)?;
    Ok(PhysicalPlan {
        root,
        estimates: CardinalityEstimates(Arc::new(std::mem::take(&mut generator.estimates))),
    })
}

/// A physical plan with the planner's estimated cardinality of its operators.
#[derive(Debug)]
pub struct PhysicalPlan {
    pub root: PhysicalRef,
    pub estimates: CardinalityEstimates,
}

/// The estimated cardinality of the operators of a physical plan (DuckDB keeps it in
/// `PhysicalOperator::estimated_cardinality`).
#[derive(Clone, Debug, Default)]
pub struct CardinalityEstimates(Arc<HashMap<usize, u64>>);

impl CardinalityEstimates {
    /// The estimate for `op`, 0 if it has none.
    pub fn get(&self, op: &PhysicalRef) -> u64 {
        self.get_by_key(explain::operator_key(op.as_ref()))
    }

    /// The estimate for the operator at address `key` (`operator_key`), 0 if it has none.
    pub fn get_by_key(&self, key: usize) -> u64 {
        self.0.get(&key).copied().unwrap_or(0)
    }
}

/// What planning statements on tables and the catalog needs (DuckDB's `ClientContext`
/// as seen by the physical planner).
#[derive(Clone, Debug)]
pub struct PlanContext {
    /// The transaction the statement runs in.
    pub transaction: Arc<Transaction>,
    /// The attached catalogs, the default one first.
    pub catalogs: Vec<Arc<Catalog>>,
    /// The settings that steer join planning.
    pub join_settings: JoinPlanSettings,
    /// Which plans `EXPLAIN` shows.
    pub explain_output: ExplainOutput,
    /// The statement's text, shown by `EXPLAIN ANALYZE`.
    pub query: ByteString,
    /// The client for the rewrites DuckDB's planner makes when the optimizer is enabled
    /// (the ordered aggregates of DISTINCT ON); `None` with the optimizer disabled.
    pub optimizer_client: Option<Arc<roastedduck_planner::ClientContext>>,
    /// The `threads` setting: whether inserts run in parallel and `UseBatchIndex`.
    pub threads: usize,
    /// The `preserve_insertion_order` setting.
    pub preserve_insertion_order: bool,
    /// The `index_scan_max_count` and `index_scan_percentage` settings.
    pub index_scan: (u64, f64),
    /// The client, for the collations planning pushes (`ExpressionBinder::PushCollation`).
    pub client: Arc<roastedduck_planner::ClientContext>,
}

impl PlanContext {
    /// The catalog called `name`; `""` is the default catalog.
    fn catalog(&self, name: impl AsRef<[u8]>) -> Result<Arc<Catalog>> {
        let name = name.as_ref();
        let found = if name.is_empty() {
            self.catalogs.first()
        } else {
            self.catalogs
                .iter()
                .find(|catalog| catalog.name().eq_ignore_ascii_case(name))
        };
        found.cloned().ok_or_else(|| {
            Error::catalog(roastedduck_common::byte_format!(
                "Catalog with name {name} does not exist!"
            ))
        })
    }
}

/// The scan's filters keyed by position in its column list instead of by table column
/// (DuckDB's `CreateTableFilterSet`).
fn table_filter_set(get: &LogicalGet) -> Result<Option<roastedduck_storage::TableFilterSet>> {
    scan_filter_set(get, &get.table_filters)
}

/// `filter_set` (keyed by column index) keyed by position in the scanned columns.
fn scan_filter_set(
    get: &LogicalGet,
    filter_set: &roastedduck_storage::TableFilterSet,
) -> Result<Option<roastedduck_storage::TableFilterSet>> {
    if filter_set.is_empty() {
        return Ok(None);
    }
    let mut filters = roastedduck_storage::TableFilterSet::default();
    for (&column, filter) in &filter_set.filters {
        let position = get
            .column_ids
            .iter()
            .position(|id| id.index == column as u64)
            .ok_or_else(|| Error::internal("Could not find column index for table filter"))?;
        filters.filters.insert(
            position,
            crate::operators::bind_expression_filters(filter.clone()),
        );
    }
    Ok(Some(filters))
}

/// What `EXPLAIN` lists under `Projections` for each scanned column of `get` (the names
/// DuckDB's `PhysicalTableScan::ParamsToString` builds with `AddProjectionNames`).
fn projection_names(get: &LogicalGet) -> Result<Vec<ByteString>> {
    get.column_ids
        .iter()
        .map(|column| {
            let (Some(name), Some(ty)) = (
                get.names.get(column.index as usize),
                get.returned_types.get(column.index as usize),
            ) else {
                return Ok(get.column_name(column));
            };
            let mut result = ByteString::new();
            add_projection_names(column, name, ty, &mut result)?;
            Ok(result)
        })
        .collect()
}

/// The name of a scanned column, or of each of its fields read, one per line (DuckDB's
/// `AddProjectionNames`).
fn add_projection_names(
    index: &ColumnIndex,
    name: &[u8],
    ty: &LogicalType,
    result: &mut ByteString,
) -> Result<()> {
    if index.child_indexes.is_empty() {
        // No fields projected out.
        if !result.is_empty() {
            result.push(b'\n');
        }
        result.push_bytes(name);
        return Ok(());
    }
    let variant_field = |child: &ColumnIndex, result: &mut ByteString| {
        let field_type = if child.has_type() {
            child.ty.clone()
        } else {
            LogicalType::variant()
        };
        add_projection_names(
            child,
            &roastedduck_common::byte_format!("{name}.{}", child.field_name()),
            &field_type,
            result,
        )
    };
    match ty.id() {
        LogicalTypeId::Struct => {
            let fields = ty.struct_children().unwrap_or_default();
            for child in &index.child_indexes {
                if child.has_primary_index() {
                    let (field, field_type) = fields
                        .get(child.index as usize)
                        .ok_or_else(|| Error::internal("projected struct field out of range"))?;
                    add_projection_names(
                        child,
                        &roastedduck_common::byte_format!("{name}.{field}"),
                        field_type,
                        result,
                    )?;
                } else {
                    variant_field(child, result)?;
                }
            }
            Ok(())
        }
        LogicalTypeId::Variant => {
            for child in &index.child_indexes {
                variant_field(child, result)?;
            }
            Ok(())
        }
        _ => Err(Error::internal(roastedduck_common::byte_format!(
            "Unexpected type ({ty}) in AddProjectionNames"
        ))),
    }
}

/// The storage of a table entry (`DuckTableEntry::GetStorage`).
fn table_storage(entry: &CatalogEntry) -> Result<Arc<DataTable>> {
    match &entry.kind {
        EntryKind::Table(table) => table.storage.clone().ok_or_else(|| {
            Error::internal(roastedduck_common::byte_format!(
                "table {} has no storage",
                entry.name()
            ))
        }),
        _ => Err(Error::internal(roastedduck_common::byte_format!(
            "{} is not a table",
            entry.name()
        ))),
    }
}

/// State carried through physical planning (DuckDB's `PhysicalPlanGenerator`).
#[derive(Debug, Default)]
struct PlanGenerator {
    /// The delim joins being planned, innermost last; their delim gets read the data of
    /// the innermost one.
    delim: Vec<(Arc<DelimShared>, usize)>,
    /// The delim scans of each delim join being planned (DuckDB's `GatherDelimScans`).
    delim_scans: Vec<Vec<Arc<PhysicalDelimScan>>>,
    /// The number of the last planned delim join (DuckDB's `delim_index`).
    delim_index: u64,
    /// How `EXPLAIN` shows the delim joins, by the address of their join.
    delim_explain: HashMap<usize, explain::DelimExplain>,
    /// Materialised CTEs by table index (`materialized_ctes`), with the recursion depth
    /// they were planned at.
    materialized: HashMap<u64, (Arc<CteData>, usize)>,
    /// Recursive CTE working tables and their types by table index
    /// (`recursive_cte_tables`).
    working_tables: HashMap<u64, (Arc<CteData>, Vec<LogicalType>)>,
    /// The recurring tables of recursive CTEs (`recurring.<name>`), by table index.
    recurring_tables: HashMap<u64, (Arc<CteData>, Vec<LogicalType>)>,
    /// Per recursive CTE being planned (outermost first), the data from outside it that
    /// its parts read.
    external: Vec<Vec<Arc<dyn SinkData>>>,
    /// The transaction and catalogs for plans on tables.
    context: Option<PlanContext>,
    /// The estimated cardinality of each planned operator, by operator address (DuckDB
    /// keeps it in `PhysicalOperator::estimated_cardinality`), for `EXPLAIN`.
    estimates: HashMap<usize, u64>,
}

impl PlanGenerator {
    /// Records that a scan at the current depth reads `data` produced at `depth`: every
    /// recursive CTE in between must wait for it. Returns whether the scan is external.
    fn read_from(&mut self, data: &Arc<dyn SinkData>, depth: usize) -> bool {
        let Some(levels) = self.external.get_mut(depth..) else {
            return false;
        };
        for level in levels.iter_mut() {
            level.push(Arc::clone(data));
        }
        !levels.is_empty()
    }

    /// The join planning settings of the client (defaults without a plan context).
    fn join_settings(&self) -> JoinPlanSettings {
        self.context
            .as_ref()
            .map_or_else(JoinPlanSettings::default, |context| context.join_settings)
    }

    /// `CreatePlan(LogicalLimit &)` without a percentage: a parallel streaming limit when
    /// the order does not matter, a batch limit when it does and the sources number their
    /// batches, a single-threaded streaming limit otherwise.
    fn plan_limit(
        &self,
        types: Vec<LogicalType>,
        limit: BoundLimitNode,
        offset: BoundLimitNode,
        child: PhysicalRef,
    ) -> PhysicalRef {
        let (threads, preserve_setting) = self.context.as_ref().map_or((1, true), |context| {
            (context.threads, context.preserve_insertion_order)
        });
        if !order_preservation::preserve_insertion_order(child.as_ref(), preserve_setting) {
            return Arc::new(PhysicalStreamingLimit::new(types, limit, offset, true, child));
        }
        if order_preservation::use_batch_index(child.as_ref(), threads)
            && use_batch_limit(child.as_ref(), &limit, &offset)
        {
            return Arc::new(PhysicalLimit::new(types, limit, offset, child));
        }
        Arc::new(PhysicalStreamingLimit::new(types, limit, offset, false, child))
    }

    fn context(&self) -> Result<&PlanContext> {
        self.context
            .as_ref()
            .ok_or_else(|| Error::internal("planning a statement on tables requires a plan context"))
    }

    /// A base table scan (DuckDB's `CreatePlan(LogicalGet&)` with `TableScanFunction`):
    /// logical column indexes become physical storage indexes.
    fn plan_table_scan(
        &self,
        get: &LogicalGet,
        entry: &CatalogEntry,
        mut types: Vec<LogicalType>,
    ) -> Result<PhysicalRef> {
        let context = self.context()?;
        let storage = table_storage(entry)?;
        let EntryKind::Table(table) = &entry.kind else {
            return Err(Error::internal("table scan of a non-table"));
        };
        let mut column_ids = get
            .column_ids
            .iter()
            .map(|column| {
                if column.index == COLUMN_IDENTIFIER_ROW_ID {
                    return Ok(StorageIndex::row_id());
                }
                let physical = usize::try_from(column.index)
                    .ok()
                    .and_then(|logical| table.logical_to_physical(logical))
                    .ok_or_else(|| Error::internal("scan of a generated column"))?;
                Ok(StorageIndex::from_column_index(column, physical as u64))
            })
            .collect::<Result<Vec<_>>>()?;
        if column_ids.is_empty() {
            // Nothing is read (e.g. `count(*)`): scan the row ids for the cardinality.
            column_ids.push(StorageIndex::row_id());
            types = vec![LogicalType::BIGINT];
        }
        let projection = get.projection_ids.iter().map(|&id| id as usize).collect();
        let filters = table_filter_set(get)?;
        // `TableScanInitGlobal` binds the ART indexes before it tries an index scan.
        if (filters.is_some() || get.dynamic_filters.is_some())
            && let Some(table_entry) = &get.table
        {
            bind_table_indexes(&context.client, table_entry)?;
        }
        let explain = ScanExplainInfo {
            table: get
                .table_schema
                .as_ref()
                .map(|(catalog, schema)| roastedduck_ast::qualifier_to_string(catalog, schema, entry.name()))
                .unwrap_or_default(),
            column_names: get
                .column_ids
                .iter()
                .map(|column| {
                    if column.is_row_id_column() {
                        ByteString::from("rowid")
                    } else {
                        get.names.get(column.index as usize).cloned().unwrap_or_default()
                    }
                })
                .collect(),
            projection_names: projection_names(get)?,
            virtual_columns: get
                .column_ids
                .iter()
                .enumerate()
                .filter(|(_, column)| column.is_row_id_column())
                .map(|(position, _)| position)
                .collect(),
            sample_method: get
                .sample_options
                .as_ref()
                .map(|sample| format!("System: {}%", sample.sample_size.to_varchar().unwrap_or_default())),
            file_filters: get.file_filters.explain_params(),
        };
        let mut scan = PhysicalSeqScan::new(
            types,
            storage,
            Arc::clone(&context.transaction),
            column_ids,
            filters,
            projection,
        )
        .with_dynamic_filters(get.dynamic_filters.clone())
        .with_index_scan(Some(context.index_scan))
        .with_explain_info(explain)
        .with_row_group_order(get.row_group_order.clone())
        .with_casts(Arc::clone(&context.client.casts));
        if let Some(sample) = &get.sample_options {
            // DuckDB's `ScanSamplingInfo`: the percentage as a fraction.
            let percentage = sample.sample_size.as_f64().unwrap_or(100.0);
            scan = scan.with_sampling(percentage / 100.0, sample.seed);
        }
        Ok(Arc::new(scan))
    }

    /// `INSERT` (DuckDB's `CreatePlan(LogicalInsert&)`); the defaults of columns missing
    /// from the input are computed by a projection (`PhysicalInsert::ResolveDefaults`).
    fn plan_insert(&self, insert: &LogicalInsert, child: PhysicalRef) -> Result<PhysicalRef> {
        let context = self.context()?;
        // `BindIndexes` of the operator's initialization.
        bind_table_indexes(&context.client, &insert.table)?;
        let storage = table_storage(&insert.table)?;
        let child = if insert.column_index_map.is_empty() {
            child
        } else {
            let input_types = child.types().to_vec();
            let expressions = insert
                .column_index_map
                .iter()
                .enumerate()
                .map(|(column, source)| match source {
                    Some(source) => input_types
                        .get(*source)
                        .map(|ty| Expression::reference("", ty.clone(), *source as u64))
                        .ok_or_else(|| Error::internal("insert column map out of range")),
                    None => insert
                        .bound_defaults
                        .get(column)
                        .cloned()
                        .ok_or_else(|| Error::internal("insert without a default")),
                })
                .collect::<Result<Vec<_>>>()?;
            let types = expressions.iter().map(|e| e.return_type.clone()).collect();
            Arc::new(PhysicalProjection::new(types, expressions, child))
        };
        let constraints = table_constraints(
            &context.transaction,
            &context.catalogs,
            &insert.table,
            &storage,
            &insert.bound_constraints,
        )?;
        let parallel_ok =
            !insert.return_chunk && !storage.has_indexes() && constraints.foreign_keys.is_empty();
        Ok(insert_operator(
            context,
            child,
            InsertTarget::Table(storage),
            constraints,
            insert.return_chunk,
            parallel_ok,
        ))
    }

    /// `DELETE`: the single expression references the row id column of the input.
    fn plan_delete(
        &self,
        delete: &LogicalDelete,
        expressions: &[Expression],
        child: PhysicalRef,
    ) -> Result<PhysicalRef> {
        let context = self.context()?;
        // `BindIndexes` of the operator's initialization.
        bind_table_indexes(&context.client, &delete.table)?;
        let storage = table_storage(&delete.table)?;
        let row_id_index = match expressions.first().map(|expression| &expression.kind) {
            Some(ExpressionKind::Reference(reference)) => reference.index as usize,
            _ => return Err(Error::internal("DELETE without a row id reference")),
        };
        let constraints = table_constraints(
            &context.transaction,
            &context.catalogs,
            &delete.table,
            &storage,
            &delete.bound_constraints,
        )?;
        Ok(Arc::new(
            PhysicalDelete::new(
                child,
                storage,
                Arc::clone(&context.transaction),
                row_id_index,
                delete.return_chunk,
            )
            .with_constraints(constraints),
        ))
    }

    /// `UPDATE`: a projection computes the new values (`DEFAULT` becomes the column's
    /// default) followed by the row id, the input's last column.
    fn plan_update(
        &self,
        update: LogicalUpdate,
        expressions: Vec<Expression>,
        child: PhysicalRef,
    ) -> Result<PhysicalRef> {
        let context = self.context()?;
        // `BindIndexes` of the operator's initialization.
        bind_table_indexes(&context.client, &update.table)?;
        let storage = table_storage(&update.table)?;
        let row_id = child
            .types()
            .len()
            .checked_sub(1)
            .ok_or_else(|| Error::internal("UPDATE input without a row id"))?;
        let mut projections = Vec::with_capacity(expressions.len() + 1);
        for (expression, &column) in expressions.into_iter().zip(&update.columns) {
            if matches!(expression.kind, ExpressionKind::Default) {
                let default = update
                    .bound_defaults
                    .get(column)
                    .cloned()
                    .ok_or_else(|| Error::internal("update without a default"))?;
                projections.push(default);
            } else {
                projections.push(expression);
            }
        }
        projections.push(Expression::reference("", LogicalType::BIGINT, row_id as u64));
        let types = projections.iter().map(|e| e.return_type.clone()).collect();
        let child: PhysicalRef = Arc::new(PhysicalProjection::new(types, projections, child));
        let constraints = table_constraints(
            &context.transaction,
            &context.catalogs,
            &update.table,
            &storage,
            &update.bound_constraints,
        )?;
        Ok(Arc::new(PhysicalUpdate::new(
            child,
            storage,
            Arc::clone(&context.transaction),
            update.columns,
            constraints,
            update.update_is_del_and_insert,
            update.return_chunk,
        )))
    }

    /// `MERGE INTO` (DuckDB's `DuckCatalog::PlanMergeInto` and `PlanMergeIntoAction`):
    /// each action gets its own INSERT, UPDATE or DELETE operator, fed by the merge; the
    /// operators that append share one append.
    fn plan_merge_into(&self, merge: LogicalMergeInto, child: PhysicalRef) -> Result<PhysicalRef> {
        let context = self.context()?;
        // `BindIndexes` of the operator's initialization.
        bind_table_indexes(&context.client, &merge.table)?;
        let storage = table_storage(&merge.table)?;
        let table_types = storage.types();
        let constraints = table_constraints(
            &context.transaction,
            &context.catalogs,
            &merge.table,
            &storage,
            &merge.bound_constraints,
        )?;
        let append = SharedAppend::default();
        let input_types = child.types().to_vec();
        let row_id = Expression::reference("", LogicalType::BIGINT, merge.row_id_start as u64);
        // The sub-operators are not part of the pipeline: their child only gives their types.
        let placeholder =
            |types: Vec<LogicalType>| -> PhysicalRef { Arc::new(PhysicalDummyScan::new(types)) };
        let mut actions = Vec::with_capacity(merge.actions.len());
        for (condition, bound_actions) in merge.actions {
            let mut planned = Vec::with_capacity(bound_actions.len());
            for action in bound_actions {
                let mut operator = MergeIntoOperator {
                    action_type: action.action_type,
                    condition: action.condition,
                    op: None,
                    expressions: Vec::new(),
                };
                match action.action_type {
                    MergeActionType::MergeUpdate => {
                        // The new values followed by the row id (`PhysicalUpdate`'s input).
                        for (expression, &column) in action.expressions.into_iter().zip(&action.columns) {
                            if matches!(expression.kind, ExpressionKind::Default) {
                                operator.expressions.push(
                                    merge
                                        .bound_defaults
                                        .get(column)
                                        .cloned()
                                        .ok_or_else(|| Error::internal("update without a default"))?,
                                );
                            } else {
                                operator.expressions.push(expression);
                            }
                        }
                        operator.expressions.push(row_id.clone());
                        let types = operator
                            .expressions
                            .iter()
                            .map(|e| e.return_type.clone())
                            .collect();
                        operator.op = Some(Arc::new(
                            PhysicalUpdate::new(
                                placeholder(types),
                                Arc::clone(&storage),
                                Arc::clone(&context.transaction),
                                action.columns,
                                constraints.clone(),
                                action.update_is_del_and_insert,
                                merge.return_chunk,
                            )
                            .with_shared_append(Arc::clone(&append)),
                        ));
                    }
                    MergeActionType::MergeDelete => {
                        operator.op = Some(Arc::new(
                            PhysicalDelete::new(
                                placeholder(input_types.clone()),
                                Arc::clone(&storage),
                                Arc::clone(&context.transaction),
                                merge.row_id_start,
                                merge.return_chunk,
                            )
                            .with_constraints(constraints.clone()),
                        ));
                    }
                    MergeActionType::MergeInsert => {
                        let mut expressions = action.expressions;
                        if !action.column_index_map.is_empty() {
                            let mut mapped = Vec::with_capacity(action.column_index_map.len());
                            for (column, source) in action.column_index_map.iter().enumerate() {
                                mapped.push(match source {
                                    Some(source) => expressions
                                        .get(*source)
                                        .cloned()
                                        .ok_or_else(|| Error::internal("insert column map out of range"))?,
                                    None => merge
                                        .bound_defaults
                                        .get(column)
                                        .cloned()
                                        .ok_or_else(|| Error::internal("insert without a default"))?,
                                });
                            }
                            expressions = mapped;
                        }
                        operator.expressions = expressions;
                        operator.op = Some(Arc::new(
                            PhysicalInsert::new(
                                placeholder(table_types.clone()),
                                InsertTarget::Table(Arc::clone(&storage)),
                                Arc::clone(&context.transaction),
                                constraints.clone(),
                                merge.return_chunk,
                            )
                            .with_shared_append(Arc::clone(&append)),
                        ));
                    }
                    MergeActionType::MergeError => operator.expressions = action.expressions,
                    MergeActionType::MergeDoNothing => {}
                }
                planned.push(operator);
            }
            actions.push((condition, planned));
        }
        Ok(Arc::new(PhysicalMergeInto::new(
            child,
            table_types,
            actions,
            merge.row_id_start,
            merge.source_marker,
            merge.return_chunk,
        )))
    }

    /// A catalog statement run by [`PhysicalDdl`].
    fn ddl(
        &self,
        name: &'static str,
        types: Vec<LogicalType>,
        action: Arc<dyn DdlAction>,
    ) -> Result<PhysicalRef> {
        let context = self.context()?;
        Ok(Arc::new(PhysicalDdl::new(
            name,
            types,
            action,
            Arc::clone(&context.transaction),
        )))
    }

    /// A duplicate-eliminated join: the left input is buffered by a delim sink that also
    /// computes the distinct correlated values the right side's delim gets scan
    /// (DuckDB's `PlanDelimJoin` + `PhysicalLeftDelimJoin`).
    fn plan_delim_join(
        &mut self,
        mut join: LogicalComparisonJoin,
        children: Vec<LogicalOperator>,
        types: Vec<LogicalType>,
    ) -> Result<PhysicalRef> {
        let mut children = children.into_iter();
        let (Some(left), Some(right)) = (children.next(), children.next()) else {
            return Err(Error::internal("delim join must have two inputs"));
        };
        let cardinalities = (
            comparison_join::estimate_cardinality(&left),
            comparison_join::estimate_cardinality(&right),
        );
        // DuckDB's `PhysicalRightDelimJoin` (flipped): the duplicate-eliminated input is
        // the right (build) side and the subquery side probes; else the left input is
        // duplicate eliminated and the subquery side builds.
        // The children are planned left to right, like DuckDB's `PlanComparisonJoin`.
        let flipped = join.delim_flipped;
        let shared = Arc::new(DelimShared::default());
        let (input, (subquery, delim_scans)) = if flipped {
            let subquery = self.plan_delim_side(left, &shared)?;
            (self.plan(right)?, subquery)
        } else {
            let input = self.plan(left)?;
            (input, self.plan_delim_side(right, &shared)?)
        };
        let Some(first_scan) = delim_scans.first() else {
            // No scan reads the duplicate-eliminated data: a plain join.
            let (left, right) = if flipped {
                (subquery, input)
            } else {
                (input, subquery)
            };
            return comparison_join::create_comparison_join(self, join, types, left, right, cardinalities);
        };
        self.delim_index += 1;
        for scan in &delim_scans {
            scan.set_delim_index(self.delim_index);
        }
        let distinct_estimate = self.recorded_estimate(&(Arc::clone(first_scan) as PhysicalRef));
        let input_types = input.types().to_vec();
        let duplicate_eliminated_columns = std::mem::take(&mut join.duplicate_eliminated_columns);
        // The DISTINCT groups by the duplicate-eliminated columns, by reference.
        let distinct_groups = duplicate_eliminated_columns
            .iter()
            .enumerate()
            .map(|(position, column)| match &column.kind {
                ExpressionKind::Reference(reference) => reference.index,
                _ => position as u64,
            })
            .collect();
        let delim = PhysicalDelimJoin::new(duplicate_eliminated_columns, &shared, Arc::clone(&input));
        let cached: PhysicalRef = Arc::new(PhysicalDelimScan::buffered_input(
            input_types,
            shared,
            delim,
            flipped,
        ));
        let (left, right) = if flipped {
            (subquery, cached)
        } else {
            (cached, subquery)
        };
        let plan = comparison_join::create_comparison_join(self, join, types, left, right, cardinalities)?;
        self.delim_explain.insert(
            explain::operator_key(plan.as_ref()),
            explain::DelimExplain {
                name: if flipped {
                    "RIGHT_DELIM_JOIN"
                } else {
                    "LEFT_DELIM_JOIN"
                },
                delim_index: self.delim_index,
                input,
                distinct_groups,
                distinct_estimate,
            },
        );
        Ok(plan)
    }

    /// Plans the side of a delim join whose delim gets read `shared`; returns it with
    /// its delim scans.
    fn plan_delim_side(
        &mut self,
        op: LogicalOperator,
        shared: &Arc<DelimShared>,
    ) -> Result<(PhysicalRef, Vec<Arc<PhysicalDelimScan>>)> {
        self.delim.push((Arc::clone(shared), self.external.len()));
        self.delim_scans.push(Vec::new());
        let plan = self.plan(op);
        self.delim.pop();
        let scans = self.delim_scans.pop().unwrap_or_default();
        Ok((plan?, scans))
    }

    /// Computes the groups, aggregate arguments and aggregate filters in a projection
    /// below the aggregate, which then reads them by reference (DuckDB's
    /// `ExtractAggregateExpressions`).
    fn extract_aggregate_expressions(
        &mut self,
        child: PhysicalRef,
        aggregates: &mut [BoundAggregateExpression],
        groups: &mut [Expression],
    ) -> PhysicalRef {
        let mut expressions = Vec::new();
        let mut extract = |expression: &mut Expression| {
            let reference =
                Expression::reference("", expression.return_type.clone(), expressions.len() as u64);
            expressions.push(std::mem::replace(expression, reference));
        };
        for group in groups.iter_mut() {
            extract(group);
        }
        for aggregate in aggregates.iter_mut() {
            for argument in &mut aggregate.children {
                extract(argument);
            }
            if let Some(filter) = &mut aggregate.filter {
                extract(filter);
            }
        }
        if expressions.is_empty() {
            return child;
        }
        let types = expressions
            .iter()
            .map(|expression| expression.return_type.clone())
            .collect();
        let estimate = self.recorded_estimate(&child);
        let projection: PhysicalRef = Arc::new(PhysicalProjection::new(types, expressions, child));
        self.record_estimate(&projection, estimate);
        projection
    }

    /// `DISTINCT` / `DISTINCT ON` (DuckDB's `CreatePlan(LogicalDistinct&)`): a hash
    /// aggregate grouped by the targets, with a `first` (ordered, for DISTINCT ON) of every
    /// other column, which the optimizer rewrites into `arg_min_null` of a sort key.
    fn plan_distinct(
        &mut self,
        mut groups: Vec<Expression>,
        order_by: Option<&BoundOrderModifier>,
        child: PhysicalRef,
    ) -> Result<PhysicalRef> {
        let types = child.types().to_vec();
        let group_count = groups.len();
        let mut group_by_references = HashMap::new();
        let mut aggregate_types = Vec::with_capacity(types.len());
        for (index, target) in groups.iter().enumerate() {
            if let ExpressionKind::Reference(reference) = &target.kind {
                group_by_references.insert(reference.index as usize, index);
            }
            aggregate_types.push(target.return_type.clone());
        }
        let client = self
            .context
            .as_ref()
            .and_then(|context| context.optimizer_client.clone());
        let mut requires_projection = types.len() != group_count;
        let mut aggregates = Vec::new();
        let mut projections = Vec::with_capacity(types.len());
        for (index, ty) in types.iter().enumerate() {
            if let Some(&group) = group_by_references.get(&index) {
                projections.push(Expression::reference("", ty.clone(), group as u64));
                requires_projection |= group != index;
                continue;
            }
            let mut first = Expression::new(
                ExpressionType::BoundAggregate,
                ty.clone(),
                ExpressionKind::Aggregate(Box::new(BoundAggregateExpression {
                    function: first_function::<false, false>(ty),
                    children: vec![Expression::reference("", ty.clone(), index as u64)],
                    bind_info: None,
                    aggr_type: AggregateType::NonDistinct,
                    filter: None,
                    order_bys: order_by.cloned(),
                })),
            );
            if let Some(client) = &client {
                // DuckDB's `OrderedAggregateOptimizer::Apply`.
                if let RuleOutcome::Replaced(rewritten) =
                    apply_ordered_aggregate(client, &mut first, &groups, None)?
                {
                    first = rewritten;
                }
            }
            let ExpressionKind::Aggregate(first) = first.kind else {
                return Err(Error::internal("DISTINCT ON aggregate is not an aggregate"));
            };
            projections.push(Expression::reference(
                "",
                ty.clone(),
                (group_count + aggregates.len()) as u64,
            ));
            aggregate_types.push(ty.clone());
            aggregates.push(bind_sorted_aggregate(*first)?);
            requires_projection = true;
        }
        let child = self.extract_aggregate_expressions(child, &mut aggregates, &mut groups);
        let estimate = self.recorded_estimate(&child);
        let group_by: PhysicalRef = Arc::new(PhysicalHashAggregate::new(
            aggregate_types,
            groups,
            aggregates,
            Vec::new(),
            Vec::new(),
            child,
        )?);
        self.record_estimate(&group_by, estimate);
        if !requires_projection {
            return Ok(group_by);
        }
        let projection: PhysicalRef = Arc::new(PhysicalProjection::new(types, projections, group_by));
        self.record_estimate(&projection, estimate);
        Ok(projection)
    }

    /// Plans `op` and records its estimated cardinality for the planned operators.
    fn plan(&mut self, op: LogicalOperator) -> Result<PhysicalRef> {
        let estimate = op.estimated_cardinality.unwrap_or(0);
        let plan = self.plan_operator(op)?;
        self.record_estimate(&plan, estimate);
        Ok(plan)
    }

    fn plan_operator(&mut self, op: LogicalOperator) -> Result<PhysicalRef> {
        let operator_type = op.operator_type();
        let LogicalOperator {
            kind,
            children,
            expressions,
            types,
            estimated_cardinality,
            ..
        } = op;
        // VALUES over a dummy scan is evaluated here, like DuckDB's foldable fast path.
        if let LogicalKind::ExpressionGet {
            expr_types,
            expressions: rows,
            ..
        } = &kind
            && children
                .first()
                .is_some_and(|child| matches!(child.kind, LogicalKind::DummyScan { .. }))
        {
            let row_count = rows.len() as u64;
            let chunks = evaluate_rows(expr_types, rows)?;
            let scan: PhysicalRef = Arc::new(PhysicalColumnDataScan::new(types, chunks.into()));
            // DuckDB's chunk scan of a folded VALUES list estimates its row count.
            self.record_estimate(&scan, row_count);
            return Ok(scan);
        }
        if let LogicalKind::ComparisonJoin(join) = &kind
            && join.operator_type == LogicalOperatorType::LogicalDelimJoin
        {
            let LogicalKind::ComparisonJoin(join) = kind else {
                unreachable!("matched above")
            };
            return self.plan_delim_join(*join, children, types);
        }
        if let LogicalKind::ComparisonJoin(join) = &kind
            && join.operator_type == LogicalOperatorType::LogicalComparisonJoin
        {
            let LogicalKind::ComparisonJoin(join) = kind else {
                unreachable!("matched above")
            };
            return self.plan_comparison_join(*join, children, types);
        }
        if let LogicalKind::ComparisonJoin(join) = &kind
            && join.operator_type == LogicalOperatorType::LogicalAsofJoin
        {
            let LogicalKind::ComparisonJoin(join) = kind else {
                unreachable!("matched above")
            };
            return self.plan_asof_join(*join, children, types);
        }
        if let LogicalKind::Explain {
            explain_type,
            explain_format,
            logical_plan_unopt,
        } = kind
        {
            let [child] = <[LogicalOperator; 1]>::try_from(children)
                .map_err(|_| Error::internal("EXPLAIN must have one child"))?;
            return self.plan_explain(explain_type, explain_format, logical_plan_unopt, child, types);
        }
        if let LogicalKind::DelimGet { .. } = &kind {
            let (shared, depth) = self
                .delim
                .last()
                .cloned()
                .ok_or_else(|| Error::internal("delim get outside a delim join"))?;
            let external = self.read_from(&(Arc::clone(&shared) as Arc<dyn SinkData>), depth);
            let scan = Arc::new(PhysicalDelimScan::new(types, shared, external));
            if let Some(scans) = self.delim_scans.last_mut() {
                scans.push(Arc::clone(&scan));
            }
            return Ok(scan);
        }
        match kind {
            LogicalKind::MaterializedCte {
                table_index, ctename, ..
            } => {
                let [definition, query] = two_children(children, "CTE")?;
                let definition = self.plan(definition)?;
                let data = Arc::new(CteData::default());
                self.materialized
                    .insert(table_index, (Arc::clone(&data), self.external.len()));
                let query = self.plan(query)?;
                return Ok(PhysicalCte::new(ctename, table_index, &data, definition, query));
            }
            LogicalKind::RecursiveCte {
                ctename,
                table_index,
                union_all,
                key_targets,
                ref_recurring,
                ..
            } => {
                // `distinct_idx`: the key columns, as references into the CTE's columns.
                let key_columns = if key_targets.is_empty() {
                    None
                } else {
                    Some(
                        key_targets
                            .iter()
                            .map(|target| match &target.kind {
                                ExpressionKind::Reference(reference) => usize::try_from(reference.index)
                                    .map_err(|_| Error::internal("USING KEY column out of range")),
                                _ => Err(Error::internal("USING KEY target is not a column reference")),
                            })
                            .collect::<Result<Vec<_>>>()?,
                    )
                };
                let [anchor, recursive] = two_children(children, "recursive CTE")?;
                let working_table = Arc::new(CteData::default());
                self.working_tables
                    .insert(table_index, (Arc::clone(&working_table), types.clone()));
                let recurring_table = Arc::new(CteData::default());
                self.recurring_tables
                    .insert(table_index, (Arc::clone(&recurring_table), types.clone()));
                self.external.push(Vec::new());
                let parts = self
                    .plan(anchor)
                    .and_then(|anchor| Ok((anchor, self.plan(recursive)?)));
                let external = self.external.pop().unwrap_or_default();
                let (anchor, recursive) = parts?;
                return Ok(Arc::new(PhysicalRecursiveCte::new(
                    (ctename, table_index),
                    types,
                    union_all,
                    working_table,
                    [anchor, recursive],
                    external,
                    key_columns,
                    ref_recurring.then_some(recurring_table),
                )));
            }
            LogicalKind::CteRef {
                cte_index,
                chunk_types,
                is_recurring,
                ..
            } => {
                if let Some((data, depth)) = self.materialized.get(&cte_index).cloned() {
                    let external = self.read_from(&(Arc::clone(&data) as Arc<dyn SinkData>), depth);
                    return Ok(Arc::new(PhysicalCteScan::new(
                        chunk_types,
                        data,
                        external,
                        cte_index,
                    )));
                }
                let tables = if is_recurring {
                    &self.recurring_tables
                } else {
                    &self.working_tables
                };
                if is_recurring && !tables.contains_key(&cte_index) {
                    return Err(Error::invalid_input(
                        "RECURRING can only be used with USING KEY in recursive CTE.",
                    ));
                }
                let (data, types) = tables
                    .get(&cte_index)
                    .ok_or_else(|| Error::invalid_input("Referenced recursive CTE does not exist."))?;
                return Ok(Arc::new(PhysicalCteScan::working_table(
                    types.clone(),
                    Arc::clone(data),
                    cte_index,
                )));
            }
            _ => {}
        }
        let has_children = !children.is_empty();
        let mut children = children
            .into_iter()
            .map(|child| self.plan(child))
            .collect::<Result<Vec<_>>>()?
            .into_iter();
        let mut next_child = |what: &str| {
            children
                .next()
                .ok_or_else(|| Error::internal(format!("{what} without input")))
        };
        let plan: PhysicalRef = match kind {
            LogicalKind::DummyScan { .. } => Arc::new(PhysicalDummyScan::new(types)),
            LogicalKind::EmptyResult { .. } => Arc::new(PhysicalColumnDataScan::empty_result(types)),
            LogicalKind::ChunkGet { collection, .. } => {
                Arc::new(PhysicalColumnDataScan::new(types, collection))
            }
            LogicalKind::Get(get) if get.table.is_some() => {
                let entry = get
                    .table
                    .clone()
                    .ok_or_else(|| Error::internal("base table scan without a table"))?;
                self.plan_table_scan(&get, &entry, types)?
            }
            LogicalKind::Insert(insert) => self.plan_insert(&insert, next_child("insert")?)?,
            LogicalKind::Delete(delete) => self.plan_delete(&delete, &expressions, next_child("delete")?)?,
            LogicalKind::Update(update) => self.plan_update(*update, expressions, next_child("update")?)?,
            LogicalKind::MergeInto(merge) => self.plan_merge_into(*merge, next_child("merge")?)?,
            LogicalKind::CopyToFile(copy) => {
                plan_copy_to_file(*copy, next_child("copy")?, self.context.as_ref())?
            }
            LogicalKind::Vacuum(vacuum) => Arc::new(crate::operators::PhysicalVacuum::new(
                next_child("vacuum")?,
                table_storage(&vacuum.table)?,
                vacuum.column_id_map,
            )),
            LogicalKind::Export(export) => {
                let context = self.context()?;
                let catalog = context.catalog(&export.catalog)?;
                let child = next_child("export").ok();
                Arc::new(crate::operators::PhysicalExport::new(
                    child,
                    *export,
                    catalog,
                    Arc::clone(&context.transaction),
                ))
            }
            LogicalKind::CreateTable(info) => {
                let context = self.context()?;
                let info = *info;
                let catalog = context.catalog(&info.catalog)?;
                let io = Arc::clone(catalog.io()?);
                let creator = Arc::new(CatalogTableCreator {
                    catalog,
                    schema: info.schema,
                    info: info.info,
                    io,
                });
                match children.next() {
                    Some(query) => insert_operator(
                        context,
                        query,
                        InsertTarget::Create(creator),
                        TableConstraints::default(),
                        false,
                        true,
                    ),
                    None => Arc::new(PhysicalCreateTable::new(
                        creator,
                        Arc::clone(&context.transaction),
                    )),
                }
            }
            LogicalKind::Create { operator_type, info } => {
                let catalog = self.context()?.catalog(&info.catalog)?;
                let info = *info;
                match operator_type {
                    LogicalOperatorType::LogicalCreateSchema => self.ddl(
                        "CREATE_SCHEMA",
                        types,
                        Arc::new(CreateSchemaAction { catalog, info }),
                    )?,
                    LogicalOperatorType::LogicalCreateView => {
                        self.ddl("CREATE_VIEW", types, Arc::new(CreateViewAction { catalog, info }))?
                    }
                    LogicalOperatorType::LogicalCreateSequence => self.ddl(
                        "CREATE_SEQUENCE",
                        types,
                        Arc::new(CreateSequenceAction { catalog, info }),
                    )?,
                    LogicalOperatorType::LogicalCreateType => match children.next() {
                        Some(query) => Arc::new(PhysicalCreateType::new(
                            catalog,
                            info,
                            query,
                            Arc::clone(&self.context()?.transaction),
                        )),
                        None => self.ddl(
                            "CREATE_TYPE",
                            types,
                            Arc::new(CreateEntryAction { catalog, info }),
                        )?,
                    },
                    LogicalOperatorType::LogicalCreateMacro => self.ddl(
                        "CREATE_MACRO",
                        types,
                        Arc::new(CreateEntryAction { catalog, info }),
                    )?,
                    other => return Err(not_implemented(other)),
                }
            }
            LogicalKind::CreateIndex(create) => {
                let create = *create;
                let catalog = self.context()?.catalog(&create.info.catalog)?;
                self.ddl(
                    "CREATE_INDEX",
                    types,
                    Arc::new(CreateIndexAction {
                        catalog,
                        info: create.info,
                        table: create.table,
                        expressions: create.expressions,
                    }),
                )?
            }
            LogicalKind::Alter(alter) => {
                let alter = *alter;
                let catalog = self.context()?.catalog(&alter.catalog)?;
                self.ddl(
                    "ALTER",
                    types,
                    Arc::new(AlterEntryAction {
                        catalog,
                        schema: alter.schema,
                        entry: alter.entry,
                        action: alter.action,
                        dependents_allowed: alter.dependents_allowed,
                        info: alter.info,
                    }),
                )?
            }
            LogicalKind::Drop(info) => {
                let catalog = self.context()?.catalog(&info.catalog)?;
                self.ddl("DROP", types, Arc::new(DropAction { catalog, info: *info }))?
            }
            LogicalKind::Get(get) if has_children => {
                // A table in-out function over its arguments' rows (`CreatePlan(LogicalGet &)`
                // with children); the binder already cast the input to the function's types.
                let child = next_child("table in-out function")?;
                let column_ids = get.column_ids.iter().map(|column| column.index).collect();
                Arc::new(crate::operators::PhysicalTableInOutFunction::new(
                    types,
                    get.function,
                    get.bind_data,
                    column_ids,
                    get.projected_input,
                    get.ordinality_index,
                    child,
                )?)
            }
            LogicalKind::Get(get)
                if (get.function.scan.is_some() || get.function.parallel_scan.is_some())
                    && (get.projection_ids.is_empty() || get.function.pushdown_filters.is_some()) =>
            {
                let column_ids: Vec<u64> = get.column_ids.iter().map(|column| column.index).collect();
                let explain = crate::operators::ScanExplainInfo {
                    table: ByteString::new(),
                    column_names: get
                        .column_ids
                        .iter()
                        .map(|column| get.column_name(column))
                        .collect(),
                    projection_names: projection_names(&get)?,
                    virtual_columns: get
                        .column_ids
                        .iter()
                        .enumerate()
                        .filter(|(_, column)| column.index >= get.names.len() as u64)
                        .map(|(position, _)| position)
                        .collect(),
                    sample_method: None,
                    file_filters: get.file_filters.explain_params(),
                };
                // Filters pushed into the scan are applied by the function itself.
                let filters = table_filter_set(&get)?;
                let mut bind_data = get.bind_data;
                if let (Some(filters), Some(pushdown), Some(data)) =
                    (&filters, &get.function.pushdown_filters, &bind_data)
                {
                    bind_data = Some(pushdown(
                        data.as_ref(),
                        &column_ids,
                        filters as &dyn std::any::Any,
                    )?);
                }
                let projection = get.projection_ids.iter().map(|&id| id as usize).collect();
                Arc::new(
                    PhysicalTableScan::new(types, get.function, bind_data, column_ids)
                        .with_column_indexes(get.column_ids)
                        .with_projection(projection)
                        .with_filters(filters)
                        .with_dynamic_filters(get.dynamic_filters)
                        .with_explain_info(explain),
                )
            }
            LogicalKind::Projection { .. } => {
                let child = next_child("projection")?;
                // A projection of the child's columns in order is omitted (DuckDB's
                // `CreatePlan(LogicalProjection &)`).
                let omit = child.types().len() == types.len()
                    && expressions.iter().enumerate().all(|(i, expression)| {
                        matches!(&expression.kind, ExpressionKind::Reference(reference) if reference.index == i as u64)
                    });
                if omit {
                    child
                } else {
                    Arc::new(PhysicalProjection::new(types, expressions, child))
                }
            }
            LogicalKind::Filter { projection_map } if expressions.is_empty() && projection_map.is_empty() => {
                next_child("filter")?
            }
            LogicalKind::Filter { projection_map } => {
                let child = next_child("filter")?;
                let is_empty_filter = expressions.is_empty();
                let mut expressions = expressions;
                let predicate = if expressions.len() == 1 {
                    expressions.remove(0)
                } else {
                    Expression::new(
                        ExpressionType::ConjunctionAnd,
                        LogicalType::BOOLEAN,
                        ExpressionKind::Conjunction(BoundConjunctionExpression {
                            children: expressions,
                        }),
                    )
                };
                if projection_map.is_empty() {
                    Arc::new(PhysicalFilter::new(types, predicate, child))
                } else {
                    // The filter keeps its input's columns; a projection then selects the
                    // mapped ones (DuckDB's `CreatePlan(LogicalFilter&)`).
                    let input_types = child.types().to_vec();
                    // Without predicates only the projection remains.
                    let filter: PhysicalRef = if is_empty_filter {
                        child
                    } else {
                        Arc::new(PhysicalFilter::new(input_types.clone(), predicate, child))
                    };
                    let select_list = projection_map
                        .iter()
                        .map(|&index| {
                            input_types
                                .get(index as usize)
                                .map(|ty| Expression::reference("", ty.clone(), index))
                                .ok_or_else(|| Error::internal("filter projection map out of range"))
                        })
                        .collect::<Result<Vec<_>>>()?;
                    Arc::new(PhysicalProjection::new(types, select_list, filter))
                }
            }
            LogicalKind::Aggregate(aggregate) => {
                let child = next_child("aggregate")?;
                let mut aggregates = expressions
                    .into_iter()
                    .map(|expression| match expression.kind {
                        ExpressionKind::Aggregate(aggregate) => bind_sorted_aggregate(*aggregate),
                        _ => Err(Error::internal(format!(
                            "non-aggregate {expression} in an aggregate operator"
                        ))),
                    })
                    .collect::<Result<Vec<_>>>()?;
                let LogicalAggregate {
                    mut groups,
                    grouping_sets,
                    grouping_functions,
                    group_stats,
                    ..
                } = *aggregate;
                let child = self.extract_aggregate_expressions(child, &mut aggregates, &mut groups);
                if !groups.is_empty()
                    && let Some(partitions) = partitioned_aggregate_columns(
                        &groups,
                        &grouping_sets,
                        &grouping_functions,
                        &aggregates,
                        &child,
                    )
                {
                    return Ok(Arc::new(PhysicalPartitionedAggregate::new(
                        types, aggregates, groups, partitions, child,
                    )));
                }
                if !groups.is_empty()
                    && grouping_sets.len() <= 1
                    && grouping_functions.is_empty()
                    && let Some((minima, required_bits)) = perfect_hash_groups(
                        &groups,
                        &group_stats,
                        &aggregates,
                        self.join_settings().perfect_ht_threshold,
                    )
                {
                    return Ok(Arc::new(PhysicalPerfectHashAggregate::new(
                        types,
                        groups,
                        aggregates,
                        minima,
                        required_bits,
                        child,
                    )));
                }
                if !groups.is_empty()
                    || grouping_sets.len() > 1
                    || aggregates
                        .iter()
                        .any(|aggregate| !aggregate.function.ops.has_simple_update())
                {
                    return Ok(Arc::new(
                        PhysicalHashAggregate::new(
                            types,
                            groups,
                            aggregates,
                            grouping_sets,
                            grouping_functions,
                            child,
                        )?
                        .with_group_stats(&group_stats),
                    ));
                }
                Arc::new(PhysicalUngroupedAggregate::new(types, aggregates, child)?)
            }
            LogicalKind::Limit { limit, offset } => {
                let child = next_child("limit")?;
                if matches!(
                    limit,
                    BoundLimitNode::ConstantPercentage(_) | BoundLimitNode::ExpressionPercentage(_)
                ) {
                    Arc::new(PhysicalLimitPercent::new(types, limit, offset, child))
                } else {
                    self.plan_limit(types, limit, offset, child)
                }
            }
            LogicalKind::Order {
                orders,
                projection_map,
            } => {
                let projections = projection_map.iter().map(|&index| index as usize).collect();
                Arc::new(PhysicalOrder::new(
                    types,
                    orders,
                    projections,
                    next_child("order")?,
                ))
            }
            LogicalKind::TopN {
                orders,
                limit,
                offset,
                dynamic_filter,
            } => Arc::new(PhysicalTopN::new(
                types,
                orders,
                limit,
                offset,
                dynamic_filter,
                next_child("top n")?,
            )),
            LogicalKind::PositionalJoin => {
                let left = next_child("positional join")?;
                let right = next_child("positional join")?;
                Arc::new(crate::operators::PhysicalPositionalJoin::new(types, left, right))
            }
            LogicalKind::CrossProduct => {
                let left = next_child("cross product")?;
                let right = next_child("cross product")?;
                Arc::new(PhysicalCrossProduct::new(types, left, right))
            }
            LogicalKind::AnyJoin { join_type, condition } => {
                let left = next_child("join")?;
                let right = next_child("join")?;
                Arc::new(PhysicalBlockwiseNLJoin::new(
                    types, join_type, *condition, left, right,
                )?)
            }
            LogicalKind::SetOperation {
                operator_type,
                setop_all,
                ..
            } => {
                if operator_type != LogicalOperatorType::LogicalUnion {
                    let (Some(left), Some(right)) = (children.next(), children.next()) else {
                        return Err(Error::internal(
                            "EXCEPT / INTERSECT must have exactly two children",
                        ));
                    };
                    let client = self.context.as_ref().map(|context| &*context.client);
                    return except_or_intersect(client, operator_type, setop_all, types, left, right);
                }
                let inputs: Vec<PhysicalRef> = children.by_ref().collect();
                let mut inputs = inputs.into_iter().rev();
                let mut plan = inputs
                    .next()
                    .ok_or_else(|| Error::internal("UNION without inputs"))?;
                for left in inputs {
                    plan = Arc::new(PhysicalUnion::new(types.clone(), left, plan));
                }
                if setop_all {
                    plan
                } else {
                    distinct_on_all_columns(types, plan)?
                }
            }
            LogicalKind::Window { .. } => {
                let child = next_child("window")?;
                let estimate = estimated_cardinality.unwrap_or(0) as usize;
                window::plan_window(types, expressions, estimate, child)?
            }
            LogicalKind::ExpressionGet {
                expressions: rows, ..
            } => Arc::new(PhysicalExpressionScan::new(
                types,
                rows,
                next_child("expression get")?,
            )),
            LogicalKind::Sample { mut sample_options } => {
                let child = next_child("sample")?;
                if sample_options.seed.is_none() {
                    // Planned without a client: an unseeded sample.
                    let mut random = roastedduck_function::random::RandomEngine::default();
                    sample_options.seed = Some(u64::from(random.next_random_integer()));
                }
                match sample_options.method {
                    roastedduck_ast::SampleMethod::ReservoirSample => {
                        Arc::new(PhysicalReservoirSample::new(types, *sample_options, child))
                    }
                    method => {
                        if !sample_options.is_percentage {
                            let name = if method == roastedduck_ast::SampleMethod::SystemSample {
                                "System"
                            } else {
                                "Bernoulli"
                            };
                            return Err(Error::parser(format!(
                                "Sample method {name} cannot be used with a discrete sample count, either switch \
                                 to reservoir sampling or use a sample_size"
                            )));
                        }
                        Arc::new(PhysicalStreamingSample::new(types, &sample_options, child)?)
                    }
                }
            }
            LogicalKind::Pivot { bound_pivot, .. } => {
                Arc::new(PhysicalPivot::new(types, next_child("pivot")?, *bound_pivot)?)
            }
            LogicalKind::Unnest { .. } => {
                Arc::new(PhysicalUnnest::new(types, expressions, next_child("unnest")?)?)
            }
            LogicalKind::Distinct {
                distinct_targets,
                order_by,
                ..
            } => self.plan_distinct(distinct_targets, order_by.as_ref(), next_child("distinct")?)?,
            _ => return Err(not_implemented(operator_type)),
        };
        Ok(plan)
    }
}

/// `EXCEPT` as an anti join and `INTERSECT` as a semi join on all columns compared with
/// `IS NOT DISTINCT FROM` under the columns' collations, then made distinct (DuckDB's
/// `CreatePlan(LogicalSetOperation&)`).
fn except_or_intersect(
    client: Option<&roastedduck_planner::ClientContext>,
    operator_type: LogicalOperatorType,
    setop_all: bool,
    types: Vec<LogicalType>,
    left: PhysicalRef,
    right: PhysicalRef,
) -> Result<PhysicalRef> {
    if left.types() != right.types() {
        return Err(Error::invalid_input("Type mismatch for SET OPERATION"));
    }
    let column_types = left.types().to_vec();
    let mut conditions: Vec<JoinCondition> = column_types
        .iter()
        .enumerate()
        .map(|(index, ty)| {
            Ok(JoinCondition {
                left: collated_reference(client, ty, index)?,
                right: collated_reference(client, ty, index)?,
                comparison: ExpressionType::CompareNotDistinctFrom,
            })
        })
        .collect::<Result<_>>()?;
    // EXCEPT ALL / INTERSECT ALL number the duplicates on each side and also join on the
    // numbers, for bag semantics.
    let (left, right, join_types) = if setop_all {
        conditions.push(JoinCondition {
            left: Expression::reference("", LogicalType::BIGINT, column_types.len() as u64),
            right: Expression::reference("", LogicalType::BIGINT, column_types.len() as u64),
            comparison: ExpressionType::CompareNotDistinctFrom,
        });
        let mut join_types = types.clone();
        join_types.push(LogicalType::BIGINT);
        (
            window::partitioned_row_number(client, left)?,
            window::partitioned_row_number(client, right)?,
            join_types,
        )
    } else {
        (left, right, types.clone())
    };
    let join_type = if operator_type == LogicalOperatorType::LogicalExcept {
        JoinType::Anti
    } else {
        JoinType::Semi
    };
    let join: PhysicalRef = Arc::new(PhysicalHashJoin::new(
        join_types,
        join_type,
        conditions,
        &[],
        &[],
        Vec::new(),
        true,
        left,
        right,
    )?);
    if setop_all {
        // Drop the row numbers again.
        let select_list = column_types
            .iter()
            .enumerate()
            .map(|(index, ty)| Expression::reference("", ty.clone(), index as u64))
            .collect();
        return Ok(Arc::new(PhysicalProjection::new(types, select_list, join)));
    }
    distinct_on_all_columns(types, join)
}

/// A reference to column `index` under the collation of `ty` (`PushCollation`); plans
/// without a client compare binary.
pub(crate) fn collated_reference(
    client: Option<&roastedduck_planner::ClientContext>,
    ty: &LogicalType,
    index: usize,
) -> Result<Expression> {
    let reference = Expression::reference("", ty.clone(), index as u64);
    match client {
        Some(client) => roastedduck_planner::collation_binding::push_collation(client, reference, ty),
        None => Ok(reference),
    }
}

/// `DISTINCT` / `DISTINCT ON` (DuckDB's `CreatePlan(LogicalDistinct&)`): one group per
/// distinct target; every other column becomes a `first` aggregate (ordered by the
/// `ORDER BY` of a `DISTINCT ON`), and a projection restores the column order.
fn two_children(children: Vec<LogicalOperator>, what: &str) -> Result<[LogicalOperator; 2]> {
    <[LogicalOperator; 2]>::try_from(children)
        .map_err(|_| Error::internal(format!("{what} must have two inputs")))
}

/// A `GROUP BY` on every column without aggregates: removes duplicate rows
/// (DuckDB plans `UNION` and `DISTINCT` this way).
fn distinct_on_all_columns(types: Vec<LogicalType>, child: PhysicalRef) -> Result<PhysicalRef> {
    let groups = types
        .iter()
        .enumerate()
        .map(|(index, ty)| Expression::reference("", ty.clone(), index as u64))
        .collect();
    Ok(Arc::new(PhysicalHashAggregate::new(
        types,
        groups,
        Vec::new(),
        Vec::new(),
        Vec::new(),
        child,
    )?))
}

/// Replaces an aggregate with `ORDER BY` arguments by its sorted wrapper (DuckDB's
/// `FunctionBinder::BindSortedAggregate`, called from `ExtractAggregateExpressions`).
/// The columns of the source that partition the input by the groups, if a partitioned
/// aggregate can compute the aggregate (DuckDB's `CanUsePartitionedAggregate`): a single
/// grouping set of plain column references, no DISTINCT aggregates, and a table function
/// scan, below projections of column references and filters, whose batches hold a single
/// value of those columns.
fn partitioned_aggregate_columns(
    groups: &[Expression],
    grouping_sets: &[GroupingSet],
    grouping_functions: &[Vec<u64>],
    aggregates: &[BoundAggregateExpression],
    child: &PhysicalRef,
) -> Option<Vec<u64>> {
    if grouping_sets.len() > 1
        || !grouping_functions.is_empty()
        || aggregates.iter().any(BoundAggregateExpression::is_distinct)
    {
        return None;
    }
    let reference_index = |expression: &Expression| match &expression.kind {
        ExpressionKind::Reference(reference) => Some(reference.index),
        _ => None,
    };
    let mut partition_columns = groups.iter().map(reference_index).collect::<Option<Vec<_>>>()?;
    // Walk down to the source operator.
    let mut current: &dyn PhysicalOperator = child.as_ref();
    let table_scan = loop {
        if let Some(table_scan) = current.cast::<PhysicalTableScan>() {
            break table_scan;
        }
        if let Some(projection) = current.cast::<PhysicalProjection>() {
            let select_list = projection.select_list();
            partition_columns = partition_columns
                .iter()
                .map(|&column| select_list.get(column as usize).and_then(reference_index))
                .collect::<Option<Vec<_>>>()?;
        } else if current.cast::<PhysicalFilter>().is_none() {
            // Unsupported operator for partition pass-through.
            return None;
        }
        current = current.children().first()?.as_ref();
    };
    (table_scan.partition_info(&mut partition_columns)? == TablePartitionInfo::SingleValuePartitions)
        .then_some(partition_columns)
}

/// The minimum and slot index bits of every group, if a perfect hash aggregate can
/// compute the aggregate (DuckDB's `CanUsePerfectHashAggregate`): every group is an
/// integer whose statistics bound it (small types are bounded by their type), the bits
/// stay within `threshold`, and no aggregate is DISTINCT.
fn perfect_hash_groups(
    groups: &[Expression],
    group_stats: &[Option<roastedduck_storage::statistics::BaseStatistics>],
    aggregates: &[BoundAggregateExpression],
    threshold: u64,
) -> Option<(Vec<Value>, Vec<usize>)> {
    use roastedduck_storage::statistics::BaseStatistics;
    let mut minima = Vec::with_capacity(groups.len());
    let mut required_bits = Vec::with_capacity(groups.len());
    let mut perfect_hash_bits = 0;
    for (index, group) in groups.iter().enumerate() {
        let ty = &group.return_type;
        let physical = ty.physical_type();
        if !matches!(
            physical,
            PhysicalType::Int8
                | PhysicalType::Int16
                | PhysicalType::Int32
                | PhysicalType::Int64
                | PhysicalType::UInt8
                | PhysicalType::UInt16
                | PhysicalType::UInt32
                | PhysicalType::UInt64
        ) {
            return None;
        }
        let stats = if let Some(stats) = group_stats.get(index).cloned().flatten() {
            stats
        } else {
            // Small types are bounded by the type itself.
            if !matches!(
                physical,
                PhysicalType::Int8 | PhysicalType::Int16 | PhysicalType::UInt8 | PhysicalType::UInt16
            ) {
                return None;
            }
            let mut stats = BaseStatistics::create_unknown(ty.clone());
            if let Some(numeric) = stats.numeric_mut() {
                numeric.min = roastedduck_storage::statistics::minimum_value(ty);
                numeric.max = roastedduck_storage::statistics::maximum_value(ty);
            }
            stats
        };
        stats.numeric().filter(|numeric| numeric.has_min_max())?;
        let (min, max) = (stats.numeric_min()?, stats.numeric_max()?);
        // A maximum below the minimum (no rows) fails here too.
        let range = u64::try_from(max.as_i128()? - min.as_i128()?).ok()?;
        // Bail out on any range of 2^31 or more.
        if range >= u64::from(i32::MAX.unsigned_abs()) {
            return None;
        }
        // One more for NULL, one more to count from one (`RequiredBitsForValue`).
        let range = range + 2;
        let bits = range.bit_width() as usize;
        perfect_hash_bits += bits;
        if perfect_hash_bits as u64 > threshold {
            return None;
        }
        minima.push(min);
        required_bits.push(bits);
    }
    if aggregates.iter().any(BoundAggregateExpression::is_distinct) {
        return None;
    }
    Some((minima, required_bits))
}

fn bind_sorted_aggregate(mut aggregate: BoundAggregateExpression) -> Result<BoundAggregateExpression> {
    let Some(order_bys) = aggregate.order_bys.take() else {
        return Ok(aggregate);
    };
    if aggregate.children.is_empty() {
        return Ok(aggregate);
    }
    // `BoundOrderModifier::Simplify`: each ordering is only needed once.
    let mut orders: Vec<BoundOrderByNode> = Vec::with_capacity(order_bys.orders.len());
    for order in order_bys.orders {
        if !orders
            .iter()
            .any(|seen| seen.expression.equals(&order.expression))
        {
            orders.push(order);
        }
    }
    let sorted_orders: Vec<SortedOrder> = orders
        .iter()
        .map(|order| SortedOrder {
            ty: order.expression.return_type.clone(),
            modifiers: OrderModifiers::new(
                if order.order_type == OrderType::Descending {
                    SortOrderType::Descending
                } else {
                    SortOrderType::Ascending
                },
                if order.null_order == OrderByNullType::NullsLast {
                    NullOrder::NullsLast
                } else {
                    NullOrder::NullsFirst
                },
            ),
            argument: aggregate
                .children
                .iter()
                .position(|child| child.equals(&order.expression)),
        })
        .collect();
    let argument_types: Vec<_> = aggregate
        .children
        .iter()
        .map(|child| child.return_type.clone())
        .collect();
    let binding = sorted::bind_sorted_aggregate(
        aggregate.function,
        aggregate.bind_info.take(),
        &argument_types,
        &sorted_orders,
    )?;
    if binding.append_orders {
        aggregate
            .children
            .extend(orders.into_iter().map(|order| order.expression));
    }
    aggregate.function = binding.function;
    aggregate.bind_info = Some(binding.bind_data);
    Ok(aggregate)
}

fn not_implemented(operator_type: LogicalOperatorType) -> Error {
    Error::not_implemented(format!(
        "physical plan for logical operator {}",
        operator_type.name()
    ))
}

/// Evaluates the rows of a `VALUES` list into chunks (DuckDB materialises foldable rows
/// into a `ColumnDataCollection`).
fn evaluate_rows(types: &[LogicalType], rows: &[Vec<Expression>]) -> Result<Vec<DataChunk>> {
    let mut chunks = Vec::new();
    for batch in rows.chunks(STANDARD_VECTOR_SIZE) {
        let mut chunk = DataChunk::with_types(types, batch.len());
        for (row_index, row) in batch.iter().enumerate() {
            for (column, expression) in row.iter().enumerate() {
                let value = ExpressionExecutor::evaluate_scalar(expression)?;
                chunk.column_mut(column).set_value(row_index, &value)?;
            }
        }
        chunk.set_len(batch.len());
        chunks.push(chunk);
    }
    Ok(chunks)
}

/// Whether a LIMIT over `child` collects its rows in parallel by batch (`UseBatchLimit`):
/// only over a filter or a join, since over a plain table scan the threads would scan many
/// rows just to throw them away, and only for a constant limit (plus offset) of at most
/// [`BATCH_LIMIT_THRESHOLD`] rows, which every thread may keep.
fn use_batch_limit(child: &dyn PhysicalOperator, limit: &BoundLimitNode, offset: &BoundLimitNode) -> bool {
    let mut current = child;
    loop {
        match current.name() {
            "TABLE_SCAN" if current.table_filters().is_none() => return false,
            "PROJECTION" => match current.children().first() {
                Some(next) => current = next.as_ref(),
                None => break,
            },
            _ => break,
        }
    }
    let BoundLimitNode::ConstantValue(limit) = limit else {
        return false;
    };
    let total = match offset {
        BoundLimitNode::ExpressionValue(_) => return false,
        BoundLimitNode::ConstantValue(offset) => limit.saturating_add(*offset),
        _ => *limit,
    };
    total <= BATCH_LIMIT_THRESHOLD
}

/// `PhysicalPlanGenerator::CreatePlan(LogicalCopyToFile &)`.
fn plan_copy_to_file(
    copy: roastedduck_planner::operator::LogicalCopyToFile,
    child: PhysicalRef,
    context: Option<&PlanContext>,
) -> Result<PhysicalRef> {
    let (threads, preserve_setting) = context.map_or((1, true), |context| {
        (context.threads, context.preserve_insertion_order)
    });
    let mut preserve_insertion_order =
        order_preservation::preserve_insertion_order(child.as_ref(), preserve_setting);
    let mut supports_batch_index = order_preservation::use_batch_index(child.as_ref(), threads);
    if let Some(preserve) = copy.preserve_order {
        preserve_insertion_order = preserve;
    }
    if copy.per_thread_output
        || copy.file_size_bytes.is_some()
        || copy.rotate
        || copy.partition_output
        || !copy.partition_columns.is_empty()
    {
        if copy.preserve_order == Some(true) {
            return Err(Error::invalid_input(
                "PRESERVE_ORDER is not supported with these parameters",
            ));
        }
        preserve_insertion_order = false;
        supports_batch_index = false;
    }
    let mode = copy.function.execution_mode.as_ref().map_or(
        roastedduck_function::copy::CopyFunctionExecutionMode::RegularCopyToFile,
        |mode| {
            mode(
                copy.bind_data.as_ref(),
                preserve_insertion_order,
                supports_batch_index,
            )
        },
    );
    let batch = mode == roastedduck_function::copy::CopyFunctionExecutionMode::BatchCopyToFile;
    if batch && !supports_batch_index {
        return Err(Error::internal(
            "BATCH_COPY_TO_FILE can only be used if batch indexes are supported",
        ));
    }
    let mut file_path = copy.file_path;
    // The writers open the file on worker threads, where the statement's file opener is
    // not set, so the access configuration is checked here (`OpenerFileSystem`).
    roastedduck_function::file_system::verify_can_access_file(&file_path)?;
    file_path = roastedduck_function::file_system::expand_path_bytes(&file_path);
    if copy.use_tmp_file {
        file_path = match file_path.iter().rposition(|byte| *byte == b'/') {
            Some(slash) => {
                roastedduck_common::byte_format!("{}tmp_{}", &file_path[..=slash], &file_path[slash + 1..])
            }
            None => roastedduck_common::byte_format!("tmp_{file_path}"),
        };
    }
    let options = crate::operators::CopyToFileOptions {
        file_path,
        use_tmp_file: copy.use_tmp_file,
        overwrite_mode: copy.overwrite_mode,
        filename_pattern: copy.filename_pattern,
        file_extension: copy.file_extension,
        per_thread_output: copy.per_thread_output,
        file_size_bytes: copy.file_size_bytes,
        rotate: copy.rotate,
        partition_output: copy.partition_output,
        partition_columns: copy.partition_columns,
        write_partition_columns: copy.write_partition_columns,
        write_empty_file: copy.write_empty_file,
        hive_file_pattern: copy.hive_file_pattern,
        return_type: copy.return_type,
        names: copy.names,
        expected_types: copy.expected_types,
    };
    let parallel = mode == roastedduck_function::copy::CopyFunctionExecutionMode::ParallelCopyToFile
        && copy.function.copy_to_sink_local.is_some()
        && copy.function.copy_to_combine.is_some()
        && !options.per_thread_output
        && !options.partition_output
        && !options.rotate;
    let operator = crate::operators::PhysicalCopyToFile::new(child, copy.function, copy.bind_data, options);
    Ok(Arc::new(if batch {
        operator.with_batches()
    } else if parallel {
        operator.with_parallel_writes()
    } else {
        operator
    }))
}

/// The insert DuckDB plans for `child` (`DuckCatalog::PlanInsert`, `PlanCreateTableAs`): a
/// batch insert when the order must be kept and every source numbers its batches, a
/// parallel insert when the order does not matter (and there are several threads), a
/// serial one otherwise. `parallel_ok` is false for `RETURNING` and, unlike DuckDB, for
/// tables with indexes or foreign keys, which always insert serially here.
fn insert_operator(
    context: &PlanContext,
    child: PhysicalRef,
    target: InsertTarget,
    constraints: TableConstraints,
    return_chunk: bool,
    parallel_ok: bool,
) -> PhysicalRef {
    let transaction = Arc::clone(&context.transaction);
    let parallel_streaming =
        !order_preservation::preserve_insertion_order(child.as_ref(), context.preserve_insertion_order);
    if parallel_ok
        && !parallel_streaming
        && order_preservation::use_batch_index(child.as_ref(), context.threads)
    {
        return Arc::new(PhysicalBatchInsert::new(child, target, transaction, constraints));
    }
    Arc::new(
        PhysicalInsert::new(child, target, transaction, constraints, return_chunk)
            .with_parallel(parallel_ok && parallel_streaming && context.threads > 1),
    )
}
