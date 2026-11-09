//! Pushing SYSTEM samples into table scans (DuckDB's `SamplingPushdown`): a percentage
//! SYSTEM sample directly over a table scan becomes the scan skipping vectors.

use roastedduck_ast::SampleMethod;
use roastedduck_planner::{LogicalKind, LogicalOperator};

/// Moves the SYSTEM percentage samples of `op` into the scans below them (DuckDB's
/// `SamplingPushdown::Optimize`).
pub fn push_down_sampling(op: &mut LogicalOperator) {
    let pushable = match (&op.kind, op.children.as_slice()) {
        (LogicalKind::Sample { sample_options }, [child]) => {
            sample_options.method == SampleMethod::SystemSample
                && sample_options.is_percentage
                // Only table scans sample (`sampling_pushdown`).
                && matches!(&child.kind, LogicalKind::Get(get) if get.table.is_some())
                && child.children.is_empty()
        }
        _ => false,
    };
    if pushable && let Some(mut get) = op.children.pop() {
        let sample = std::mem::replace(&mut op.kind, LogicalKind::CrossProduct);
        if let (LogicalKind::Sample { sample_options }, LogicalKind::Get(scan)) = (sample, &mut get.kind) {
            scan.sample_options = Some(sample_options);
        }
        *op = get;
    }
    for child in &mut op.children {
        push_down_sampling(child);
    }
}
