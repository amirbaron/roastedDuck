//! Prepared statements (DuckDB's `PreparedStatementData`, `StatementProperties`,
//! `BoundParameterMap`).

use std::sync::{Arc, Mutex, PoisonError};

use rustdb_ast::{SqlStatement, StatementType};
use rustdb_common::{CaseInsensitiveMap, Error, Result};
use rustdb_types::{LogicalType, LogicalTypeId, Value};

use crate::expression::{
    BoundParameterData, BoundParameterExpression, Expression, ExpressionKind, SharedParameterData,
};

/// What kind of result a statement returns (DuckDB's `StatementReturnType`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StatementReturnType {
    /// A query result with rows.
    #[default]
    QueryResult,
    /// The number of changed rows.
    ChangedRows,
    /// Nothing.
    Nothing,
}

/// Properties of a bound statement (DuckDB's `StatementProperties`).
// Mirrors DuckDB's `StatementProperties` field for field, flags included.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug)]
pub struct StatementProperties {
    /// Whether the statement must run inside a valid (not aborted) transaction.
    pub requires_valid_transaction: bool,
    /// Whether the result can be streamed.
    pub allow_stream_result: bool,
    /// Whether every parameter's type could be resolved.
    pub bound_all_parameters: bool,
    /// Whether re-executing requires binding again (volatile catalog state, ...).
    pub always_require_rebind: bool,
    pub return_type: StatementReturnType,
    /// The number of prepared statement parameters.
    pub parameter_count: usize,
    /// The databases that functions in the statement write to, in the order their
    /// calls were bound (`RegisterDBModify` by a function's `get_modified_databases`).
    pub modified_databases: Vec<String>,
}

impl Default for StatementProperties {
    fn default() -> Self {
        Self {
            requires_valid_transaction: true,
            allow_stream_result: false,
            bound_all_parameters: true,
            always_require_rebind: false,
            return_type: StatementReturnType::QueryResult,
            parameter_count: 0,
            modified_databases: Vec::new(),
        }
    }
}

/// A statement prepared with `PREPARE` (DuckDB's `PreparedStatementData`).
#[derive(Clone, Debug)]
pub struct PreparedStatementData {
    pub statement_type: StatementType,
    /// The statement as parsed, bound again for each execution.
    pub unbound_statement: SqlStatement,
    pub names: Vec<String>,
    pub types: Vec<LogicalType>,
    pub properties: StatementProperties,
    /// The parameters and their resolved types.
    pub value_map: CaseInsensitiveMap<SharedParameterData>,
    /// Bound plan awaiting physical-preparation metadata checks. Consumed when the
    /// client prepares it; execution still binds against current catalog state.
    pub preparation_plan: Option<crate::LogicalOperator>,
}

impl PreparedStatementData {
    /// Checks the number of supplied values (DuckDB's `CheckParameterCount`).
    pub fn check_parameter_count(&self, count: usize) -> Result<()> {
        let required = self.properties.parameter_count;
        if count != required {
            return Err(Error::binder(format!(
                "Parameter/argument count mismatch for prepared statement. Expected {required}, got {count}"
            )));
        }
        Ok(())
    }

    /// The type a parameter was resolved to, if any.
    pub fn parameter_type(&self, identifier: &str) -> Option<LogicalType> {
        self.value_map.get(identifier).map(|data| {
            data.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .return_type
                .clone()
        })
    }
}

/// The parameters of the statement being bound (DuckDB's `BoundParameterMap`).
#[derive(Debug, Default)]
pub struct BoundParameterMap {
    /// Values supplied up front (`EXECUTE` with values): bound as constants.
    pub parameter_data: CaseInsensitiveMap<BoundParameterData>,
    /// The parameters bound so far.
    pub parameters: CaseInsensitiveMap<SharedParameterData>,
    /// Whether the statement must be bound again once all types are known.
    pub rebind: bool,
}

impl BoundParameterMap {
    pub fn new(parameter_data: CaseInsensitiveMap<BoundParameterData>) -> Self {
        Self {
            parameter_data,
            parameters: CaseInsensitiveMap::default(),
            rebind: false,
        }
    }

    /// The type of a supplied parameter value, or `UNKNOWN`.
    pub fn return_type(&self, identifier: &str) -> LogicalType {
        self.parameter_data
            .get(identifier)
            .map_or(LogicalType::UNKNOWN, |data| data.return_type.clone())
    }

    /// A parameter expression sharing data with every other occurrence of `identifier`
    /// (DuckDB's `BindParameterExpression`).
    pub fn bind_parameter_expression(&mut self, identifier: &str, alias: &str) -> Expression {
        let identifier_type = self.return_type(identifier);
        let data = if let Some(data) = self.parameters.get(identifier) {
            Arc::clone(data)
        } else {
            let data = Arc::new(Mutex::new(BoundParameterData {
                value: Value::default(),
                return_type: identifier_type.clone(),
            }));
            self.parameters.insert(identifier, Arc::clone(&data));
            data
        };
        let param_type = data
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .return_type
            .clone();
        if identifier_type.id() == LogicalTypeId::Unknown && param_type.id() != LogicalTypeId::Unknown {
            self.rebind = true;
        }
        let mut expression = Expression::new(
            rustdb_ast::ExpressionType::ValueParameter,
            identifier_type,
            ExpressionKind::Parameter(BoundParameterExpression {
                identifier: identifier.to_owned(),
                parameter_data: data,
            }),
        );
        alias.clone_into(&mut expression.alias);
        expression
    }
}
