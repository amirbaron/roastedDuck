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

    /// A binder for the expressions of generated columns (the `ExpressionBinder` of
    /// `Binder::BindGeneratedColumns`): column references expand like in CHECK
    /// constraints, without a target type.
    pub fn generated(
        binder: &'a mut Binder,
        outer: Option<&'a mut dyn Scope>,
        table: impl Into<String>,
        columns: &'a [CheckColumn],
    ) -> Self {
        Self {
            base: BinderBase::new(binder, outer),
            table: table.into(),
            columns,
            bound_columns: Vec::new(),
        }
    }

    /// Casts the bound expression to `target` (DuckDB's `target_type`).
    #[must_use]
    pub fn with_target_type(mut self, target: LogicalType) -> Self {
        self.base = self.base.with_target_type(target);
        self
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

/// Expressions over a projection being built (DuckDB's `ProjectionBinder`): every
/// column reference or subquery is pushed into the projection's expressions and replaced
/// by a reference to that projection column.
#[derive(Debug)]
pub struct ProjectionBinder<'a> {
    base: BinderBase<'a>,
    proj_index: u64,
    proj_expressions: &'a mut Vec<Expression>,
    /// The clause named in errors (`WHERE clause`, `Error Message`).
    clause: String,
    in_child_projection: bool,
}

impl<'a> ProjectionBinder<'a> {
    pub fn new(
        binder: &'a mut Binder,
        outer: Option<&'a mut dyn Scope>,
        proj_index: u64,
        proj_expressions: &'a mut Vec<Expression>,
        clause: impl Into<String>,
        target: LogicalType,
    ) -> Self {
        Self {
            base: BinderBase::new(binder, outer).with_target_type(target),
            proj_index,
            proj_expressions,
            clause: clause.into(),
            in_child_projection: false,
        }
    }

    /// `ProjectionBinder::BindColumnRef`.
    fn bind_projected(
        &mut self,
        expr: &mut ParsedExpression,
        depth: u64,
        root_expression: bool,
    ) -> Result<BindResult> {
        if self.in_child_projection {
            return bind_expression_base(self, expr, depth, root_expression);
        }
        self.in_child_projection = true;
        let result = bind_expression_base(self, expr, depth, root_expression);
        self.in_child_projection = false;
        let bound = match result? {
            BindResult::Bound(bound) => bound,
            unbound @ BindResult::Unbound(_) => return Ok(unbound),
        };
        if matches!(bound.kind, crate::expression::ExpressionKind::LambdaRef(_)) {
            return Ok(bound.into());
        }
        let reference = Expression::column_ref(
            bound.name(),
            bound.return_type.clone(),
            crate::ColumnBinding::new(self.proj_index, self.proj_expressions.len() as u64),
            0,
        );
        self.proj_expressions.push(bound);
        Ok(reference.into())
    }
}

expression_binder_base!(ProjectionBinder);

impl ExpressionBinder for ProjectionBinder<'_> {
    expression_binder_accessors!();

    fn bind_expression(
        &mut self,
        expr: &mut ParsedExpression,
        depth: u64,
        root_expression: bool,
    ) -> Result<BindResult> {
        match &expr.kind {
            ParsedExpressionKind::Default => {
                let message = format!("{} cannot contain DEFAULT clause", self.clause);
                crate::binder::expression::bind_unsupported_expression(self, expr, depth, &message)
            }
            ParsedExpressionKind::Window(_) => {
                let message = format!("{} cannot contain window functions!", self.clause);
                crate::binder::expression::bind_unsupported_expression(self, expr, depth, &message)
            }
            ParsedExpressionKind::ColumnRef(_) | ParsedExpressionKind::Subquery(_) => {
                self.bind_projected(expr, depth, root_expression)
            }
            _ => bind_expression_base(self, expr, depth, root_expression),
        }
    }

    fn unsupported_aggregate_message(&self) -> String {
        format!("{} cannot contain aggregate functions", self.clause)
    }
}

/// The `USING` expression of `ALTER TABLE ... ALTER COLUMN ... TYPE` (DuckDB's
/// `AlterBinder`): column references become references to the scanned columns, in the
/// order they are first referenced.
#[derive(Debug)]
pub struct AlterBinder<'a> {
    base: BinderBase<'a>,
    table: String,
    columns: &'a [CheckColumn],
    /// The physical columns read, one per reference.
    pub bound_columns: Vec<usize>,
}

impl<'a> AlterBinder<'a> {
    pub fn new(
        binder: &'a mut Binder,
        table: impl Into<String>,
        columns: &'a [CheckColumn],
        target: Option<LogicalType>,
    ) -> Self {
        let base = BinderBase::new(binder, None);
        let base = match target {
            Some(target) => base.with_target_type(target),
            None => base,
        };
        Self {
            base,
            table: table.into(),
            columns,
            bound_columns: Vec::new(),
        }
    }

    /// `AlterBinder::BindColumnReference`.
    fn bind_alter_column(&mut self, expr: &mut ParsedExpression, depth: u64) -> Result<BindResult> {
        let ParsedExpressionKind::ColumnRef(column) = &expr.kind else {
            return Err(Error::internal("expected a column reference"));
        };
        let names = column.column_names.clone();
        if names.len() > 1 {
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
                "Table does not contain column {name} referenced in alter statement!"
            )));
        };
        let Some(physical) = column.physical else {
            return Err(Error::binder(
                "Using generated columns in alter statement not supported",
            ));
        };
        self.bound_columns.push(physical);
        Ok(Expression::reference(
            column.name.clone(),
            column.logical_type.clone(),
            (self.bound_columns.len() - 1) as u64,
        )
        .into())
    }
}

expression_binder_base!(AlterBinder);

impl ExpressionBinder for AlterBinder<'_> {
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
                "window functions are not allowed in alter statement",
            ))),
            ParsedExpressionKind::Subquery(_) => Ok(BindResult::Unbound(unsupported(
                expr,
                "cannot use subquery in alter statement",
            ))),
            ParsedExpressionKind::Default => Ok(BindResult::Unbound(unsupported(
                expr,
                "default values are not supported in alter statement",
            ))),
            ParsedExpressionKind::ColumnRef(_) => self.bind_alter_column(expr, depth),
            _ => bind_expression_base(self, expr, depth, root_expression),
        }
    }

    fn unsupported_aggregate_message(&self) -> String {
        "aggregate functions are not allowed in alter statement".to_owned()
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
