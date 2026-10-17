//! How a plan's output order must be kept when it runs on several threads (DuckDB's
//! `PhysicalPlanGenerator::PreserveInsertionOrder` / `UseBatchIndex`, `plan_insert.cpp`,
//! and `PhysicalOperator::GetSources`).

use crate::operator::{OrderPreservationType, PhysicalOperator};

/// The order of the rows `op` produces: that of the first source below it that does not
/// simply keep insertion order (`OrderPreservationRecursive`).
pub fn order_preservation_recursive(op: &dyn PhysicalOperator) -> OrderPreservationType {
    if op.as_source().is_some() {
        return op.source_order();
    }
    // A CTE's definition does not reach its output.
    let skip = usize::from(op.is_cte());
    for child in op.children().iter().skip(skip) {
        let order = order_preservation_recursive(child.as_ref());
        if order != OrderPreservationType::InsertionOrder {
            return order;
        }
    }
    OrderPreservationType::InsertionOrder
}

/// Whether the rows of `op` must arrive in order at a sink that cares about order
/// (`PreserveInsertionOrder`); `preserve_insertion_order` is the setting.
pub fn preserve_insertion_order(op: &dyn PhysicalOperator, preserve_insertion_order: bool) -> bool {
    match order_preservation_recursive(op) {
        OrderPreservationType::FixedOrder => true,
        OrderPreservationType::NoOrder => false,
        OrderPreservationType::InsertionOrder => preserve_insertion_order,
    }
}

/// Whether a sink above `op` can keep the order with batch indices instead of running
/// single-threaded (`UseBatchIndex`).
pub fn use_batch_index(op: &dyn PhysicalOperator, threads: usize) -> bool {
    // Batch indices only pay off with several threads.
    threads > 1 && all_sources_support_batch_index(op)
}

/// Whether every source of the pipelines producing `op`'s rows numbers its output in
/// batches (`AllSourcesSupportBatchIndex`).
pub fn all_sources_support_batch_index(op: &dyn PhysicalOperator) -> bool {
    let mut sources = Vec::new();
    collect_sources(op, &mut sources);
    sources.iter().all(|source| source.supports_batch_index())
}

/// The operators that start the pipelines feeding `op`'s output (`GetSources`): a sink
/// or a leaf is its own source, a streaming operator passes through to its input, a join
/// to its probe side (and itself when it emits unmatched build rows), a union to all its
/// inputs and a CTE to its query.
fn collect_sources<'a>(op: &'a dyn PhysicalOperator, out: &mut Vec<&'a dyn PhysicalOperator>) {
    let children = op.children();
    if op.is_union() {
        for child in children {
            collect_sources(child.as_ref(), out);
        }
    } else if op.is_cte() {
        if let Some(query) = children.get(1) {
            collect_sources(query.as_ref(), out);
        }
    } else if op.as_sink().is_some() && op.as_operator().is_some() {
        if let Some(probe) = children.first() {
            collect_sources(probe.as_ref(), out);
        }
        if op.as_source().is_some() {
            out.push(op);
        }
    } else if op.as_sink().is_some() || children.is_empty() {
        out.push(op);
    } else {
        collect_sources(children[0].as_ref(), out);
    }
}
