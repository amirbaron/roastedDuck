//! The planner's simple cardinality estimates (DuckDB's `LogicalOperator::EstimateCardinality`
//! and its overrides). The join order optimizer stores better estimates in
//! [`LogicalOperator::estimated_cardinality`], which take precedence.

use super::{BoundLimitNode, LogicalKind, LogicalOperator};

impl LogicalOperator {
    /// The base estimate: the largest estimate of the children, cached in
    /// `estimated_cardinality` (DuckDB's `LogicalOperator::EstimateCardinality`).
    fn estimate_from_children(&mut self) -> u64 {
        if let Some(estimate) = self.estimated_cardinality {
            return estimate;
        }
        let estimate = self
            .children
            .iter_mut()
            .map(Self::estimate_cardinality)
            .max()
            .unwrap_or(0);
        self.estimated_cardinality = Some(estimate);
        estimate
    }

    /// The estimated number of output rows (DuckDB's `EstimateCardinality`). Like DuckDB,
    /// the overrides of some operators return their estimate without caching it.
    pub fn estimate_cardinality(&mut self) -> u64 {
        match &self.kind {
            LogicalKind::Aggregate(aggregate) if aggregate.groups.is_empty() => 1,
            LogicalKind::CopyToFile(_)
            | LogicalKind::CreateTable(_)
            | LogicalKind::Create { .. }
            | LogicalKind::CreateIndex(_)
            | LogicalKind::CreateSecret(_)
            | LogicalKind::Alter(_)
            | LogicalKind::Drop(_)
            | LogicalKind::Attach(_)
            | LogicalKind::Detach(_)
            | LogicalKind::Load(_)
            | LogicalKind::UpdateExtensions(_)
            | LogicalKind::AlterDatabase(_)
            | LogicalKind::Transaction { .. }
            | LogicalKind::Pragma { .. }
            | LogicalKind::Set { .. }
            | LogicalKind::Reset { .. }
            | LogicalKind::Prepare { .. }
            | LogicalKind::DummyScan { .. } => 1,
            LogicalKind::Insert(insert) if !insert.return_chunk => 1,
            LogicalKind::Delete(delete) if !delete.return_chunk => 1,
            LogicalKind::Update(update) if !update.return_chunk => 1,
            LogicalKind::MergeInto(merge) if !merge.return_chunk => 1,
            LogicalKind::EmptyResult { .. } => 0,
            LogicalKind::ExpressionGet { expressions, .. } => expressions.len() as u64,
            LogicalKind::Explain { .. } => 3,
            LogicalKind::Get(get) => {
                if let Some(estimate) = self.estimated_cardinality {
                    return estimate;
                }
                if let Some(rows) = get.table_cardinality() {
                    return rows;
                }
                self.children.first_mut().map_or(1, Self::estimate_cardinality)
            }
            LogicalKind::Sample { sample_options } => {
                let percentage = sample_options.is_percentage;
                let size = &sample_options.sample_size;
                let (percentage_value, size_value) = (
                    size.as_f64().unwrap_or(100.0),
                    size.as_i128().and_then(|v| u64::try_from(v).ok()),
                );
                let child = self.children.first_mut().map_or(0, Self::estimate_cardinality);
                if percentage {
                    let sampled = child as f64 * (percentage_value / 100.0);
                    if sampled > child as f64 {
                        child
                    } else {
                        sampled as u64
                    }
                } else {
                    match size_value {
                        Some(size) if size < child => size,
                        _ => child,
                    }
                }
            }
            LogicalKind::Limit { limit, .. } => {
                let limit = match limit {
                    BoundLimitNode::ConstantValue(value) => Some(Ok(*value)),
                    BoundLimitNode::ConstantPercentage(percentage) => Some(Err(*percentage)),
                    _ => None,
                };
                let child = self.children.first_mut().map_or(0, Self::estimate_cardinality);
                match limit {
                    Some(Ok(value)) => child.min(value),
                    Some(Err(percentage)) => (child as f64 * percentage) as u64,
                    None => child,
                }
            }
            LogicalKind::TopN { limit, .. } => {
                let limit = *limit;
                self.estimate_from_children().min(limit)
            }
            _ => self.estimate_from_children(),
        }
    }
}

impl super::LogicalGet {
    /// The estimated row count of the scan (DuckDB's `function.cardinality`): the row
    /// count of a base table (`TableScanCardinality`), or the function's estimate.
    pub fn table_cardinality(&self) -> Option<u64> {
        let Some(entry) = self.table.as_ref() else {
            let cardinality = self.function.cardinality.as_ref()?;
            return cardinality(self.bind_data.as_deref());
        };
        match &entry.kind {
            roastedduck_catalog::EntryKind::Table(table) => {
                table.storage.as_ref().map(|storage| storage.total_rows())
            }
            _ => None,
        }
    }
}
