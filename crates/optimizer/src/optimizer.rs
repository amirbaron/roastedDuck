//! The optimizer driver (DuckDB's `optimizer.cpp`): runs the passes in DuckDB's order,
//! each unless disabled through `disabled_optimizers`.

use std::cell::Cell;
use std::collections::BTreeSet;
use std::sync::Arc;

use rustdb_catalog::EntryKind;
use rustdb_common::string_util::candidates_error_message;
use rustdb_common::{Error, Result};
use rustdb_planner::function_binder::FunctionBinder;
use rustdb_planner::{ClientContext, Expression, LogicalKind, LogicalOperator, LogicalOperatorType};

use crate::build_probe_side::optimize_build_probe_side;
use crate::column_lifetime_analyzer::analyze_column_lifetime;
use crate::expression_rewriter::ExpressionRewriter;
use crate::filter_pullup::FilterPullup;
use crate::filter_pushdown::FilterPushdown;
use crate::join_order::JoinOrderOptimizer;
use crate::remove_unused_columns::remove_unused_columns;
use crate::statistics_propagator::StatisticsPropagator;

macro_rules! optimizer_types {
    ($($variant:ident => $name:literal,)*) => {
        /// An optimizer pass (DuckDB's `OptimizerType`).
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum OptimizerType {
            $($variant,)*
        }

        impl OptimizerType {
            /// Every pass, in DuckDB's declaration order.
            pub const ALL: &'static [Self] = &[$(Self::$variant,)*];

            /// The pass's name in `disabled_optimizers` (DuckDB's `OptimizerTypeToString`).
            pub const fn name(self) -> &'static str {
                match self {
                    $(Self::$variant => $name,)*
                }
            }
        }
    };
}

optimizer_types! {
    ExpressionRewriter => "expression_rewriter",
    FilterPullup => "filter_pullup",
    FilterPushdown => "filter_pushdown",
    EmptyResultPullup => "empty_result_pullup",
    CteFilterPusher => "cte_filter_pusher",
    RegexRange => "regex_range",
    InClause => "in_clause",
    JoinOrder => "join_order",
    Deliminator => "deliminator",
    UnnestRewriter => "unnest_rewriter",
    UnusedColumns => "unused_columns",
    StatisticsPropagation => "statistics_propagation",
    CommonSubexpressions => "common_subexpressions",
    CommonAggregate => "common_aggregate",
    ColumnLifetime => "column_lifetime",
    LimitPushdown => "limit_pushdown",
    RowGroupPruner => "row_group_pruner",
    TopN => "top_n",
    TopNWindowElimination => "top_n_window_elimination",
    BuildSideProbeSide => "build_side_probe_side",
    CompressedMaterialization => "compressed_materialization",
    DuplicateGroups => "duplicate_groups",
    ReorderFilter => "reorder_filter",
    SamplingPushdown => "sampling_pushdown",
    JoinFilterPushdown => "join_filter_pushdown",
    Extension => "extension",
    MaterializedCte => "materialized_cte",
    SumRewriter => "sum_rewriter",
    LateMaterialization => "late_materialization",
    CteInlining => "cte_inlining",
    CommonSubplan => "common_subplan",
    JoinElimination => "join_elimination",
    WindowSelfJoin => "window_self_join",
}

impl OptimizerType {
    /// The pass called `name` (DuckDB's `OptimizerTypeFromString`).
    pub fn from_name(name: &str) -> Result<Self> {
        if let Some(found) = Self::ALL.iter().find(|ty| ty.name() == name) {
            return Ok(*found);
        }
        let names: Vec<String> = Self::ALL.iter().map(|ty| ty.name().to_owned()).collect();
        Err(Error::parser(format!(
            "Optimizer type \"{name}\" not recognized\n{}",
            candidates_error_message(&names, name, "Candidate optimizers")
        )))
    }

    /// Parses the value of the `disabled_optimizers` setting: a comma-separated list
    /// (DuckDB's `DisabledOptimizersSetting::SetGlobal`).
    pub fn parse_list(list: &str) -> Result<BTreeSet<Self>> {
        let mut result = BTreeSet::new();
        for entry in list.split(',') {
            let name = entry.trim().to_lowercase();
            if name.is_empty() {
                continue;
            }
            result.insert(Self::from_name(&name)?);
        }
        Ok(result)
    }
}

/// The settings that steer the optimizer.
#[derive(Clone, Debug)]
pub struct OptimizerConfig {
    /// `enable_optimizer` (`PRAGMA enable_optimizer` / `disable_optimizer`).
    pub enabled: bool,
    /// `disabled_optimizers`.
    pub disabled: BTreeSet<OptimizerType>,
    /// `debug_force_no_cross_product`.
    pub force_no_cross_product: bool,
    /// `prefer_range_joins`: no projection maps on joins that may become range joins.
    pub prefer_range_joins: bool,
}

impl Default for OptimizerConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            disabled: BTreeSet::new(),
            force_no_cross_product: false,
            prefer_range_joins: false,
        }
    }
}

/// Rewrites logical plans into cheaper equivalent ones (DuckDB's `Optimizer`).
#[derive(Debug)]
pub struct Optimizer {
    pub(crate) context: Arc<ClientContext>,
    pub(crate) config: OptimizerConfig,
    /// The next unused table index (DuckDB's `Binder::GenerateTableIndex`).
    next_table_index: Cell<u64>,
}

/// Fixes the columns of every scan: the binder shares them with its table bindings
/// while binding; from here on the optimizer owns them.
fn finalize_column_ids(op: &mut LogicalOperator) {
    if let LogicalKind::Get(get) = &mut op.kind {
        get.sync_column_ids();
        get.shared_column_ids = None;
    }
    for child in &mut op.children {
        finalize_column_ids(child);
    }
}

/// The largest table index a plan uses.
fn max_table_index(op: &LogicalOperator) -> u64 {
    let own = op
        .column_bindings()
        .iter()
        .map(|binding| binding.table_index)
        .filter(|&index| index != u64::MAX)
        .max()
        .unwrap_or(0);
    let kind_index = match &op.kind {
        LogicalKind::Aggregate(aggregate) => aggregate.group_index.max(aggregate.aggregate_index).max(
            if aggregate.groupings_index == u64::MAX {
                0
            } else {
                aggregate.groupings_index
            },
        ),
        LogicalKind::ComparisonJoin(join) => join.mark_index,
        _ => 0,
    };
    op.children
        .iter()
        .map(max_table_index)
        .fold(own.max(kind_index), u64::max)
}

impl Optimizer {
    /// An optimizer for plans bound in `context`.
    pub fn new(context: Arc<ClientContext>, config: OptimizerConfig) -> Self {
        Self {
            context,
            config,
            next_table_index: Cell::new(0),
        }
    }

    /// A fresh table index (DuckDB's `Binder::GenerateTableIndex`).
    pub fn generate_table_index(&self) -> u64 {
        let index = self.next_table_index.get();
        self.next_table_index.set(index + 1);
        index
    }

    /// The client context the plan was bound in.
    pub fn context(&self) -> &ClientContext {
        &self.context
    }

    /// Whether a pass is disabled (DuckDB's `Optimizer::OptimizerDisabled`).
    pub fn optimizer_disabled(&self, ty: OptimizerType) -> bool {
        self.config.disabled.contains(&ty)
    }

    /// Optimizes `plan` (DuckDB's `ClientContext` check of `enable_optimizer` and
    /// `RequireOptimizer`, then `Optimizer::Optimize`).
    pub fn optimize(&mut self, plan: LogicalOperator) -> Result<LogicalOperator> {
        if !self.config.enabled || matches!(plan.kind, LogicalKind::Prepare { .. }) {
            return Ok(plan);
        }
        let next = self.next_table_index.get().max(max_table_index(&plan) + 1);
        self.next_table_index.set(next);
        let mut plan = plan;
        finalize_column_ids(&mut plan);
        self.run_built_in_optimizers(plan)
    }

    /// Runs `pass` on the plan unless it is disabled (DuckDB's `RunOptimizer`).
    fn run(
        &mut self,
        ty: OptimizerType,
        plan: LogicalOperator,
        pass: impl FnOnce(&mut Self, LogicalOperator) -> Result<LogicalOperator>,
    ) -> Result<LogicalOperator> {
        if self.optimizer_disabled(ty) {
            return Ok(plan);
        }
        pass(self, plan)
    }

    /// DuckDB's `Optimizer::RunBuiltInOptimizers`.
    fn run_built_in_optimizers(&mut self, plan: LogicalOperator) -> Result<LogicalOperator> {
        if plan.children.is_empty()
            && matches!(
                plan.operator_type(),
                LogicalOperatorType::LogicalTransaction
                    | LogicalOperatorType::LogicalPragma
                    | LogicalOperatorType::LogicalSet
                    | LogicalOperatorType::LogicalAttach
                    | LogicalOperatorType::LogicalUpdateExtensions
                    | LogicalOperatorType::LogicalCreateSecret
                    | LogicalOperatorType::LogicalExtensionOperator
            )
        {
            return Ok(plan);
        }
        // Expression rewrites simplify the expression trees without changing the plan.
        let plan = self.run(OptimizerType::ExpressionRewriter, plan, |optimizer, mut plan| {
            ExpressionRewriter::new(optimizer).visit_operator(&mut plan)?;
            Ok(plan)
        })?;
        let plan = self.run(OptimizerType::FilterPullup, plan, |_, plan| {
            FilterPullup::default().rewrite(plan)
        })?;
        let plan = self.run(OptimizerType::FilterPushdown, plan, |optimizer, mut plan| {
            FilterPushdown::check_mark_to_semi(&mut plan, &mut BTreeSet::new());
            FilterPushdown::new(optimizer, true).rewrite(plan)
        })?;
        // Join ordering also turns cross products with filters into joins and sets the
        // estimated cardinalities.
        let plan = self.run(OptimizerType::JoinOrder, plan, |optimizer, plan| {
            JoinOrderOptimizer::new(
                optimizer.context(),
                optimizer.config.force_no_cross_product,
                optimizer.context().config.max_expression_depth,
            )
            .optimize(plan, None)
        })?;
        let plan = self.run(OptimizerType::UnusedColumns, plan, |optimizer, mut plan| {
            remove_unused_columns(optimizer, &mut plan)?;
            Ok(plan)
        })?;
        // Projection maps drop columns as soon as they are no longer needed.
        let plan = self.run(OptimizerType::ColumnLifetime, plan, |optimizer, mut plan| {
            analyze_column_lifetime(&mut plan, optimizer.config.prefer_range_joins)?;
            Ok(plan)
        })?;
        // With the column lifetimes known, the cheaper side of each join is built.
        let plan = self.run(OptimizerType::BuildSideProbeSide, plan, |optimizer, mut plan| {
            optimize_build_probe_side(&mut plan, optimizer.config.prefer_range_joins)?;
            Ok(plan)
        })?;
        // ORDER BY + LIMIT becomes a Top-N.
        let plan = self.run(OptimizerType::TopN, plan, |_, plan| {
            Ok(crate::topn::optimize(plan))
        })?;
        let plan = self.run(
            OptimizerType::StatisticsPropagation,
            plan,
            |optimizer, mut plan| {
                let mut propagator = StatisticsPropagator::new(optimizer, &mut plan)?;
                propagator.propagate_statistics(&mut plan)?;
                Ok(plan)
            },
        )?;
        let plan = self.run(OptimizerType::ColumnLifetime, plan, |optimizer, mut plan| {
            analyze_column_lifetime(&mut plan, optimizer.config.prefer_range_joins)?;
            Ok(plan)
        })?;
        Ok(plan)
    }

    /// Binds a call to a built-in scalar function; failures are internal errors (DuckDB's
    /// `Optimizer::BindScalarFunction`).
    pub fn bind_scalar_function(&self, name: &str, children: Vec<Expression>) -> Result<Expression> {
        bind_scalar_function(&self.context, name, children, false).map_err(|error| {
            Error::internal(format!(
                "Optimizer exception - failed to bind function {name}: {}",
                error.raw_message()
            ))
        })
    }
}

/// Binds a call to a built-in scalar function by name, raising the binder's error if no
/// overload fits (DuckDB's `FunctionBinder::BindScalarFunction` followed by
/// `error.Throw()`).
pub(crate) fn bind_scalar_function(
    context: &ClientContext,
    name: &str,
    children: Vec<Expression>,
    is_operator: bool,
) -> Result<Expression> {
    let entry = context
        .get_entry(
            "system",
            "main",
            rustdb_catalog::CatalogType::ScalarFunction,
            name,
            true,
        )?
        .ok_or_else(|| Error::internal(format!("scalar function {name} not found")))?;
    let EntryKind::ScalarFunction(functions) = &entry.entry.kind else {
        return Err(Error::internal(format!("{name} is not a scalar function")));
    };
    FunctionBinder::new(context)
        .bind_scalar_function(functions, children, is_operator)?
        .map_err(|error| error.error)
}

/// Binds a call to a built-in aggregate function by name, without DISTINCT, filter or
/// ordering (DuckDB's `FunctionBinder::BindAggregateFunction` on e.g. `CountStarFun`).
pub(crate) fn bind_aggregate_function(
    context: &ClientContext,
    name: &str,
    children: Vec<Expression>,
) -> Result<Expression> {
    let entry = context
        .get_entry(
            "system",
            "main",
            rustdb_catalog::CatalogType::AggregateFunction,
            name,
            true,
        )?
        .ok_or_else(|| Error::internal(format!("aggregate function {name} not found")))?;
    let EntryKind::AggregateFunction(functions) = &entry.entry.kind else {
        return Err(Error::internal(format!("{name} is not an aggregate function")));
    };
    let binder = FunctionBinder::new(context);
    let types = FunctionBinder::argument_types(&children);
    let function = binder.bind_aggregate_overload(functions, &types)??;
    binder.bind_aggregate_function(
        function,
        children,
        None,
        rustdb_planner::expression::AggregateType::NonDistinct,
        None,
    )
}
