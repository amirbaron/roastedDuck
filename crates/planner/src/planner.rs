//! The entry point from statements to logical plans (DuckDB's `Planner`).

use std::collections::BTreeSet;
use std::sync::Arc;

use rustdb_ast::SqlStatement;
use rustdb_common::{CaseInsensitiveMap, Error, ErrorKind, Result};
use rustdb_types::LogicalType;

use crate::binder::statement::{BoundPragmaInfo, bind_statement};
use crate::binder::{BindScope, Binder, BindingMode};
use crate::context::ClientContext;
use crate::expression::BoundParameterData;
use crate::operator::LogicalOperator;
use crate::prepared::{BoundParameterMap, PreparedStatementData, StatementProperties};

/// A planned statement (the members DuckDB's `Planner` fills in).
#[derive(Debug)]
pub struct PlannedStatement {
    /// `None` when parameter types could not be resolved (the statement can only be
    /// prepared, not run).
    pub plan: Option<LogicalOperator>,
    pub names: Vec<rustdb_common::ByteString>,
    pub types: Vec<LogicalType>,
    pub properties: StatementProperties,
    /// The statement's parameters and their resolved types.
    pub parameters: CaseInsensitiveMap<crate::expression::SharedParameterData, rustdb_common::ByteString>,
    /// The number of table indexes the binder generated; DuckDB's optimizer generates
    /// further ones with the same binder (`Optimizer(*planner.binder, context)`).
    pub table_index_count: u64,
}

/// Binds statements into logical plans.
#[derive(Debug)]
pub struct Planner {
    context: Arc<ClientContext>,
}

/// DuckDB's `CheckTreeDepth`.
fn check_tree_depth(op: &LogicalOperator, max_depth: u64, depth: u64) -> Result<()> {
    if depth >= max_depth {
        return Err(Error::parser(format!(
            "Maximum tree depth of {max_depth} exceeded in logical planner"
        )));
    }
    op.children
        .iter()
        .try_for_each(|child| check_tree_depth(child, max_depth, depth + 1))
}

impl Planner {
    pub fn new(context: Arc<ClientContext>) -> Self {
        Self { context }
    }

    /// Binds and plans `statement` (DuckDB's `Planner::CreatePlan`), with values for
    /// parameters that are already known.
    pub fn create_plan_with_values(
        &self,
        mut statement: SqlStatement,
        values: CaseInsensitiveMap<BoundParameterData, rustdb_common::ByteString>,
    ) -> Result<PlannedStatement> {
        self.create_plan_with_values_mut(&mut statement, values, None)
    }

    /// Bind a relation while retaining injected external scan references in its tree.
    pub fn create_relation_plan(&self, statement: &mut SqlStatement) -> Result<PlannedStatement> {
        let mut bound = statement.clone();
        let mut replacements = Vec::new();
        let plan = self.create_plan_with_values_mut(
            &mut bound,
            CaseInsensitiveMap::default(),
            Some(&mut replacements),
        )?;
        if let rustdb_ast::SqlStatementKind::Select(original) = &mut statement.kind {
            rustdb_ast::retain_external_replacement_list(original, replacements);
        }
        Ok(plan)
    }

    fn create_plan_with_values_mut(
        &self,
        statement: &mut SqlStatement,
        values: CaseInsensitiveMap<BoundParameterData, rustdb_common::ByteString>,
        retain_external: Option<&mut Vec<rustdb_ast::TableRef>>,
    ) -> Result<PlannedStatement> {
        // The statements DuckDB's `Planner::CreatePlan` plans: all but the expanded ones
        // (`StatementTypeToString(MULTI_STATEMENT)` is `MULTI`).
        if let rustdb_ast::SqlStatementKind::Multi(_) = statement.kind {
            return Err(Error::not_implemented("Cannot plan statement of type MULTI!"));
        }
        // DuckDB counts the parameters the parser found, bound or not.
        let parameter_count = statement.named_param_map.len();
        let mut binder = Binder::new(Arc::clone(&self.context))?;
        if retain_external.is_some() {
            binder.capture_external_replacements();
        }
        binder.set_parameters(BoundParameterMap::new(values));
        let bound = {
            let mut scope = BindScope::root(&mut binder);
            bind_statement(&mut scope, statement)
        };
        if let Some(replacements) = retain_external {
            replacements.extend(binder.take_external_replacements());
        }
        let (mut plan, names, types, resolved) = match bound {
            Ok(bound) => (Some(bound.plan), bound.names, bound.types, true),
            Err(error) if error.kind() == ErrorKind::ParameterNotResolved => {
                (None, vec!["unknown".into()], vec![LogicalType::UNKNOWN], false)
            }
            Err(error) => return Err(error),
        };
        if let Some(bound_plan) = plan.take() {
            check_tree_depth(&bound_plan, self.context.config.max_expression_depth, 0)?;
            plan = Some(crate::subquery::decorrelate_independent(&binder, bound_plan)?);
        }
        let table_index_count = binder.table_index_count();
        let parameter_map = binder.take_parameters().unwrap_or_default();
        let mut properties = binder.properties();
        properties.parameter_count = parameter_count;
        properties.bound_all_parameters = !parameter_map.rebind && resolved;
        let mut parameters = CaseInsensitiveMap::default();
        for (identifier, data) in parameter_map.parameters {
            let mut locked = data.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if !locked.return_type.is_valid()
                || locked.return_type.id() == rustdb_types::LogicalTypeId::Unknown
            {
                properties.bound_all_parameters = false;
                drop(locked);
                continue;
            }
            locked.value = rustdb_types::Value::null(locked.return_type.clone());
            drop(locked);
            parameters.insert(identifier, data);
        }
        Ok(PlannedStatement {
            plan,
            names,
            types,
            properties,
            parameters,
            table_index_count,
        })
    }

    /// The tables `statement` uses, by name or by qualified name (the binding in DuckDB's
    /// `ClientContext::GetTableNames`).
    pub fn table_names(
        &self,
        mut statement: SqlStatement,
        qualified: bool,
    ) -> Result<BTreeSet<rustdb_common::ByteString>> {
        let mut binder = Binder::new(Arc::clone(&self.context))?;
        binder.set_binding_mode(if qualified {
            BindingMode::ExtractQualifiedNames
        } else {
            BindingMode::ExtractNames
        });
        bind_statement(&mut BindScope::root(&mut binder), &mut statement)?;
        Ok(binder.take_table_names())
    }

    /// Binds and plans `statement` (DuckDB's `Planner::CreatePlan`).
    /// Binds a pragma's overload and arguments (DuckDB's `Binder::BindPragma`, which the
    /// `PragmaHandler` calls before replacing a pragma by its query).
    pub fn bind_pragma(&self, pragma: &mut rustdb_ast::PragmaInfo) -> Result<BoundPragmaInfo> {
        let mut binder = Binder::new(Arc::clone(&self.context))?;
        let mut scope = BindScope::root(&mut binder);
        crate::binder::statement::bind_pragma_info(&mut scope, pragma)
    }

    pub fn create_plan(&self, statement: SqlStatement) -> Result<PlannedStatement> {
        self.create_plan_with_values(statement, CaseInsensitiveMap::default())
    }

    /// Prepares `statement` (DuckDB's `Planner::PrepareSQLStatement`).
    pub fn prepare(&self, statement: SqlStatement) -> Result<PreparedStatementData> {
        let mut planned = self.create_plan(statement.clone())?;
        let mut prepared = Self::prepared_data(statement, &planned);
        prepared.preparation_plan = planned.plan.take();
        Ok(prepared)
    }

    /// Plans a prepared statement again with parameter values, as `EXECUTE` does.
    pub fn prepare_with_values(
        &self,
        statement: SqlStatement,
        values: CaseInsensitiveMap<BoundParameterData, rustdb_common::ByteString>,
    ) -> Result<(PreparedStatementData, LogicalOperator)> {
        let planned = self.create_plan_with_values(statement.clone(), values)?;
        let prepared = Self::prepared_data(statement, &planned);
        let plan = planned
            .plan
            .ok_or_else(|| Error::internal("prepared statement without a plan"))?;
        Ok((prepared, plan))
    }

    fn prepared_data(statement: SqlStatement, planned: &PlannedStatement) -> PreparedStatementData {
        PreparedStatementData {
            statement_type: statement.statement_type(),
            unbound_statement: statement,
            names: planned.names.clone(),
            types: planned.types.clone(),
            properties: planned.properties.clone(),
            value_map: planned.parameters.clone(),
            preparation_plan: None,
        }
    }
}
