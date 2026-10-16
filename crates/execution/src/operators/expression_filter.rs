//! Evaluation of expression filters pushed into table scans (DuckDB's
//! `ExpressionFilter`, executed by the scan through an `ExpressionExecutor`).

use std::sync::Arc;

use rustdb_common::Result;
use rustdb_planner::operator::ExpressionTableFilter;
use rustdb_storage::TableFilter;
use rustdb_storage::table_filter::ExpressionFilter;
use rustdb_types::{DataChunk, SelectionVector, Vector};

use crate::ExpressionExecutor;

/// An expression filter that evaluates its predicate over the scanned column.
#[derive(Debug)]
pub struct ExecutableExpressionFilter {
    filter: ExpressionTableFilter,
}

impl ExpressionFilter for ExecutableExpressionFilter {
    fn select(&self, vector: &Vector, count: usize, sel: &mut Vec<u32>) -> Result<()> {
        if sel.is_empty() {
            return Ok(());
        }
        let input = if sel.len() == count && sel.iter().enumerate().all(|(i, &row)| row as usize == i) {
            vector.clone()
        } else {
            vector.slice(&SelectionVector::new(sel.clone()))
        };
        let chunk = DataChunk::new(vec![input], sel.len());
        let mut executor = ExpressionExecutor::new(vec![self.filter.expression.clone()]);
        let mut true_sel = Vec::new();
        executor.select(&chunk, &mut true_sel)?;
        let selected: Vec<u32> = true_sel.into_iter().map(|row| sel[row as usize]).collect();
        *sel = selected;
        Ok(())
    }

    fn to_sql(&self, column_name: &str) -> String {
        self.filter.to_sql(column_name)
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// Replaces the planner's expression filters in `filter` by executable ones.
pub fn bind_expression_filters(filter: TableFilter) -> TableFilter {
    match filter {
        TableFilter::Expression(expression) => {
            match expression.as_any().downcast_ref::<ExpressionTableFilter>() {
                Some(planned) => TableFilter::Expression(Arc::new(ExecutableExpressionFilter {
                    filter: planned.clone(),
                })),
                None => TableFilter::Expression(expression),
            }
        }
        TableFilter::And(children) => {
            TableFilter::And(children.into_iter().map(bind_expression_filters).collect())
        }
        TableFilter::Or(children) => {
            TableFilter::Or(children.into_iter().map(bind_expression_filters).collect())
        }
        TableFilter::Optional(child) => {
            TableFilter::Optional(child.map(|child| Box::new(bind_expression_filters(*child))))
        }
        TableFilter::StructExtract {
            child_index,
            child_name,
            child_filter,
        } => TableFilter::StructExtract {
            child_index,
            child_name,
            child_filter: Box::new(bind_expression_filters(*child_filter)),
        },
        other => other,
    }
}
