//! The table indexes an operator introduces (DuckDB's `LogicalOperator::GetTableIndex`).

use roastedduck_ast::JoinType;

use super::{LogicalKind, LogicalOperator};

impl LogicalOperator {
    /// The table indexes of the columns this operator creates (DuckDB's
    /// `GetTableIndex`); empty for operators that only pass columns through.
    pub fn table_indexes(&self) -> Vec<u64> {
        match &self.kind {
            LogicalKind::Aggregate(aggregate) => {
                let mut result = vec![aggregate.group_index, aggregate.aggregate_index];
                if aggregate.groupings_index != u64::MAX {
                    result.push(aggregate.groupings_index);
                }
                result
            }
            LogicalKind::Get(get) => vec![get.table_index],
            LogicalKind::Projection { table_index }
            | LogicalKind::ChunkGet { table_index, .. }
            | LogicalKind::DelimGet { table_index, .. }
            | LogicalKind::ExpressionGet { table_index, .. }
            | LogicalKind::DummyScan { table_index }
            | LogicalKind::CteRef { table_index, .. }
            | LogicalKind::SetOperation { table_index, .. }
            | LogicalKind::RecursiveCte { table_index, .. }
            | LogicalKind::MaterializedCte { table_index, .. } => vec![*table_index],
            LogicalKind::Window { window_index } => vec![*window_index],
            LogicalKind::Unnest { unnest_index } => vec![*unnest_index],
            LogicalKind::Pivot { pivot_index, .. } => vec![*pivot_index],
            LogicalKind::Insert(insert) => vec![insert.table_index],
            LogicalKind::Delete(delete) => vec![delete.table_index],
            LogicalKind::MergeInto(merge) => vec![merge.table_index],
            LogicalKind::ComparisonJoin(join) if join.join_type == JoinType::Mark => vec![join.mark_index],
            LogicalKind::DependentJoin(join) if join.join.join_type == JoinType::Mark => {
                vec![join.join.mark_index]
            }
            _ => Vec::new(),
        }
    }
}
