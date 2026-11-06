//! `EXPLAIN` rendering of logical plans (DuckDB's `LogicalOperator::GetName`,
//! `ParamsToString` and `ToString(ExplainFormat)`).

use rustdb_ast::ExplainFormat;
use rustdb_common::tree_renderer::{
    RenderParams, RenderSource, RenderTree, RenderTreeNode, RendererKind, render,
};
use rustdb_common::{ByteString, Result};

use super::{LogicalKind, LogicalOperator};
use crate::expression::Expression;
use crate::generated::enums::LogicalOperatorType;

/// The renderer of an `EXPLAIN` format; formats without a renderer use the text one.
pub fn renderer_kind(format: ExplainFormat) -> RendererKind {
    match format {
        ExplainFormat::Json => RendererKind::Json,
        ExplainFormat::Html => RendererKind::Html,
        ExplainFormat::Graphviz => RendererKind::Graphviz,
        ExplainFormat::Yaml => RendererKind::Yaml,
        ExplainFormat::Mermaid => RendererKind::Mermaid,
        ExplainFormat::Default | ExplainFormat::Text => RendererKind::Text,
    }
}

/// The short operator name of `EXPLAIN` output (DuckDB's `LogicalOperatorToString`).
pub const fn logical_operator_name(operator_type: LogicalOperatorType) -> &'static str {
    use LogicalOperatorType as T;
    match operator_type {
        T::LogicalGet => "GET",
        T::LogicalChunkGet => "CHUNK_GET",
        T::LogicalDelimGet => "DELIM_GET",
        T::LogicalEmptyResult => "EMPTY_RESULT",
        T::LogicalExpressionGet => "EXPRESSION_GET",
        T::LogicalAnyJoin => "ANY_JOIN",
        T::LogicalAsofJoin => "ASOF_JOIN",
        T::LogicalDependentJoin => "DEPENDENT_JOIN",
        T::LogicalComparisonJoin => "COMPARISON_JOIN",
        T::LogicalDelimJoin => "DELIM_JOIN",
        T::LogicalProjection => "PROJECTION",
        T::LogicalFilter => "FILTER",
        T::LogicalAggregateAndGroupBy => "AGGREGATE",
        T::LogicalWindow => "WINDOW",
        T::LogicalUnnest => "UNNEST",
        T::LogicalLimit => "LIMIT",
        T::LogicalOrderBy => "ORDER_BY",
        T::LogicalTopN => "TOP_N",
        T::LogicalSample => "SAMPLE",
        T::LogicalCopyToFile => "COPY_TO_FILE",
        T::LogicalCopyDatabase => "COPY_DATABASE",
        T::LogicalJoin => "JOIN",
        T::LogicalCrossProduct => "CROSS_PRODUCT",
        T::LogicalPositionalJoin => "POSITIONAL_JOIN",
        T::LogicalUnion => "UNION",
        T::LogicalExcept => "EXCEPT",
        T::LogicalIntersect => "INTERSECT",
        T::LogicalInsert => "INSERT",
        T::LogicalDistinct => "DISTINCT",
        T::LogicalDelete => "DELETE",
        T::LogicalUpdate => "UPDATE",
        T::LogicalMergeInto => "MERGE_INTO",
        T::LogicalPrepare => "PREPARE",
        T::LogicalDummyScan => "DUMMY_SCAN",
        T::LogicalCreateIndex => "CREATE_INDEX",
        T::LogicalCreateTable => "CREATE_TABLE",
        T::LogicalCreateMacro => "CREATE_MACRO",
        T::LogicalExplain => "EXPLAIN",
        T::LogicalExecute => "EXECUTE",
        T::LogicalVacuum => "VACUUM",
        T::LogicalRecursiveCte => "REC_CTE",
        T::LogicalMaterializedCte => "CTE",
        T::LogicalCteRef => "CTE_SCAN",
        T::LogicalAlter => "ALTER",
        T::LogicalCreateSequence => "CREATE_SEQUENCE",
        T::LogicalCreateType => "CREATE_TYPE",
        T::LogicalCreateView => "CREATE_VIEW",
        T::LogicalCreateSchema => "CREATE_SCHEMA",
        T::LogicalCreateSecret => "CREATE_SECRET",
        T::LogicalAttach => "ATTACH",
        T::LogicalDetach => "DETACH",
        T::LogicalDrop => "DROP",
        T::LogicalPragma => "PRAGMA",
        T::LogicalTransaction => "TRANSACTION",
        T::LogicalExport => "EXPORT",
        T::LogicalSet => "SET",
        T::LogicalReset => "RESET",
        T::LogicalLoad => "LOAD",
        T::LogicalExtensionOperator => "CUSTOM_OP",
        T::LogicalPivot => "PIVOT",
        T::LogicalUpdateExtensions => "UPDATE_EXTENSIONS",
        T::LogicalInvalid => "INVALID",
    }
}

/// The expressions' names, one per line.
fn names(expressions: &[Expression]) -> ByteString {
    ByteString::join(
        &expressions.iter().map(Expression::name).collect::<Vec<_>>(),
        b"\n",
    )
}

impl LogicalOperator {
    /// The operator's name in `EXPLAIN` output (DuckDB's `GetName`).
    pub fn explain_name(&self) -> ByteString {
        match &self.kind {
            LogicalKind::Get(get) => get.function.name.to_ascii_uppercase().into(),
            _ => logical_operator_name(self.operator_type()).into(),
        }
    }

    /// Adds the estimated cardinality, if known (DuckDB's
    /// `SetParamsEstimatedCardinality`).
    pub fn set_params_estimated_cardinality(&self, result: &mut RenderParams) {
        if let Some(estimate) = self.estimated_cardinality {
            result.insert(
                RenderTreeNode::ESTIMATED_CARDINALITY.to_owned(),
                estimate.to_string(),
            );
        }
    }

    /// The operator's key-value lines in `EXPLAIN` output (DuckDB's `ParamsToString`).
    pub fn params_to_string(&self) -> RenderParams {
        let mut result = RenderParams::new();
        match &self.kind {
            LogicalKind::Aggregate(aggregate) => {
                result.insert("Groups".to_owned(), names(&aggregate.groups));
                result.insert("Expressions".to_owned(), names(&self.expressions));
            }
            LogicalKind::AnyJoin { condition, .. } => {
                result.insert("Condition".to_owned(), condition.to_bytes());
            }
            LogicalKind::ComparisonJoin(join) => {
                result.insert("Join Type".to_owned(), join.join_type.name().to_owned());
                let mut conditions: Vec<ByteString> = join
                    .conditions
                    .iter()
                    .map(|condition| {
                        Expression::comparison(
                            condition.comparison,
                            condition.left.clone(),
                            condition.right.clone(),
                        )
                        .to_bytes()
                    })
                    .collect();
                if let Some(predicate) = &join.predicate {
                    conditions.push(predicate.to_bytes());
                }
                result.insert("Conditions".to_owned(), ByteString::join(&conditions, b"\n"));
            }
            LogicalKind::CteRef { cte_index, .. } => {
                result.insert("CTE Index".to_owned(), cte_index.to_string());
            }
            LogicalKind::Distinct { distinct_targets, .. } => {
                result.insert("Expressions".to_owned(), names(&self.expressions));
                if !distinct_targets.is_empty() {
                    result.insert("Distinct Targets".to_owned(), names(distinct_targets));
                }
            }
            LogicalKind::Order { orders, .. } => {
                let orders: Vec<ByteString> = orders.iter().map(|order| order.expression.name()).collect();
                result.insert("__order_by__".to_owned(), ByteString::join(&orders, b"\n"));
            }
            LogicalKind::MaterializedCte {
                ctename, table_index, ..
            }
            | LogicalKind::RecursiveCte {
                ctename, table_index, ..
            } => {
                result.insert("CTE Name".to_owned(), ctename.clone());
                result.insert("Table Index".to_owned(), table_index.to_string());
            }
            LogicalKind::Get(get) => {
                let filters: Vec<ByteString> = get
                    .table_filters
                    .filters
                    .iter()
                    .filter_map(|(&column, filter)| get.names.get(column).map(|name| filter.to_sql(name)))
                    .collect();
                result.insert("Filters".to_owned(), ByteString::join(&filters, b"\n"));
                if let Some(sample_options) = &get.sample_options {
                    result.insert(
                        "Sample Method".to_owned(),
                        format!(
                            "System: {}%",
                            sample_options.sample_size.to_varchar().unwrap_or_default()
                        ),
                    );
                }
                for (key, value) in get.file_filters.explain_params() {
                    result.insert(key, value);
                }
                if let (Some(table), Some((catalog, schema))) = (&get.table, &get.table_schema) {
                    result.insert(
                        "Table".to_owned(),
                        rustdb_ast::qualifier_to_string(catalog, schema, table.name()),
                    );
                    result.insert("Type".to_owned(), "Sequential Scan".to_owned());
                }
            }
            _ => {
                result.insert("Expressions".to_owned(), names(&self.expressions));
            }
        }
        self.set_params_estimated_cardinality(&mut result);
        result
    }

    /// The plan rendered for `EXPLAIN` (DuckDB's `LogicalOperator::ToString(format)`).
    pub fn to_explain_string(&self, format: ExplainFormat) -> Result<ByteString> {
        render(renderer_kind(format), RenderTree::create(&self))
    }
}

impl RenderSource for &LogicalOperator {
    fn render_node(&self) -> RenderTreeNode {
        RenderTreeNode::new(self.explain_name(), self.params_to_string())
    }

    fn render_children(&self) -> Vec<Self> {
        self.children.iter().collect()
    }
}
