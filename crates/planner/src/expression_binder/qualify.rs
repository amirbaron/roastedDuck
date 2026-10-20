//! Qualifying column references with their tables before binding (DuckDB's
//! `ExpressionBinder::QualifyColumnName(s)`, `CreateStructExtract`, `CreateStructPack` and
//! `BindAndQualifyFunction`).

use std::collections::HashSet;

use rustdb_ast::{
    ExpressionClass, ExpressionType, FunctionExpression, LambdaRefExpression, ParsedExpression,
    ParsedExpressionKind, lambda_parameters,
};
use rustdb_catalog::CatalogType;
use rustdb_common::{Error, Result};
use rustdb_types::Value;

use super::{BindError, function_call, is_bound, is_unnest_function, operator_expression};
use crate::binder::{Binder, Binding, BindingAlias};
use crate::context::EntryLookup;

/// Parameter names of the lambdas being qualified, innermost last.
type LambdaParameters = Vec<HashSet<String>>;

/// What qualification needs: the binder's relations, macro and lambda parameters, and
/// which names are select-list aliases. Expression binders implement it; binders that
/// qualify differently (HAVING) override [`ColumnQualifier::qualify_column_name`].
pub trait ColumnQualifier {
    fn binder_ref(&self) -> &Binder;
    /// Parameters of the lambdas being bound, innermost last.
    fn lambda_bindings(&self) -> &[Binding];

    /// Whether `names` could be a select-list alias (DuckDB's `DoesColumnAliasExist`).
    fn does_column_alias_exist(&self, _names: &[String]) -> bool {
        false
    }

    /// The qualified form of a column reference (DuckDB's virtual `QualifyColumnName`).
    fn qualify_column_name(
        &self,
        names: &[String],
        location: Option<u64>,
    ) -> Result<std::result::Result<ParsedExpression, BindError>> {
        default_qualify_column_name(self, names, location)
    }

    /// Qualifies every column reference in `expr` (DuckDB's `QualifyColumnNames`).
    fn qualify_column_names(&self, expr: &mut ParsedExpression) -> Result<()> {
        let mut lambda_params = LambdaParameters::new();
        qualify_recursive(self, expr, &mut lambda_params, false)
    }
}

/// Qualification against a binder alone (DuckDB's `QualifyColumnNames(Binder &, expr)`,
/// which uses a throw-away `WhereBinder`).
#[derive(Debug)]
pub struct QualifyContext<'a> {
    binder: &'a Binder,
    lambda_bindings: &'a [Binding],
}

impl<'a> QualifyContext<'a> {
    pub fn new(binder: &'a Binder, lambda_bindings: &'a [Binding]) -> Self {
        Self {
            binder,
            lambda_bindings,
        }
    }
}

impl ColumnQualifier for QualifyContext<'_> {
    fn binder_ref(&self) -> &Binder {
        self.binder
    }

    fn lambda_bindings(&self) -> &[Binding] {
        self.lambda_bindings
    }
}

/// Qualifies the column references of `expr` against `binder` alone.
pub fn qualify_column_names(binder: &Binder, expr: &mut ParsedExpression) -> Result<()> {
    QualifyContext::new(binder, &[]).qualify_column_names(expr)
}

fn is_lambda_parameter(lambda_params: &LambdaParameters, name: &str) -> bool {
    lambda_params.iter().any(|level| level.contains(name))
}

fn qualify_recursive<Q: ColumnQualifier + ?Sized>(
    qualifier: &Q,
    expr: &mut ParsedExpression,
    lambda_params: &mut LambdaParameters,
    within_function: bool,
) -> Result<()> {
    if is_bound(expr) {
        return Ok(());
    }
    let mut next_within_function = false;
    match expr.expression_type {
        ExpressionType::ColumnRef => {
            let ParsedExpressionKind::ColumnRef(column) = &expr.kind else {
                return Ok(());
            };
            let name = expr.name();
            if is_lambda_parameter(lambda_params, &name) {
                return Ok(());
            }
            let names = column.column_names.clone();
            if let Ok(mut qualified) = qualifier.qualify_column_name(&names, expr.query_location)? {
                if !expr.alias.is_empty() {
                    qualified.alias.clone_from(&expr.alias);
                } else if within_function {
                    // Within a function, an alias would turn the argument into a named one.
                    qualified.alias.clear();
                }
                qualified.query_location = expr.query_location;
                *expr = qualified;
            }
            return Ok(());
        }
        ExpressionType::PositionalReference => {
            if let ParsedExpressionKind::PositionalReference(reference) = &expr.kind
                && expr.alias.is_empty()
                && let Ok((_, column)) = qualifier.binder_ref().bind_context.positional_column(reference)
            {
                expr.alias = column;
            }
        }
        ExpressionType::Function => {
            let location = expr.query_location;
            if let ParsedExpressionKind::Function(function) = &mut expr.kind {
                if !is_unnest_function(&function.function_name) {
                    bind_and_qualify_function(qualifier, function, location, false)?;
                }
                if is_lambda_function(function) {
                    return qualify_in_lambda(qualifier, function, lambda_params);
                }
            }
            next_within_function = true;
        }
        _ => {}
    }
    for_each_child(expr, &mut |child| {
        qualify_recursive(qualifier, child, lambda_params, next_within_function)
    })
}

/// Whether a function has a lambda argument (DuckDB's `FunctionExpression::IsLambdaFunction`).
pub fn is_lambda_function(function: &FunctionExpression) -> bool {
    function.function_name != "->>"
        && function
            .children
            .iter()
            .any(|child| child.expression_class() == ExpressionClass::Lambda)
}

/// DuckDB's `QualifyColumnNamesInLambda`: lambda parameters are not qualified.
fn qualify_in_lambda<Q: ColumnQualifier + ?Sized>(
    qualifier: &Q,
    function: &mut FunctionExpression,
    lambda_params: &mut LambdaParameters,
) -> Result<()> {
    for child in &mut function.children {
        let ParsedExpressionKind::Lambda(lambda) = &mut child.kind else {
            qualify_recursive(qualifier, child, lambda_params, true)?;
            continue;
        };
        let Some(parameters) = lambda_parameters(lambda) else {
            // Possibly the JSON arrow operator: qualify both sides.
            qualify_recursive(qualifier, &mut lambda.lhs, lambda_params, true)?;
            qualify_recursive(qualifier, &mut lambda.expr, lambda_params, true)?;
            continue;
        };
        let names = parameters.iter().map(|parameter| parameter.name()).collect();
        lambda_params.push(names);
        qualify_recursive(qualifier, &mut lambda.expr, lambda_params, true)?;
        lambda_params.pop();
    }
    Ok(())
}

/// Calls `callback` on each direct child of a parsed expression
/// (DuckDB's `ParsedExpressionIterator::EnumerateChildren`).
pub fn for_each_child(
    expr: &mut ParsedExpression,
    callback: &mut dyn FnMut(&mut ParsedExpression) -> Result<()>,
) -> Result<()> {
    match &mut expr.kind {
        ParsedExpressionKind::Between(between) => {
            callback(&mut between.input)?;
            callback(&mut between.lower)?;
            callback(&mut between.upper)?;
        }
        ParsedExpressionKind::Case(case) => {
            for check in &mut case.case_checks {
                callback(&mut check.when_expr)?;
                callback(&mut check.then_expr)?;
            }
            callback(&mut case.else_expr)?;
        }
        ParsedExpressionKind::Cast(cast) => callback(&mut cast.child)?,
        ParsedExpressionKind::Collate(collate) => callback(&mut collate.child)?,
        ParsedExpressionKind::Comparison(comparison) => {
            callback(&mut comparison.left)?;
            callback(&mut comparison.right)?;
        }
        ParsedExpressionKind::Conjunction(conjunction) => {
            for child in &mut conjunction.children {
                callback(child)?;
            }
        }
        ParsedExpressionKind::Function(function) => {
            for child in &mut function.children {
                callback(child)?;
            }
            if let Some(filter) = &mut function.filter {
                callback(filter)?;
            }
            for order in &mut function.order_bys.orders {
                callback(&mut order.expression)?;
            }
        }
        ParsedExpressionKind::Lambda(lambda) => {
            callback(&mut lambda.lhs)?;
            callback(&mut lambda.expr)?;
        }
        ParsedExpressionKind::Operator(operator) => {
            for child in &mut operator.children {
                callback(child)?;
            }
        }
        ParsedExpressionKind::Star(star) => {
            if let Some(inner) = &mut star.expr {
                callback(inner)?;
            }
            for (_, replacement) in star.replace_list.iter_mut() {
                callback(replacement)?;
            }
        }
        ParsedExpressionKind::Subquery(subquery) => {
            if let Some(child) = &mut subquery.child {
                callback(child)?;
            }
        }
        ParsedExpressionKind::Window(window) => {
            for child in &mut window.children {
                callback(child)?;
            }
            for partition in &mut window.partitions {
                callback(partition)?;
            }
            for order in &mut window.orders {
                callback(&mut order.expression)?;
            }
            for bound in [
                &mut window.start_expr,
                &mut window.end_expr,
                &mut window.offset_expr,
                &mut window.default_expr,
                &mut window.filter_expr,
            ]
            .into_iter()
            .flatten()
            {
                callback(bound)?;
            }
            for order in &mut window.arg_orders {
                callback(&mut order.expression)?;
            }
        }
        ParsedExpressionKind::Type(ty) => {
            for child in &mut ty.children {
                callback(child)?;
            }
        }
        ParsedExpressionKind::ColumnRef(_)
        | ParsedExpressionKind::LambdaRef(_)
        | ParsedExpressionKind::Constant(_)
        | ParsedExpressionKind::Default
        | ParsedExpressionKind::Parameter(_)
        | ParsedExpressionKind::PositionalReference(_)
        | ParsedExpressionKind::Bound(_) => {}
    }
    Ok(())
}

/// `base.field` as a `STRUCT_EXTRACT` operator (DuckDB's `CreateStructExtract`).
pub fn create_struct_extract(base: ParsedExpression, field: &str) -> ParsedExpression {
    operator_expression(
        ExpressionType::StructExtract,
        vec![base, ParsedExpression::constant(Value::varchar(field))],
    )
}

/// A lambda parameter reference if `name` is one (DuckDB's `LambdaRefExpression::FindMatchingBinding`).
fn lambda_reference(lambda_bindings: &[Binding], name: &str) -> Option<ParsedExpression> {
    let index = lambda_bindings
        .iter()
        .rposition(|binding| binding.has_matching_binding(name))?;
    Some(ParsedExpression::new(
        ExpressionType::LambdaRef,
        ParsedExpressionKind::LambdaRef(LambdaRefExpression {
            lambda_idx: index as u64,
            column_name: name.to_owned(),
        }),
    ))
}

/// Whether a binding (or the macro binding) has the given alias and column
/// (DuckDB's `Binder::GetMatchingBinding`): `Ok(None)` with the soft error otherwise.
fn matching_binding(
    binder: &Binder,
    alias: &BindingAlias,
    column: &str,
) -> Result<std::result::Result<BindingAlias, Error>> {
    if let Some(macro_binding) = &binder.macro_binding
        && alias.catalog.is_empty()
        && alias.schema.is_empty()
        && alias.alias == macro_binding.alias.alias
    {
        return Ok(Ok(macro_binding.alias.clone()));
    }
    match binder.bind_context.binding_with_column(alias, column)? {
        Ok(index) => Ok(Ok(binder.bind_context.bindings()[index].alias.clone())),
        Err(error) => Ok(Err(error)),
    }
}

/// DuckDB's `QualifyColumnName(expr, column_name, error)`: the qualified reference for an
/// unqualified name, through USING sets, lambda and macro parameters and the tables in
/// scope.
fn qualify_single<Q: ColumnQualifier + ?Sized>(
    qualifier: &Q,
    column_name: &str,
    location: Option<u64>,
) -> Result<std::result::Result<ParsedExpression, BindError>> {
    let binder = qualifier.binder_ref();
    let context = &binder.bind_context;
    if let Some(set) = context.using_binding(column_name)? {
        let set = set
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if set.primary_binding.is_set() {
            return Ok(Ok(
                context.create_column_reference(&set.primary_binding, column_name)
            ));
        }
        let children = set
            .bindings
            .iter()
            .map(|alias| column_reference(alias, column_name))
            .collect();
        return Ok(Ok(operator_expression(
            ExpressionType::OperatorCoalesce,
            children,
        )));
    }
    if let Some(reference) = lambda_reference(qualifier.lambda_bindings(), column_name) {
        return Ok(Ok(reference));
    }
    let table_binding = context.matching_binding(column_name, location)?;
    if let Some(macro_binding) = &binder.macro_binding
        && macro_binding.has_matching_binding(column_name)
    {
        if table_binding.is_some() {
            return Err(
                Error::binder(format!("Conflicting column names for column {column_name}!"))
                    .with_position(location),
            );
        }
        return Ok(Ok(
            context.create_column_reference(&macro_binding.alias, column_name)
        ));
    }
    if let Some(binding) = table_binding {
        return Ok(Ok(context.create_column_reference(&binding.alias, column_name)));
    }
    let similar = context.similar_bindings(column_name);
    Ok(Err(BindError::column_not_found(column_name, similar, location)))
}

fn column_reference(alias: &BindingAlias, column: &str) -> ParsedExpression {
    let mut names = Vec::new();
    for part in [&alias.catalog, &alias.schema, &alias.alias] {
        if !part.is_empty() {
            names.push(part.clone());
        }
    }
    names.push(column.to_owned());
    ParsedExpression::column_ref(names)
}

/// An implicit `struct_pack` of all columns of a table referenced by name
/// (DuckDB's `CreateStructPack`).
fn create_struct_pack(binder: &Binder, names: &[String]) -> Result<Option<ParsedExpression>> {
    let context = &binder.bind_context;
    let lookup = |alias: BindingAlias| -> Result<Option<BindingAlias>> {
        Ok(context.binding(&alias)?.map(|binding| binding.alias.clone()))
    };
    let alias = match names {
        [table] => lookup(BindingAlias::new(table))?,
        [first, second] => match lookup(BindingAlias::qualified("", first, second))? {
            Some(alias) => Some(alias),
            None => lookup(BindingAlias::qualified(first, "", second))?,
        },
        [catalog, schema, table] => lookup(BindingAlias::qualified(catalog, schema, table))?,
        _ => return Ok(None),
    };
    let Some(alias) = alias else { return Ok(None) };
    let Some(binding) = context.binding(&alias)? else {
        return Ok(None);
    };
    let children = binding
        .names
        .iter()
        .map(|column| context.create_column_reference_unexpanded(&binding.alias, column))
        .collect();
    Ok(Some(function_call("struct_pack", children)))
}

/// DuckDB's base `ExpressionBinder::QualifyColumnName(ColumnRefExpression &)`.
pub fn default_qualify_column_name<Q: ColumnQualifier + ?Sized>(
    qualifier: &Q,
    names: &[String],
    location: Option<u64>,
) -> Result<std::result::Result<ParsedExpression, BindError>> {
    let binder = qualifier.binder_ref();
    if let [name] = names
        && let Some(reference) = lambda_reference(qualifier.lambda_bindings(), name)
    {
        return Ok(Ok(reference));
    }
    match names {
        [] => Err(Error::internal("column reference without names")),
        [name] => match qualify_single(qualifier, name, location)? {
            Ok(qualified) => Ok(Ok(qualified)),
            Err(error) => Ok(create_struct_pack(binder, names)?.ok_or(error)),
        },
        [table, column] => match matching_binding(binder, &BindingAlias::new(table), column)? {
            Ok(alias) => Ok(Ok(binder.bind_context.create_column_reference(&alias, column))),
            Err(error) => {
                if let Ok(qualified) = qualify_single(qualifier, table, location)? {
                    return Ok(Ok(create_struct_extract(qualified, column)));
                }
                Ok(create_struct_pack(binder, names)?.ok_or_else(|| BindError::from(error)))
            }
        },
        _ => qualify_many_dots(qualifier, names, location),
    }
}

/// `a.b.c...`: catalog/schema/table/column/struct field resolution (DuckDB's
/// `QualifyColumnNameWithManyDots`).
fn qualify_many_dots<Q: ColumnQualifier + ?Sized>(
    qualifier: &Q,
    names: &[String],
    location: Option<u64>,
) -> Result<std::result::Result<ParsedExpression, BindError>> {
    let binder = qualifier.binder_ref();
    let context = &binder.bind_context;
    let attempt = |alias: BindingAlias, column: &str| matching_binding(binder, &alias, column);
    let mut fully_qualified_error = None;
    let mut resolved: Option<(ParsedExpression, usize)> = None;
    if names.len() > 3 {
        match attempt(
            BindingAlias::qualified(&names[0], &names[1], &names[2]),
            &names[3],
        )? {
            Ok(alias) => resolved = Some((context.create_column_reference(&alias, &names[3]), 4)),
            Err(error) => fully_qualified_error = Some(error),
        }
    }
    let mut catalog_table_error = None;
    let mut schema_table_error = None;
    let mut table_column_error = None;
    if resolved.is_none() {
        match attempt(BindingAlias::qualified(&names[0], "", &names[1]), &names[2])? {
            Ok(alias) => resolved = Some((context.create_column_reference(&alias, &names[2]), 3)),
            Err(error) => catalog_table_error = Some(error),
        }
    }
    if resolved.is_none() {
        match attempt(BindingAlias::qualified("", &names[0], &names[1]), &names[2])? {
            Ok(alias) => resolved = Some((context.create_column_reference(&alias, &names[2]), 3)),
            Err(error) => schema_table_error = Some(error),
        }
    }
    if resolved.is_none() {
        match attempt(BindingAlias::new(&names[0]), &names[1])? {
            Ok(alias) => resolved = Some((context.create_column_reference(&alias, &names[1]), 2)),
            Err(error) => table_column_error = Some(error),
        }
    }
    if resolved.is_none()
        && let Ok(qualified) = qualify_single(qualifier, &names[0], location)?
    {
        resolved = Some((qualified, 1));
    }
    if let Some((mut result, start)) = resolved {
        for field in &names[start..] {
            result = create_struct_extract(result, field);
        }
        return Ok(Ok(result));
    }
    if let Some(pack) = create_struct_pack(binder, names)? {
        return Ok(Ok(pack));
    }
    // Pick the error matching what the first parts most likely are.
    let (mut catalog_pos, mut schema_pos, mut table_pos) = (None, None, None);
    for binding in context.bindings() {
        let (catalog, schema) = binding.entry_parent.as_ref().map_or(
            (&binding.alias.catalog, &binding.alias.schema),
            |(catalog, schema)| (catalog, schema),
        );
        for (i, name) in names.iter().take(3).enumerate() {
            if catalog == name {
                catalog_pos = Some(i);
            }
            if schema == name {
                schema_pos = Some(i);
            }
            if &binding.alias.alias == name {
                table_pos = Some(i);
            }
        }
    }
    let many = names.len() > 3;
    // One arm per branch of DuckDB's ordered if-chain, kept in its order.
    #[allow(clippy::match_same_arms)]
    let error = match (table_pos, catalog_pos, schema_pos) {
        (Some(1), Some(_), _) => catalog_table_error,
        (Some(1), None, _) => schema_table_error,
        (Some(position), _, _) if position >= 2 && many => fully_qualified_error,
        (None, Some(_), Some(_)) if many => fully_qualified_error,
        (None, Some(_), _) => catalog_table_error,
        (None, None, Some(0)) => schema_table_error,
        (None, None, Some(1)) if many => fully_qualified_error,
        _ => table_column_error,
    };
    let error = error.unwrap_or_else(|| Error::binder("could not qualify column reference"));
    Ok(Err(BindError::from(error)))
}

/// Resolves a function's catalog entry, turning `x.f()` into `f(x)` when `x` is a column
/// (DuckDB's `BindAndQualifyFunction`).
pub fn bind_and_qualify_function<Q: ColumnQualifier + ?Sized>(
    qualifier: &Q,
    function: &mut FunctionExpression,
    location: Option<u64>,
    allow_throw: bool,
) -> Result<Option<EntryLookup>> {
    let binder = qualifier.binder_ref();
    let context = binder.context.clone();
    if !function.schema.is_empty()
        && function.catalog.is_empty()
        && context.catalog(&function.schema).is_some()
    {
        function.catalog = std::mem::take(&mut function.schema);
    }
    let lookup = |catalog: &str, schema: &str, ty: CatalogType| {
        context.get_entry(catalog, schema, ty, &function.function_name, true)
    };
    let found = lookup(&function.catalog, &function.schema, CatalogType::ScalarFunction)?;
    if found.is_some() {
        return Ok(found);
    }
    if lookup(&function.catalog, &function.schema, CatalogType::TableFunction)?.is_some() {
        if !allow_throw {
            return Ok(None);
        }
        return Err(Error::binder(format!(
            "Function \"{}\" is a table function but it was used as a scalar function. This function has to be \
             called in a FROM clause (similar to a table).",
            function.function_name
        )));
    }
    if !function.schema.is_empty() && lookup("", "", CatalogType::ScalarFunction)?.is_some() {
        // `x.f()` where `f` is a built-in: a method call on the column `x`.
        let names = if function.catalog.is_empty() {
            vec![function.schema.clone()]
        } else {
            vec![function.catalog.clone(), function.schema.clone()]
        };
        let column = ParsedExpression::column_ref(names.clone());
        if let Err(error) = qualifier.qualify_column_name(&names, None)?
            && !qualifier.does_column_alias_exist(&names)
        {
            if !allow_throw {
                return lookup("", "", CatalogType::ScalarFunction);
            }
            return Err(error.error);
        }
        function.children.insert(0, column);
        function.catalog.clear();
        function.schema.clear();
        return lookup("", "", CatalogType::ScalarFunction);
    }
    context
        .get_entry(
            &function.catalog,
            &function.schema,
            CatalogType::ScalarFunction,
            &function.function_name,
            !allow_throw,
        )
        .map_err(|error| crate::context::position_missing_entry(error, location))
}
