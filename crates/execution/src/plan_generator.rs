//! Conversion of resolved logical plans into physical plans (DuckDB's
//! `PhysicalPlanGenerator`). Logical operators without a ported physical counterpart
//! raise `Not implemented`.

use std::collections::HashMap;
use std::sync::Arc;

use rustdb_ast::JoinType;
use rustdb_catalog::{Catalog, CatalogEntry, EntryKind, Transaction};
use rustdb_common::{Error, Result, STANDARD_VECTOR_SIZE};
use rustdb_planner::expression::BoundConjunctionExpression;
use rustdb_planner::operator::{
    COLUMN_IDENTIFIER_ROW_ID, JoinCondition, LogicalAggregate, LogicalComparisonJoin, LogicalDelete,
    LogicalGet, LogicalInsert, LogicalUpdate,
};
use rustdb_planner::{
    BoundLimitNode, Expression, ExpressionKind, ExpressionType, LogicalKind, LogicalOperator,
    LogicalOperatorType,
};
use rustdb_storage::{DataTable, StorageIndex, TableConstraints, TableIoManager};
use rustdb_types::{DataChunk, LogicalType};

use crate::constraints::table_constraints;
use crate::expression_executor::ExpressionExecutor;
use crate::operator::{PhysicalRef, SinkData};
use crate::operators::{
    AlterEntryAction, CatalogTableCreator, CreateIndexAction, CreateSchemaAction, CreateSequenceAction,
    CreateViewAction, DdlAction, DropAction, InsertTarget, PhysicalCreateTable, PhysicalDdl, PhysicalDelete,
    PhysicalInsert, PhysicalSeqScan, PhysicalUpdate,
};
use crate::operators::{CteData, PhysicalCte, PhysicalCteScan, PhysicalRecursiveCte};
use crate::operators::{
    DelimShared, PhysicalDelimJoin, PhysicalDelimScan, PhysicalNestedLoopJoin, PhysicalUnion,
};
use crate::operators::{
    PhysicalColumnDataScan, PhysicalDummyScan, PhysicalFilter, PhysicalHashAggregate, PhysicalLimitPercent,
    PhysicalOrder, PhysicalProjection, PhysicalStreamingLimit, PhysicalTableScan, PhysicalUngroupedAggregate,
};

/// Converts a logical plan whose column references are resolved into a physical plan.
/// Plans that read or change tables or the catalog need [`create_physical_plan_with`].
pub fn create_physical_plan(op: LogicalOperator) -> Result<PhysicalRef> {
    PlanGenerator::default().plan(op)
}

/// Converts a logical plan into a physical plan running in `context`'s transaction.
pub fn create_physical_plan_with(op: LogicalOperator, context: &PlanContext) -> Result<PhysicalRef> {
    PlanGenerator {
        context: Some(context.clone()),
        ..PlanGenerator::default()
    }
    .plan(op)
}

/// What planning statements on tables and the catalog needs (DuckDB's `ClientContext`
/// as seen by the physical planner).
#[derive(Clone, Debug)]
pub struct PlanContext {
    /// The transaction the statement runs in.
    pub transaction: Arc<Transaction>,
    /// The attached catalogs, the default one first.
    pub catalogs: Vec<Arc<Catalog>>,
    /// Where new tables keep their data.
    pub io: Arc<TableIoManager>,
}

impl PlanContext {
    /// The catalog called `name`; `""` is the default catalog.
    fn catalog(&self, name: &str) -> Result<Arc<Catalog>> {
        let found = if name.is_empty() {
            self.catalogs.first()
        } else {
            self.catalogs
                .iter()
                .find(|catalog| catalog.name().eq_ignore_ascii_case(name))
        };
        found
            .cloned()
            .ok_or_else(|| Error::catalog(format!("Catalog with name {name} does not exist!")))
    }
}

/// The storage of a table entry (`DuckTableEntry::GetStorage`).
fn table_storage(entry: &CatalogEntry) -> Result<Arc<DataTable>> {
    match &entry.kind {
        EntryKind::Table(table) => table
            .storage
            .clone()
            .ok_or_else(|| Error::internal(format!("table {} has no storage", entry.name()))),
        _ => Err(Error::internal(format!("{} is not a table", entry.name()))),
    }
}

/// State carried through physical planning (DuckDB's `PhysicalPlanGenerator`).
#[derive(Debug, Default)]
struct PlanGenerator {
    /// The delim joins being planned, innermost last; their delim gets read the data of
    /// the innermost one.
    delim: Vec<(Arc<DelimShared>, usize)>,
    /// Materialised CTEs by table index (`materialized_ctes`), with the recursion depth
    /// they were planned at.
    materialized: HashMap<u64, (Arc<CteData>, usize)>,
    /// Recursive CTE working tables and their types by table index
    /// (`recursive_cte_tables`).
    working_tables: HashMap<u64, (Arc<CteData>, Vec<LogicalType>)>,
    /// Per recursive CTE being planned (outermost first), the data from outside it that
    /// its parts read.
    external: Vec<Vec<Arc<dyn SinkData>>>,
    /// The transaction and catalogs for plans on tables.
    context: Option<PlanContext>,
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
                Ok(StorageIndex::new(physical as u64))
            })
            .collect::<Result<Vec<_>>>()?;
        if column_ids.is_empty() {
            // Nothing is read (e.g. `count(*)`): scan the row ids for the cardinality.
            column_ids.push(StorageIndex::row_id());
            types = vec![LogicalType::BIGINT];
        }
        let projection = get.projection_ids.iter().map(|&id| id as usize).collect();
        Ok(Arc::new(PhysicalSeqScan::new(
            types,
            storage,
            Arc::clone(&context.transaction),
            column_ids,
            None,
            projection,
        )))
    }

    /// `INSERT` (DuckDB's `CreatePlan(LogicalInsert&)`); the defaults of columns missing
    /// from the input are computed by a projection (`PhysicalInsert::ResolveDefaults`).
    fn plan_insert(&self, insert: &LogicalInsert, child: PhysicalRef) -> Result<PhysicalRef> {
        let context = self.context()?;
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
        Ok(Arc::new(PhysicalInsert::new(
            child,
            InsertTarget::Table(storage),
            Arc::clone(&context.transaction),
            constraints,
            insert.return_chunk,
        )))
    }

    /// `DELETE`: the single expression references the row id column of the input.
    fn plan_delete(
        &self,
        delete: &LogicalDelete,
        expressions: &[Expression],
        child: PhysicalRef,
    ) -> Result<PhysicalRef> {
        let context = self.context()?;
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
        join: LogicalComparisonJoin,
        children: Vec<LogicalOperator>,
        types: Vec<LogicalType>,
    ) -> Result<PhysicalRef> {
        if join.delim_flipped {
            return Err(Error::not_implemented("RIGHT_DELIM_JOIN"));
        }
        let mut children = children.into_iter();
        let (Some(left), Some(right)) = (children.next(), children.next()) else {
            return Err(Error::internal("delim join must have two inputs"));
        };
        let left = self.plan(left)?;
        let shared = Arc::new(DelimShared::default());
        self.delim.push((Arc::clone(&shared), self.external.len()));
        let right = self.plan(right);
        self.delim.pop();
        let right = right?;
        let left_types = left.types().to_vec();
        let delim = PhysicalDelimJoin::new(join.duplicate_eliminated_columns, &shared, left);
        let cached: PhysicalRef = Arc::new(PhysicalDelimScan::buffered_input(left_types, shared, delim));
        Ok(Arc::new(PhysicalNestedLoopJoin::new(
            types,
            join.join_type,
            join.conditions,
            join.predicate,
            cached,
            right,
        )?))
    }

    fn plan(&mut self, op: LogicalOperator) -> Result<PhysicalRef> {
        let operator_type = op.operator_type();
        let LogicalOperator {
            kind,
            children,
            expressions,
            types,
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
            let chunks = evaluate_rows(expr_types, rows)?;
            return Ok(Arc::new(PhysicalColumnDataScan::new(types, chunks.into())));
        }
        if let LogicalKind::ComparisonJoin(join) = &kind
            && join.operator_type == LogicalOperatorType::LogicalDelimJoin
        {
            let LogicalKind::ComparisonJoin(join) = kind else {
                unreachable!("matched above")
            };
            return self.plan_delim_join(*join, children, types);
        }
        if let LogicalKind::DelimGet { .. } = &kind {
            let (shared, depth) = self
                .delim
                .last()
                .cloned()
                .ok_or_else(|| Error::internal("delim get outside a delim join"))?;
            let external = self.read_from(&(Arc::clone(&shared) as Arc<dyn SinkData>), depth);
            return Ok(Arc::new(PhysicalDelimScan::new(types, shared, external)));
        }
        match kind {
            LogicalKind::MaterializedCte { table_index, .. } => {
                let [definition, query] = two_children(children, "CTE")?;
                let definition = self.plan(definition)?;
                let data = Arc::new(CteData::default());
                self.materialized
                    .insert(table_index, (Arc::clone(&data), self.external.len()));
                let query = self.plan(query)?;
                return Ok(PhysicalCte::new(&data, definition, query));
            }
            LogicalKind::RecursiveCte {
                table_index,
                union_all,
                key_targets,
                ref_recurring,
                ..
            } => {
                if !key_targets.is_empty() || ref_recurring {
                    return Err(Error::not_implemented("recursive CTE with USING KEY"));
                }
                let [anchor, recursive] = two_children(children, "recursive CTE")?;
                let working_table = Arc::new(CteData::default());
                self.working_tables
                    .insert(table_index, (Arc::clone(&working_table), types.clone()));
                self.external.push(Vec::new());
                let parts = self
                    .plan(anchor)
                    .and_then(|anchor| Ok((anchor, self.plan(recursive)?)));
                let external = self.external.pop().unwrap_or_default();
                let (anchor, recursive) = parts?;
                return Ok(Arc::new(PhysicalRecursiveCte::new(
                    types,
                    union_all,
                    working_table,
                    anchor,
                    recursive,
                    external,
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
                    return Ok(Arc::new(PhysicalCteScan::new(chunk_types, data, external)));
                }
                if is_recurring {
                    return Err(Error::invalid_input(
                        "RECURRING can only be used with USING KEY in recursive CTE.",
                    ));
                }
                let (data, types) = self
                    .working_tables
                    .get(&cte_index)
                    .ok_or_else(|| Error::invalid_input("Referenced recursive CTE does not exist."))?;
                return Ok(Arc::new(PhysicalCteScan::new(
                    types.clone(),
                    Arc::clone(data),
                    false,
                )));
            }
            _ => {}
        }
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
            LogicalKind::EmptyResult { .. } => Arc::new(PhysicalColumnDataScan::new(types, Arc::from([]))),
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
            LogicalKind::CreateTable(info) => {
                let context = self.context()?;
                let info = *info;
                let creator = Arc::new(CatalogTableCreator {
                    catalog: context.catalog(&info.catalog)?,
                    schema: info.schema,
                    info: info.info,
                    io: Arc::clone(&context.io),
                });
                let transaction = Arc::clone(&context.transaction);
                match children.next() {
                    Some(query) => Arc::new(PhysicalInsert::new(
                        query,
                        InsertTarget::Create(creator),
                        transaction,
                        TableConstraints::default(),
                        false,
                    )),
                    None => Arc::new(PhysicalCreateTable::new(creator, transaction)),
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
                    }),
                )?
            }
            LogicalKind::Drop(info) => {
                let catalog = self.context()?.catalog(&info.catalog)?;
                self.ddl("DROP", types, Arc::new(DropAction { catalog, info: *info }))?
            }
            LogicalKind::Get(get) if get.function.scan.is_some() && get.projection_ids.is_empty() => {
                let column_ids = get.column_ids.iter().map(|column| column.index).collect();
                Arc::new(PhysicalTableScan::new(
                    types,
                    get.function,
                    get.bind_data,
                    column_ids,
                ))
            }
            LogicalKind::Projection { .. } => Arc::new(PhysicalProjection::new(
                types,
                expressions,
                next_child("projection")?,
            )),
            LogicalKind::Filter { projection_map } => {
                if !projection_map.is_empty() {
                    return Err(Error::not_implemented("filter with a projection map"));
                }
                let child = next_child("filter")?;
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
                Arc::new(PhysicalFilter::new(types, predicate, child))
            }
            LogicalKind::Aggregate(aggregate) => {
                let child = next_child("aggregate")?;
                if !aggregate.groups.is_empty() || aggregate.grouping_sets.len() > 1 {
                    let aggregates = expressions
                        .into_iter()
                        .map(|expression| match expression.kind {
                            ExpressionKind::Aggregate(aggregate) => Ok(*aggregate),
                            _ => Err(Error::internal(format!(
                                "non-aggregate {expression} in an aggregate operator"
                            ))),
                        })
                        .collect::<Result<Vec<_>>>()?;
                    let LogicalAggregate {
                        groups,
                        grouping_sets,
                        grouping_functions,
                        ..
                    } = *aggregate;
                    return Ok(Arc::new(PhysicalHashAggregate::new(
                        types,
                        groups,
                        aggregates,
                        grouping_sets,
                        grouping_functions,
                        child,
                    )?));
                }
                let aggregates = expressions
                    .into_iter()
                    .map(|expression| match expression.kind {
                        ExpressionKind::Aggregate(aggregate) => Ok(*aggregate),
                        _ => Err(Error::internal(format!(
                            "non-aggregate {expression} in an aggregate operator"
                        ))),
                    })
                    .collect::<Result<Vec<_>>>()?;
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
                    Arc::new(PhysicalStreamingLimit::new(types, limit, offset, child))
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
            LogicalKind::ComparisonJoin(join)
                if join.operator_type == LogicalOperatorType::LogicalComparisonJoin =>
            {
                let left = next_child("join")?;
                let right = next_child("join")?;
                let join = *join;
                if !join.left_projection_map.is_empty() || !join.right_projection_map.is_empty() {
                    return Err(Error::not_implemented("join projection maps"));
                }
                Arc::new(PhysicalNestedLoopJoin::new(
                    types,
                    join.join_type,
                    join.conditions,
                    join.predicate,
                    left,
                    right,
                )?)
            }
            LogicalKind::CrossProduct => {
                let left = next_child("cross product")?;
                let right = next_child("cross product")?;
                Arc::new(PhysicalNestedLoopJoin::new(
                    types,
                    JoinType::Inner,
                    Vec::new(),
                    None,
                    left,
                    right,
                )?)
            }
            LogicalKind::AnyJoin { join_type, condition } => {
                let left = next_child("join")?;
                let right = next_child("join")?;
                Arc::new(PhysicalNestedLoopJoin::new(
                    types,
                    join_type,
                    Vec::new(),
                    Some(*condition),
                    left,
                    right,
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
                    return except_or_intersect(operator_type, setop_all, types, left, right);
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
            LogicalKind::Distinct {
                distinct_targets,
                order_by,
                ..
            } => {
                let child = next_child("distinct")?;
                let identity = distinct_targets.len() == types.len()
                && distinct_targets.iter().enumerate().all(|(i, target)| {
                    matches!(&target.kind, ExpressionKind::Reference(r) if r.index as usize == i)
                });
                if !identity || order_by.is_some() {
                    return Err(Error::not_implemented(
                        "DISTINCT ON (requires the first() aggregate)",
                    ));
                }
                distinct_on_all_columns(types, child)?
            }
            _ => return Err(not_implemented(operator_type)),
        };
        Ok(plan)
    }
}

/// `EXCEPT` as an anti join and `INTERSECT` as a semi join on all columns compared with
/// `IS NOT DISTINCT FROM`, then made distinct (DuckDB's `CreatePlan(LogicalSetOperation&)`).
fn except_or_intersect(
    operator_type: LogicalOperatorType,
    setop_all: bool,
    types: Vec<LogicalType>,
    left: PhysicalRef,
    right: PhysicalRef,
) -> Result<PhysicalRef> {
    if left.types() != right.types() {
        return Err(Error::invalid_input("Type mismatch for SET OPERATION"));
    }
    if setop_all {
        return Err(Error::not_implemented(
            "EXCEPT ALL / INTERSECT ALL (requires the window operator)",
        ));
    }
    let conditions = left
        .types()
        .iter()
        .enumerate()
        .map(|(index, ty)| JoinCondition {
            left: Expression::reference("", ty.clone(), index as u64),
            right: Expression::reference("", ty.clone(), index as u64),
            comparison: ExpressionType::CompareNotDistinctFrom,
        })
        .collect();
    let join_type = if operator_type == LogicalOperatorType::LogicalExcept {
        JoinType::Anti
    } else {
        JoinType::Semi
    };
    let join = Arc::new(PhysicalNestedLoopJoin::new(
        types.clone(),
        join_type,
        conditions,
        None,
        left,
        right,
    )?);
    distinct_on_all_columns(types, join)
}

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
