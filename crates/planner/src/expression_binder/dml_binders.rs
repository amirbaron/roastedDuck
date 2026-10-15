//! Expression binders of DDL and DML statements (DuckDB's `CheckBinder`,
//! `UpdateBinder` and `ReturningBinder`).

use rustdb_ast::{ParsedExpression, ParsedExpressionKind};
use rustdb_common::{Error, Result};
use rustdb_types::LogicalType;

use super::base::{BinderBase, expression_binder_accessors, expression_binder_base};
use super::{BindResult, ExpressionBinder, unsupported};
use crate::binder::expression::bind_expression_base;
use crate::binder::{Binder, Scope};
use crate::expression::Expression;

/// A column of the table a CHECK constraint belongs to.
#[derive(Clone, Debug)]
pub struct CheckColumn {
    pub name: String,
    pub logical_type: LogicalType,
    /// The physical index; `None` for generated columns.
    pub physical: Option<usize>,
    /// The expression of a generated column.
    pub generated: Option<ParsedExpression>,
}

/// CHECK constraints (DuckDB's `CheckBinder`): column references become references to
/// the physical columns of the table's chunks.
#[derive(Debug)]
pub struct CheckBinder<'a> {
    base: BinderBase<'a>,
    table: String,
    columns: &'a [CheckColumn],
    /// The physical columns the constraint reads.
    pub bound_columns: Vec<usize>,
}

impl<'a> CheckBinder<'a> {
    pub fn new(
        binder: &'a mut Binder,
        outer: Option<&'a mut dyn Scope>,
        table: impl Into<String>,
        columns: &'a [CheckColumn],
    ) -> Self {
        Self {
            base: BinderBase::new(binder, outer).with_target_type(LogicalType::INTEGER),
            table: table.into(),
            columns,
            bound_columns: Vec::new(),
        }
    }

    /// `CheckBinder::BindCheckColumn`.
    fn bind_check_column(&mut self, expr: &mut ParsedExpression, depth: u64) -> Result<BindResult> {
        let ParsedExpressionKind::ColumnRef(column) = &expr.kind else {
            return Err(Error::internal("expected a column reference"));
        };
        let names = column.column_names.clone();
        if names.len() > 1 {
            // `BindQualifiedColumnName`: drop a leading table name, the rest are fields.
            let start = usize::from(names[0] == self.table);
            let mut result = ParsedExpression::column_ref(vec![names[names.len() - 1].clone()]);
            for field in &names[start..names.len() - 1] {
                result = super::create_struct_extract(result, field);
            }
            return self.bind_expression(&mut result, depth, false);
        }
        let name = &names[0];
        let Some(column) = self
            .columns
            .iter()
            .find(|column| column.name.eq_ignore_ascii_case(name))
        else {
            return Err(Error::binder(format!(
                "Table does not contain column {name} referenced in check constraint!"
            )));
        };
        if let Some(generated) = &column.generated {
            let mut generated = generated.clone();
            return self.bind_expression(&mut generated, 0, false);
        }
        let physical = column
            .physical
            .ok_or_else(|| Error::internal("CHECK column without a physical index"))?;
        if !self.bound_columns.contains(&physical) {
            self.bound_columns.push(physical);
        }
        Ok(Expression::reference(column.name.clone(), column.logical_type.clone(), physical as u64).into())
    }
}

expression_binder_base!(CheckBinder);

impl ExpressionBinder for CheckBinder<'_> {
    expression_binder_accessors!();

    fn bind_expression(
        &mut self,
        expr: &mut ParsedExpression,
        depth: u64,
        root_expression: bool,
    ) -> Result<BindResult> {
        match &expr.kind {
            ParsedExpressionKind::Window(_) => Ok(BindResult::error(
                "window functions are not allowed in check constraints",
            )),
            ParsedExpressionKind::Subquery(_) => {
                Ok(BindResult::error("cannot use subquery in check constraint"))
            }
            ParsedExpressionKind::ColumnRef(_) => self.bind_check_column(expr, depth),
            _ => bind_expression_base(self, expr, depth, root_expression),
        }
    }

    fn unsupported_aggregate_message(&self) -> String {
        "aggregate functions are not allowed in check constraints".to_owned()
    }
}

/// The `SET` expressions of an `UPDATE` (DuckDB's `UpdateBinder`).
#[derive(Debug)]
pub struct UpdateBinder<'a> {
    base: BinderBase<'a>,
}

impl<'a> UpdateBinder<'a> {
    pub fn new(binder: &'a mut Binder, outer: Option<&'a mut dyn Scope>, target: LogicalType) -> Self {
        Self {
            base: BinderBase::new(binder, outer).with_target_type(target),
        }
    }
}

expression_binder_base!(UpdateBinder);

impl ExpressionBinder for UpdateBinder<'_> {
    expression_binder_accessors!();

    fn bind_expression(
        &mut self,
        expr: &mut ParsedExpression,
        depth: u64,
        root_expression: bool,
    ) -> Result<BindResult> {
        match &expr.kind {
            ParsedExpressionKind::Window(_) => Ok(BindResult::Unbound(unsupported(
                expr,
                "window functions are not allowed in UPDATE",
            ))),
            _ => bind_expression_base(self, expr, depth, root_expression),
        }
    }

    fn unsupported_aggregate_message(&self) -> String {
        "aggregate functions are not allowed in UPDATE".to_owned()
    }
}

/// The key expressions of `CREATE INDEX` (DuckDB's `IndexBinder`), bound against the
/// indexed table.
#[derive(Debug)]
pub struct IndexBinder<'a> {
    base: BinderBase<'a>,
}

impl<'a> IndexBinder<'a> {
    pub fn new(binder: &'a mut Binder, outer: Option<&'a mut dyn Scope>) -> Self {
        Self {
            base: BinderBase::new(binder, outer),
        }
    }
}

expression_binder_base!(IndexBinder);

impl ExpressionBinder for IndexBinder<'_> {
    expression_binder_accessors!();

    fn bind_expression(
        &mut self,
        expr: &mut ParsedExpression,
        depth: u64,
        root_expression: bool,
    ) -> Result<BindResult> {
        match &expr.kind {
            ParsedExpressionKind::Window(_) => Ok(BindResult::Unbound(unsupported(
                expr,
                "window functions are not allowed in index expressions",
            ))),
            ParsedExpressionKind::Subquery(_) => Ok(BindResult::Unbound(unsupported(
                expr,
                "cannot use subquery in index expressions",
            ))),
            _ => bind_expression_base(self, expr, depth, root_expression),
        }
    }

    fn unsupported_aggregate_message(&self) -> String {
        "aggregate functions are not allowed in index expressions".to_owned()
    }
}

/// The `RETURNING` list (DuckDB's `ReturningBinder`).
#[derive(Debug)]
pub struct ReturningBinder<'a> {
    base: BinderBase<'a>,
}

impl<'a> ReturningBinder<'a> {
    pub fn new(binder: &'a mut Binder, outer: Option<&'a mut dyn Scope>) -> Self {
        Self {
            base: BinderBase::new(binder, outer),
        }
    }
}

expression_binder_base!(ReturningBinder);

impl ExpressionBinder for ReturningBinder<'_> {
    expression_binder_accessors!();

    fn bind_expression(
        &mut self,
        expr: &mut ParsedExpression,
        depth: u64,
        root_expression: bool,
    ) -> Result<BindResult> {
        match &expr.kind {
            ParsedExpressionKind::Subquery(_) => Ok(BindResult::Unbound(unsupported(
                expr,
                "SUBQUERY is not supported in returning statements",
            ))),
            _ => bind_expression_base(self, expr, depth, root_expression),
        }
    }
}
