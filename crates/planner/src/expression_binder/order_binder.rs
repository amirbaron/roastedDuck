//! ORDER BY and DISTINCT ON targets (DuckDB's `OrderBinder`). These bind only to the
//! select list: a term is a position in it, an alias of it, an expression equal to one of
//! its entries, or else a new entry appended to it. The result is a constant that names
//! the position; types are filled in once the select list is bound.

use rustdb_ast::{ExpressionClass, ParsedExpression, ParsedExpressionKind};
use rustdb_common::{Error, ErrorKind, Result};
use rustdb_types::Value;

use super::base_select_binder::SelectBindState;
use super::{is_potential_alias, qualify_column_names};
use crate::binder::Binder;
use crate::expression::Expression;

/// Binds ORDER BY / DISTINCT ON terms against a select list.
#[derive(Debug)]
pub struct OrderBinder<'a> {
    binders: Vec<&'a Binder>,
    /// The select list new terms can be appended to (absent for set operations).
    extra_list: Option<&'a mut Vec<ParsedExpression>>,
    bind_state: &'a mut SelectBindState,
    query_component: String,
}

impl<'a> OrderBinder<'a> {
    pub fn new(
        binders: Vec<&'a Binder>,
        extra_list: Option<&'a mut Vec<ParsedExpression>>,
        bind_state: &'a mut SelectBindState,
        query_component: &str,
    ) -> Self {
        Self {
            binders,
            extra_list,
            bind_state,
            query_component: query_component.to_owned(),
        }
    }

    /// Whether terms not in the select list can be added to it.
    pub fn has_extra_list(&self) -> bool {
        self.extra_list.is_some()
    }

    /// A constant naming select-list position `index` (DuckDB's `CreateProjectionReference`).
    fn projection_reference(&self, expr: &ParsedExpression, index: u64) -> Expression {
        let alias = match self
            .extra_list
            .as_deref()
            .and_then(|list| list.get(index as usize))
        {
            Some(entry) => entry.to_sql_bytes(),
            None => expr.alias.clone(),
        };
        let mut result = Expression::constant(Value::ubigint(index)).with_alias(alias);
        result.query_location = expr.query_location;
        result
    }

    /// Appends `expr` to the select list (DuckDB's `CreateExtraReference`).
    pub fn create_extra_reference(&mut self, expr: ParsedExpression) -> Result<Expression> {
        let Some(list) = self.extra_list.as_deref_mut() else {
            return Err(Error::internal("CreateExtraReference called without extra_list"));
        };
        let index = list.len();
        // Made before the term joins the list: named by the term's own alias.
        let mut reference = Expression::constant(Value::ubigint(index as u64)).with_alias(expr.alias.clone());
        reference.query_location = expr.query_location;
        self.bind_state.projection_map.insert(&expr, index);
        list.push(expr);
        Ok(reference)
    }

    /// The select-list position a term names directly (DuckDB's `TryGetProjectionReference`).
    fn projection_index(&self, expr: &ParsedExpression) -> Result<Option<u64>> {
        match &expr.kind {
            ParsedExpressionKind::Constant(constant) => {
                if !constant.value.logical_type().is_integral() {
                    let allowed = self
                        .binders
                        .first()
                        .is_some_and(|binder| binder.context.config.order_by_non_integer_literal);
                    if !allowed {
                        return Err(Error::binder(format!(
                            "{} non-integer literal has no effect.\n* SET order_by_non_integer_literal=true to allow \
                             this behavior.",
                            self.query_component
                        ))
                        .with_position(expr.query_location));
                    }
                    return Ok(None);
                }
                let value = constant.value.as_i128().unwrap_or_default();
                Ok(Some(if value <= 0 {
                    i64::MAX as u64
                } else {
                    (value - 1) as u64
                }))
            }
            ParsedExpressionKind::ColumnRef(column) => {
                if !is_potential_alias(&column.column_names) {
                    return Ok(None);
                }
                let Some(name) = column.column_names.last() else {
                    return Ok(None);
                };
                if let Some(&index) = self.bind_state.alias_map.get(name) {
                    return Ok(Some(index as u64));
                }
                let matching: Vec<usize> = self
                    .bind_state
                    .original_expressions
                    .iter()
                    .enumerate()
                    .filter(|(_, original)| match &original.kind {
                        ParsedExpressionKind::ColumnRef(original_column) => {
                            original.alias.is_empty() && original_column.column_names.last() == Some(name)
                        }
                        _ => false,
                    })
                    .map(|(i, _)| i)
                    .collect();
                Ok(match matching.as_slice() {
                    [index] => Some(*index as u64),
                    _ => None,
                })
            }
            ParsedExpressionKind::PositionalReference(reference) => Ok(Some(reference.index.wrapping_sub(1))),
            _ => Ok(None),
        }
    }

    /// Binds one term (DuckDB's `OrderBinder::Bind`); `None` drops the term (a
    /// non-integer constant when allowed).
    pub fn bind(&mut self, mut expr: ParsedExpression) -> Result<Option<Expression>> {
        match expr.expression_class() {
            ExpressionClass::Constant => {
                let Some(index) = self.projection_index(&expr)? else {
                    return Ok(None);
                };
                let mut result =
                    Expression::constant(Value::struct_value(vec![("index".into(), Value::ubigint(index))]))
                        .with_alias(expr.alias.clone());
                result.query_location = expr.query_location;
                return Ok(Some(result));
            }
            ExpressionClass::ColumnRef | ExpressionClass::PositionalReference => {
                if let Some(index) = self.projection_index(&expr)? {
                    return Ok(Some(self.projection_reference(&expr, index)));
                }
            }
            ExpressionClass::Parameter => {
                return Err(Error::new(
                    ErrorKind::ParameterNotAllowed,
                    format!("Parameter not supported in {} clause", self.query_component),
                ));
            }
            ExpressionClass::Collate => {
                if let ParsedExpressionKind::Collate(collate) = &expr.kind
                    && let Some(index) = self.projection_index(&collate.child)?
                {
                    return Ok(Some(Expression::constant(Value::struct_value(vec![
                        ("index".into(), Value::ubigint(index)),
                        ("collation".into(), Value::try_varchar(&collate.collation)?),
                    ]))));
                }
            }
            _ => {}
        }
        for binder in &self.binders {
            qualify_column_names(binder, &mut expr)?;
        }
        if let Some(&index) = self.bind_state.projection_map.get(&expr) {
            if index == usize::MAX {
                return Err(Error::binder("Ambiguous reference to column"));
            }
            return Ok(Some(self.projection_reference(&expr, index as u64)));
        }
        if self.extra_list.is_none() {
            return Err(Error::binder(format!(
                "Could not ORDER BY column \"{expr}\": add the expression/function to every SELECT, or move the \
                 UNION into a FROM clause."
            )));
        }
        self.create_extra_reference(expr).map(Some)
    }
}
