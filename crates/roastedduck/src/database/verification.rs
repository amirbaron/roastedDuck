//! The regular SELECT verifiers from DuckDB 1.5.6's `client_verify.cpp`.
use std::collections::HashMap;
use std::sync::atomic::Ordering;

use roastedduck_ast::{
    OrderByNode, ParsedExpression, ParsedExpressionKind, QueryNode, QueryNodeKind, ResultModifier, SqlStatementKind,
    TableRef, TableRefKind,
};
use roastedduck_common::{ByteString, byte_format, byte_write};
use roastedduck_function::cast::values_are_equal;
use roastedduck_serde::binary::{BinaryDeserializer, BinarySerializer};
use roastedduck_serde::{SerializationCompatibility, SerializationOptions};
use roastedduck_types::Vector;

use super::{CastFunctionSet, Connection, Error, ErrorKind, MaterializedResult, Result, SqlStatement, Value};

const PREPARED_NAME: &str = "__duckdb_verification_prepared_statement";

#[derive(Clone, Copy)]
struct SavedSettings {
    optimizer_disabled: bool,
    profiler_enabled: bool,
    force_external: bool,
}

impl Connection {
    pub(super) fn verify_statement(&mut self, statement: &SqlStatement) -> Result<()> {
        if !self.settings.query_verification || self.verification_running {
            return Ok(());
        }
        let SqlStatementKind::Select(select) = &statement.kind else {
            return Ok(());
        };
        let serialized = BinarySerializer::serialize(
            select,
            SerializationOptions {
                compatibility: SerializationCompatibility::latest(),
                ..SerializationOptions::default()
            },
        )?;
        let select: roastedduck_ast::SelectStatement = BinaryDeserializer::deserialize(&serialized)?;
        let mut deserialized = SqlStatement::new(SqlStatementKind::Select(select.clone()));
        deserialized.named_param_map = select.named_param_map;
        let saved = SavedSettings {
            optimizer_disabled: self.settings.optimizer_disabled,
            profiler_enabled: self.settings.profiler.enable_profiler,
            force_external: self.settings.force_external(),
        };
        self.verification_running = true;
        self.settings.profiler.enable_profiler = false;
        let outcome = self.verify_select(statement, deserialized, saved);
        self.verification_running = false;
        outcome
    }

    fn verifier_run(
        &mut self,
        statement: SqlStatement,
        unoptimized: bool,
        no_cache: bool,
    ) -> Result<MaterializedResult> {
        self.settings.optimizer_disabled = unoptimized;
        self.settings
            .set("enable_caching_operators", &Value::boolean(!no_cache))?;
        self.settings
            .set("debug_force_external", &Value::boolean(false))?;
        self.client.interrupt_flag().store(false, Ordering::Release);
        let query = statement.query.clone();
        let result = self.execute_statement(query, statement);
        self.client.interrupt_flag().store(false, Ordering::Release);
        result
    }

    fn verify_select(
        &mut self,
        statement: &SqlStatement,
        deserialized: SqlStatement,
        saved: SavedSettings,
    ) -> Result<()> {
        let original = self.verifier_run(statement.clone(), false, false);
        let mut others = Vec::new();
        for (name, candidate, unoptimized, no_cache) in [
            ("Copied", statement.clone(), false, false),
            ("Deserialized", deserialized, false, false),
            ("Unoptimized", statement.clone(), true, false),
            ("No operator caching", statement.clone(), false, true),
        ] {
            others.push((name, self.verifier_run(candidate, unoptimized, no_cache)));
        }
        if original.is_ok() {
            let SqlStatementKind::Select(select) = &statement.kind else {
                unreachable!()
            };
            let rendered = select.to_sql_bytes();
            let mut parsed = roastedduck_parser::parse_bytes(&rendered).map_err(|error| {
                Error::internal(byte_format!(
                    "Parsed statement verification failed. Query:\n{rendered}\n\nError: {error}"
                ))
            })?;
            if parsed.len() != 1 {
                return Err(Error::internal("Parsed verifier did not produce one statement"));
            }
            others.push(("Parsed", self.verifier_run(parsed.remove(0), false, false)));
        }
        let any_failed = original.is_err() || others.iter().any(|(_, result)| result.is_err());
        if !any_failed && statement.named_param_map.is_empty() {
            match self.verify_prepared(statement.clone()) {
                Ok(result) => others.push(("Prepared", Ok(result))),
                Err(error) if error.kind() == ErrorKind::Internal => return Err(error),
                Err(_) => {} // Unsupported prepared forms are excluded by upstream too.
            }
        } else if self.transactions.is_invalidated()
            || self.database.instance.validity.invalidated().is_some()
        {
            return original.map(|_| ());
        }
        self.settings.optimizer_disabled = saved.optimizer_disabled;
        self.settings
            .set("debug_force_external", &Value::boolean(saved.force_external))?;
        if original.is_ok() {
            let explain = SqlStatement::new(SqlStatementKind::Explain(roastedduck_ast::ExplainStatement {
                statement: Box::new(statement.clone()),
                explain_type: roastedduck_ast::ExplainType::ExplainStandard,
                explain_format: roastedduck_ast::ExplainFormat::Default,
            }));
            if let Err(error) = self.verifier_run(explain, false, false) {
                return Err(Error::new_bytes(
                    error.kind(),
                    byte_format!("Query succeeded but EXPLAIN failed with: {}", error.raw_message()),
                ));
            }
        }
        if saved.profiler_enabled {
            self.settings.profiler.enable_profiler = true;
        }
        for (name, other) in others {
            compare(name, &original, &other)?;
        }
        Ok(())
    }

    fn verify_prepared(&mut self, mut statement: SqlStatement) -> Result<MaterializedResult> {
        let mut constants = Constants::default();
        if let SqlStatementKind::Select(select) = &mut statement.kind {
            constants.query_node(&mut select.node);
            for (name, _) in &constants.values {
                statement.named_param_map.insert(name.clone(), 0);
                select.named_param_map.insert(name.clone(), 0);
            }
        }
        let prepare = SqlStatement::new(SqlStatementKind::Prepare(roastedduck_ast::PrepareStatement {
            name: PREPARED_NAME.into(),
            statement: Box::new(statement),
        }));
        let mut named_values = roastedduck_common::CaseInsensitiveMap::default();
        for (name, value) in constants.values {
            named_values.insert(name, value);
        }
        let execute = SqlStatement::new(SqlStatementKind::Execute(roastedduck_ast::ExecuteStatement {
            name: PREPARED_NAME.into(),
            named_values,
        }));
        let result = self
            .execute_statement(b"", prepare)
            .and_then(|_| self.execute_statement(b"", execute));
        // Deallocate even when preparation/execution failed, as upstream does.
        let _ = self.query(format!("DEALLOCATE {PREPARED_NAME}"));
        self.client.interrupt_flag().store(false, Ordering::Release);
        result
    }
}

/// `PreparedStatementVerifier::ConvertConstants`, visiting what
/// `ParsedExpressionIterator` enumerates, in its order.
#[derive(Default)]
struct Constants {
    values: Vec<(ByteString, ParsedExpression)>,
}

impl Constants {
    fn all(&mut self, expressions: &mut [ParsedExpression]) {
        for expression in expressions {
            self.expression(expression);
        }
    }

    fn optional(&mut self, expression: &mut Option<Box<ParsedExpression>>) {
        if let Some(expression) = expression {
            self.expression(expression);
        }
    }

    fn orders(&mut self, orders: &mut [OrderByNode]) {
        for order in orders {
            self.expression(&mut order.expression);
        }
    }

    /// `EnumerateQueryNodeChildren`.
    fn query_node(&mut self, node: &mut QueryNode) {
        match &mut node.kind {
            QueryNodeKind::RecursiveCte(cte) => {
                self.query_node(&mut cte.left);
                self.query_node(&mut cte.right);
            }
            QueryNodeKind::Select(select) => {
                self.all(&mut select.select_list);
                self.all(&mut select.group_expressions);
                self.optional(&mut select.where_clause);
                self.optional(&mut select.having);
                self.optional(&mut select.qualify);
                self.table_ref(&mut select.from_table);
            }
            QueryNodeKind::SetOperation(set_operation) => {
                for child in &mut set_operation.children {
                    self.query_node(child);
                }
            }
        }
        for modifier in &mut node.modifiers {
            match modifier {
                ResultModifier::Limit(limit) => {
                    self.optional(&mut limit.limit);
                    self.optional(&mut limit.offset);
                }
                ResultModifier::LimitPercent(limit) => {
                    self.optional(&mut limit.limit);
                    self.optional(&mut limit.offset);
                }
                ResultModifier::Order(order) => self.orders(&mut order.orders),
                ResultModifier::Distinct(distinct) => self.all(&mut distinct.distinct_on_targets),
            }
        }
        for (_, cte) in node.cte_map.map.iter_mut() {
            self.query_node(&mut cte.query.node);
        }
    }

    /// `EnumerateTableRefChildren`. Base tables and SHOW/SUMMARIZE references are
    /// not unfolded, so the names SUMMARIZE returns as data keep their constants.
    fn table_ref(&mut self, table: &mut TableRef) {
        match &mut table.kind {
            TableRefKind::ExpressionList(list) => {
                for row in &mut list.values {
                    self.all(row);
                }
            }
            TableRefKind::Join(join) => {
                self.table_ref(&mut join.left);
                self.table_ref(&mut join.right);
                self.optional(&mut join.condition);
            }
            TableRefKind::Pivot(pivot) => {
                self.table_ref(&mut pivot.source);
                self.all(&mut pivot.aggregates);
            }
            TableRefKind::Subquery(subquery) => self.query_node(&mut subquery.subquery.node),
            TableRefKind::TableFunction(function) => self.expression(&mut function.function),
            TableRefKind::BaseTable(_)
            | TableRefKind::Empty
            | TableRefKind::Show(_)
            | TableRefKind::ColumnData(_) => {}
        }
    }

    /// `ConvertConstants` over `EnumerateChildren`. A subquery expression exposes
    /// only its comparison child; the query inside it keeps its constants.
    fn expression(&mut self, expression: &mut ParsedExpression) {
        match &mut expression.kind {
            ParsedExpressionKind::Constant(constant) => {
                let index = self
                    .values
                    .iter()
                    .position(|(_, existing)| {
                        matches!(&existing.kind, ParsedExpressionKind::Constant(value) if value.value == constant.value)
                    })
                    .unwrap_or(self.values.len());
                let identifier: ByteString = (index + 1).to_string().into();
                if index == self.values.len() {
                    let mut value = expression.clone();
                    value.alias.clear();
                    self.values.push((identifier.clone(), value));
                }
                *expression = ParsedExpression {
                    expression_type: roastedduck_ast::ExpressionType::ValueParameter,
                    alias: expression.alias.clone(),
                    query_location: None,
                    kind: ParsedExpressionKind::Parameter(roastedduck_ast::ParameterExpression { identifier }),
                };
            }
            ParsedExpressionKind::Between(between) => {
                self.expression(&mut between.input);
                self.expression(&mut between.lower);
                self.expression(&mut between.upper);
            }
            ParsedExpressionKind::Case(case) => {
                for check in &mut case.case_checks {
                    self.expression(&mut check.when_expr);
                    self.expression(&mut check.then_expr);
                }
                self.expression(&mut case.else_expr);
            }
            ParsedExpressionKind::Cast(cast) => self.expression(&mut cast.child),
            ParsedExpressionKind::Collate(collate) => self.expression(&mut collate.child),
            ParsedExpressionKind::Comparison(comparison) => {
                self.expression(&mut comparison.left);
                self.expression(&mut comparison.right);
            }
            ParsedExpressionKind::Conjunction(conjunction) => self.all(&mut conjunction.children),
            ParsedExpressionKind::Function(function) => {
                self.all(&mut function.children);
                self.optional(&mut function.filter);
                self.orders(&mut function.order_bys.orders);
            }
            ParsedExpressionKind::Type(ty) => self.all(&mut ty.children),
            ParsedExpressionKind::Lambda(lambda) => {
                self.expression(&mut lambda.lhs);
                self.expression(&mut lambda.expr);
            }
            ParsedExpressionKind::Operator(operator) => self.all(&mut operator.children),
            ParsedExpressionKind::Star(star) => {
                self.optional(&mut star.expr);
                for (_, replacement) in star.replace_list.iter_mut() {
                    self.expression(replacement);
                }
            }
            ParsedExpressionKind::Subquery(subquery) => self.optional(&mut subquery.child),
            ParsedExpressionKind::Window(window) => {
                self.all(&mut window.partitions);
                self.orders(&mut window.orders);
                self.all(&mut window.children);
                self.optional(&mut window.filter_expr);
                self.optional(&mut window.start_expr);
                self.optional(&mut window.end_expr);
                self.optional(&mut window.offset_expr);
                self.optional(&mut window.default_expr);
                self.orders(&mut window.arg_orders);
            }
            ParsedExpressionKind::ColumnRef(_)
            | ParsedExpressionKind::Default
            | ParsedExpressionKind::LambdaRef(_)
            | ParsedExpressionKind::Parameter(_)
            | ParsedExpressionKind::PositionalReference(_)
            | ParsedExpressionKind::Bound(_) => {}
        }
    }
}

fn hash(value: &Value) -> Result<u64> {
    let vector = Vector::constant(value)?;
    let mut out = [0];
    roastedduck_row::hash::hash_selected(&vector, &[0], &mut out)?;
    Ok(out[0])
}

fn compare(
    name: &str,
    original: &Result<MaterializedResult>,
    other: &Result<MaterializedResult>,
) -> Result<()> {
    let mismatch = match (original, other) {
        (Err(_), Err(_)) => return Ok(()),
        (Ok(left), Ok(right)) => compare_rows(left, right)?,
        _ => Some(ByteString::new()),
    };
    let Some(detail) = mismatch else {
        return Ok(());
    };
    let mut message = byte_format!(
        "{name} statement differs from original result!\nOriginal Result:\n{}{name}:\n{}",
        render(original)?,
        render(other)?
    );
    if !detail.is_empty() {
        let _ = byte_write!(&mut message, "\n\n---------------------------------\n{detail}");
    }
    Err(Error::new_bytes(ErrorKind::Invalid, message))
}

fn compare_rows(left: &MaterializedResult, right: &MaterializedResult) -> Result<Option<ByteString>> {
    if left.column_count() != right.column_count() {
        return Ok(Some(byte_format!(
            "Column count mismatch ({} vs {})",
            left.row_count(),
            right.row_count()
        )));
    }
    if left.row_count() != right.row_count() {
        return Ok(Some(byte_format!(
            "Row count mismatch ({} vs {})",
            left.row_count(),
            right.row_count()
        )));
    }
    let casts = CastFunctionSet::default();
    let mut mismatch = None;
    'ordered: for row in 0..left.row_count() {
        for column in 0..left.column_count() {
            let a = left.value(column, row);
            let b = right.value(column, row);
            if !values_are_equal(&casts, &a, &b)? {
                mismatch = Some(byte_format!(
                    "{} <> {} (row: {row}, col: {column})\n",
                    a.to_varchar_bytes()?,
                    b.to_varchar_bytes()?
                ));
                break 'ordered;
            }
        }
    }
    if mismatch.is_none() {
        return Ok(None);
    }
    // Upstream falls back to an unordered multiset comparison per column.
    for column in 0..left.column_count() {
        let mut values: HashMap<u64, Vec<Value>> = HashMap::new();
        for row in 0..left.row_count() {
            let value = left.value(column, row);
            values.entry(hash(&value)?).or_default().push(value);
        }
        for row in 0..right.row_count() {
            let value = right.value(column, row);
            let Some(bucket) = values.get_mut(&hash(&value)?) else {
                return Ok(mismatch);
            };
            let mut found = None;
            for (index, candidate) in bucket.iter().enumerate() {
                if values_are_equal(&casts, candidate, &value)? {
                    found = Some(index);
                    break;
                }
            }
            let Some(index) = found else {
                return Ok(mismatch);
            };
            bucket.swap_remove(index);
        }
    }
    Ok(None)
}

fn render(result: &Result<MaterializedResult>) -> Result<ByteString> {
    let result = match result {
        Ok(result) => result,
        Err(error) => return Ok(byte_format!("{error}\n")),
    };
    let mut text = ByteString::new();
    for name in result.names() {
        let _ = byte_write!(&mut text, "{name}\t");
    }
    text.push_bytes("\n");
    for ty in result.types() {
        let _ = byte_write!(&mut text, "{ty}\t");
    }
    let _ = byte_write!(&mut text, "\n[ Rows: {}]\n", result.row_count());
    for row in 0..result.row_count() {
        for column in 0..result.column_count() {
            if column != 0 {
                text.push_bytes("\t");
            }
            let value = result.value(column, row);
            if value.is_null() {
                text.push_bytes("NULL");
            } else {
                text.push_bytes(value.to_varchar_bytes()?.replace_byte(0, b"\\0"));
            }
        }
        text.push_bytes("\n");
    }
    text.push_bytes("\n");
    Ok(text)
}
