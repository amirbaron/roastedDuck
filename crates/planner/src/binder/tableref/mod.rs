//! FROM-clause items (DuckDB's `planner/binder/tableref`): each binds into the binder's
//! bind context and returns its plan.

use std::collections::HashMap;
use std::sync::{Arc, PoisonError};

use roastedduck_ast::{BaseTableRef, ColumnDataRef, ExpressionListRef, SubqueryRef, TableRef, TableRefKind};
use roastedduck_catalog::{CatalogType, EntryKind};
use roastedduck_common::{ByteString, Error, Result};
use roastedduck_function::TableFunction;
use roastedduck_types::LogicalType;

pub(crate) mod bind_joinref;
mod bind_pivot;
mod bind_showref;
mod bind_table_function;
mod plan_joinref;

pub use bind_showref::SHOW_TABLES_EXPANDED;
pub use plan_joinref::{JoinSide, create_join, extract_join_conditions};

pub(crate) use bind_table_function::bind_named_parameters;
pub use plan_joinref::extract_join_conditions_from;
pub(crate) use plan_joinref::table_references;

use super::bind_context::alias_column_names;
use super::binding::{Binding, BindingAlias, BindingKind};
use super::query_node::{bind_node, plan_subqueries};
use super::{BindScope, Binder, BinderType, BindingMode, BoundStatement, Scope};
use crate::expression_binder::{ExpressionBinder, InsertBinder, expression_return_type};
use crate::function_binder::{add_cast_to_type, max_logical_type, normalize_type};
use crate::operator::{ColumnIndex, LogicalGet, LogicalKind, LogicalOperator};

/// Binds a FROM-clause item (DuckDB's `Binder::Bind(TableRef &)`).
pub fn bind_table_ref(scope: &mut BindScope<'_>, table_ref: &mut TableRef) -> Result<BoundStatement> {
    let mut external_replacement = None;
    let mut result = match &mut table_ref.kind {
        TableRefKind::Empty => Ok(BoundStatement::new(
            LogicalOperator::new(LogicalKind::DummyScan {
                table_index: scope.generate_table_index(),
            }),
            Vec::new(),
            Vec::new(),
        )),
        TableRefKind::ExpressionList(list) => bind_expression_list(scope, list, &table_ref.alias),
        TableRefKind::Subquery(subquery) => bind_subquery_ref(scope, subquery, &table_ref.alias),
        TableRefKind::BaseTable(base) => bind_base_table(
            scope,
            base,
            &table_ref.alias,
            table_ref.query_location,
            &mut external_replacement,
        ),
        TableRefKind::Join(join) => bind_joinref::bind_join(scope, join, table_ref.query_location),
        TableRefKind::TableFunction(function) => bind_table_function::bind_table_function(
            scope,
            function,
            &table_ref.alias,
            table_ref.query_location,
        ),
        TableRefKind::Pivot(pivot) => bind_pivot::bind_pivot(scope, pivot, &table_ref.alias),
        TableRefKind::Show(show) => bind_showref::bind_show(scope, show),
        TableRefKind::ColumnData(column_data) => bind_column_data(scope, column_data, &table_ref.alias),
    }?;
    if let Some(mut replacement) = external_replacement {
        replacement.sample = table_ref.sample.take();
        replacement.query_location = table_ref.query_location;
        scope.binder.retain_external_replacement(&replacement);
        *table_ref = replacement;
    }
    if let Some(sample_options) = table_ref.sample.take() {
        result.plan =
            LogicalOperator::new(LogicalKind::Sample { sample_options }).with_children(vec![result.plan]);
    }
    Ok(result)
}

/// Rows carried by the statement, e.g. an appender's (DuckDB's
/// `Binder::Bind(ColumnDataRef &)`).
fn bind_column_data(
    scope: &mut BindScope<'_>,
    column_data: &ColumnDataRef,
    alias: &[u8],
) -> Result<BoundStatement> {
    let collection = column_data
        .collection
        .as_ref()
        .ok_or_else(|| Error::internal("ColumnDataRef without a collection"))?;
    let types = collection.types().to_vec();
    let mut names = column_data.expected_names.clone();
    for i in names.len()..types.len() {
        names.push(roastedduck_common::byte_format!("col{}", i + 1));
    }
    let table_index = scope.binder.generate_table_index();
    scope
        .binder
        .bind_context
        .add_generic_binding(table_index, alias, names.clone(), types.clone())?;
    let plan = LogicalOperator::new(LogicalKind::ChunkGet {
        table_index,
        chunk_types: types.clone(),
        collection: Arc::clone(collection.chunks()),
    });
    Ok(BoundStatement::new(plan, names, types))
}

/// `VALUES (...), (...)` (DuckDB's `Binder::Bind(ExpressionListRef &)`).
fn bind_expression_list(
    scope: &mut BindScope<'_>,
    list: &mut ExpressionListRef,
    alias: &[u8],
) -> Result<BoundStatement> {
    let mut types = list.expected_types.clone();
    let mut names = list.expected_names.clone();
    let previous_can_contain_nulls = scope.binder.can_contain_nulls;
    let mut values: Vec<Vec<crate::expression::Expression>> = Vec::with_capacity(list.values.len());
    for row in &mut list.values {
        if names.is_empty() {
            names = (0..row.len())
                .map(|i| roastedduck_common::byte_format!("col{i}"))
                .collect();
        }
        scope.binder.can_contain_nulls = true;
        let mut bound_row = Vec::with_capacity(row.len());
        {
            let (inner, outer) = scope.split();
            let mut binder = InsertBinder::new(inner, outer);
            for (i, value) in row.iter_mut().enumerate() {
                binder.set_target_type(types.get(i).cloned());
                bound_row.push(binder.bind(value, true)?);
            }
        }
        values.push(bound_row);
        scope.binder.can_contain_nulls = previous_can_contain_nulls;
    }
    if types.is_empty() && !list.values.is_empty() {
        types = vec![LogicalType::SQLNULL; list.values[0].len()];
        for row in &values {
            for (i, value) in row.iter().enumerate() {
                types[i] = max_logical_type(&scope.context, &types[i], &expression_return_type(value))?;
            }
        }
        for ty in &mut types {
            *ty = normalize_type(ty)?;
        }
        let context = scope.context.clone();
        for row in &mut values {
            for (i, value) in row.iter_mut().enumerate() {
                let expression = std::mem::replace(
                    value,
                    crate::expression::Expression::constant(roastedduck_types::Value::default()),
                );
                *value = add_cast_to_type(&context, expression, &types[i], false)?;
            }
        }
    }
    let bind_index = scope.generate_table_index();
    scope
        .bind_context
        .add_generic_binding(bind_index, alias, names.clone(), types.clone())?;
    let mut root = LogicalOperator::new(LogicalKind::DummyScan {
        table_index: scope.generate_table_index(),
    });
    for row in &mut values {
        for value in row {
            plan_subqueries(scope, value, &mut root)?;
        }
    }
    let plan = LogicalOperator::new(LogicalKind::ExpressionGet {
        table_index: bind_index,
        expr_types: types.clone(),
        expressions: values,
    })
    .with_children(vec![root]);
    Ok(BoundStatement::new(plan, names, types))
}

/// A subquery in FROM (DuckDB's `Binder::Bind(SubqueryRef &)`).
pub(crate) fn bind_subquery_ref(
    scope: &mut BindScope<'_>,
    subquery: &mut SubqueryRef,
    alias: &[u8],
) -> Result<BoundStatement> {
    let mut binder = scope.binder.child()?;
    binder.can_contain_nulls = true;
    let bound = {
        let mut child_scope = BindScope {
            binder: &mut binder,
            outer: Some(&mut *scope),
        };
        bind_node(&mut child_scope, &mut subquery.subquery.node)?
    };
    binder.alias = if alias.is_empty() {
        ByteString::from("unnamed_subquery")
    } else {
        ByteString::from(alias)
    };
    let bind_index = bound.plan.root_index()?;
    let subquery_alias = if alias.is_empty() {
        scope.binder.next_unnamed_subquery_name()
    } else {
        ByteString::from(alias)
    };
    binder.is_outside_flattened = scope.binder.is_outside_flattened;
    if binder.has_unplanned_dependent_joins {
        scope.binder.has_unplanned_dependent_joins = true;
    }
    scope.binder.bind_context.add_subquery(
        bind_index,
        &subquery_alias,
        &subquery.column_name_alias,
        &bound.names,
        bound.types.clone(),
    )?;
    scope.binder.move_correlated_expressions(&mut binder);
    Ok(bound)
}

/// Renames duplicate names `x` to `x_1`, `x_2`, ... (DuckDB's `QueryResult::DeduplicateColumns`).
pub fn deduplicate_columns(names: &mut [ByteString]) {
    let mut counts: HashMap<ByteString, u64> = HashMap::new();
    for name in names.iter_mut() {
        let lower = name.to_ascii_lowercase();
        let Some(count) = counts.get(&lower).copied() else {
            counts.insert(lower, 1);
            continue;
        };
        let mut count = count;
        let mut candidate = roastedduck_common::byte_format!("{name}_{count}");
        while counts.contains_key(&candidate.to_ascii_lowercase()) {
            count += 1;
            counts.insert(lower.clone(), count);
            candidate = roastedduck_common::byte_format!("{name}_{count}");
        }
        *counts.entry(candidate.to_ascii_lowercase()).or_insert(0) += 1;
        *name = candidate;
    }
}

/// The outcome of looking up a CTE through the enclosing binders.
enum CteLookup {
    /// Bound and referenced: `(cte index, names, types)`.
    Found(u64, Vec<ByteString>, Vec<LogicalType>),
    /// Only a CTE that may not be referenced here (a self-reference).
    Circular,
    NotFound,
}

/// Looks up a CTE in `holder` and the binders around it, binding it on first reference
/// (DuckDB's `Binder::GetCTEBinding` + `CTEBinding::Reference` + `CTEBindState::Bind`).
fn reference_cte(
    holder: &mut Binder,
    defining: Option<&mut dyn Scope>,
    alias: &BindingAlias,
) -> Result<CteLookup> {
    let mut circular = false;
    if let Some(binding) = holder.bind_context.cte_binding(alias) {
        if binding.can_be_referenced() {
            if let Some(state) = binding.cte_bind_state() {
                bind_cte_query(&state, binding.index, &binding.alias.clone(), defining)?;
            }
            let binding = holder
                .bind_context
                .cte_binding_mut(alias)
                .ok_or_else(|| Error::internal("CTE binding disappeared"))?;
            binding.reference()?;
            return Ok(CteLookup::Found(
                binding.index,
                binding.names.clone(),
                binding.types.clone(),
            ));
        }
        circular = true;
    }
    let found = if holder.binder_type == BinderType::Regular
        && let Some(next) = defining
    {
        let (next_binder, next_defining) = next.parts();
        reference_cte(next_binder, next_defining, alias)?
    } else {
        CteLookup::NotFound
    };
    Ok(match found {
        CteLookup::NotFound if circular => CteLookup::Circular,
        other => other,
    })
}

/// Binds a CTE's query next to the binder that defined it (DuckDB's `CTEBindState::Bind`).
fn bind_cte_query(
    state: &Arc<std::sync::Mutex<super::binding::CteBindState>>,
    index: u64,
    alias: &BindingAlias,
    defining: Option<&mut dyn Scope>,
) -> Result<()> {
    let (mut definition, aliases) = {
        let state = state.lock().unwrap_or_else(PoisonError::into_inner);
        (state.definition.clone(), state.aliases.clone())
    };
    let Some(defining) = defining else {
        return Err(Error::internal("CTE without a defining binder"));
    };
    let mut query_binder = defining.scope_binder().child()?;
    query_binder.can_contain_nulls = true;
    query_binder.bind_context.add_cte_binding(Binding::new(
        BindingKind::Cte {
            can_be_referenced: false,
            reference_count: 0,
            bind_state: None,
        },
        alias.clone(),
        Vec::new(),
        Vec::new(),
        index,
    )?)?;
    let mut bound = {
        let mut scope = BindScope {
            binder: &mut query_binder,
            outer: Some(defining),
        };
        bind_node(&mut scope, &mut definition)?
    };
    for (name, alias) in bound.names.iter_mut().zip(&aliases) {
        name.clone_from(alias);
    }
    deduplicate_columns(&mut bound.names);
    let mut state = state.lock().unwrap_or_else(PoisonError::into_inner);
    state.bound = Some(bound);
    state.query_binder = Some(query_binder);
    Ok(())
}

/// Local path probing keeps the platform's filename representation.
fn replacement_path_is_file(path: &[u8]) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        std::path::Path::new(std::ffi::OsStr::from_bytes(path)).is_file()
    }
    #[cfg(not(unix))]
    {
        // The Windows filesystem API receives UTF-8 through its wide-string adapter.
        std::str::from_utf8(path).is_ok_and(|path| std::path::Path::new(path).is_file())
    }
}

/// `FileSystem::ExtractBaseName`: the file name up to its first dot.
fn extract_base_name(path: &[u8]) -> ByteString {
    let file = path
        .split(|byte| matches!(byte, b'/' | b'\\'))
        .rfind(|part| !part.is_empty())
        .unwrap_or_default();
    file.split(|byte| *byte == b'.')
        .find(|part| !part.is_empty())
        .unwrap_or_default()
        .into()
}

/// `ReplacementScan::GetFullPath`: the catalog, schema and table names joined by dots.
fn replacement_full_path(base: &BaseTableRef) -> ByteString {
    let mut full_path = base.catalog_name.clone();
    if !base.schema_name.is_empty() {
        if !full_path.is_empty() {
            full_path.push(b'.');
        }
        full_path.push_bytes(&base.schema_name);
    }
    if !full_path.is_empty() {
        full_path.push(b'.');
    }
    full_path.push_bytes(&base.table_name);
    full_path
}

/// The CSV replacement scan (DuckDB's `ReadCSVReplacement` through
/// `Binder::BindWithReplacementScan`): `FROM 'file.csv'` reads the file with
/// `read_csv_auto`; DuckDB, Parquet and JSON files are read by their readers likewise.
#[allow(
    clippy::case_sensitive_file_extension_comparisons,
    reason = "the name is lowercased"
)]
fn csv_replacement_scan(base: &BaseTableRef, alias: &[u8]) -> Result<Option<TableRef>> {
    let full_path = replacement_full_path(base);
    let mut lower_name = full_path.as_bytes().to_ascii_lowercase();
    if lower_name.ends_with(b".gz") {
        lower_name.truncate(lower_name.len() - 3);
    } else if lower_name.ends_with(b".zst") {
        lower_name.truncate(lower_name.len() - 4);
    }
    // `ReplacementScan::CanReplace` for the CSV reader, then `read_duckdb`'s
    // (`ReadDuckDBTableFunction::ReplacementScan`, on the whole name), then the Parquet
    // extension's `ParquetScanReplacement`.
    let has_extension = |name: &[u8], extension: &str| {
        let suffix = format!(".{extension}");
        let query_suffix = format!("{suffix}?");
        name.ends_with(suffix.as_bytes())
            || name
                .windows(query_suffix.len())
                .any(|part| part == query_suffix.as_bytes())
    };
    let can_replace = |extension: &str| has_extension(&lower_name, extension);
    let full_lower_name = full_path.as_bytes().to_ascii_lowercase();
    let is_duckdb_file = ["db", "ddb", "duckdb"]
        .iter()
        .any(|extension| has_extension(&full_lower_name, extension));
    let function_name = if can_replace("csv") || can_replace("tsv") {
        "read_csv_auto"
    } else if is_duckdb_file {
        "read_duckdb"
    } else if can_replace("parquet") {
        "parquet_scan"
    } else if can_replace("json") || can_replace("jsonl") || can_replace("ndjson") {
        // The JSON extension's `ReadJSONReplacement`.
        "read_json_auto"
    } else {
        return Ok(None);
    };
    let function = roastedduck_ast::FunctionExpression {
        function_name: function_name.to_owned().into(),
        schema: String::new().into(),
        children: vec![roastedduck_ast::ParsedExpression::constant(
            roastedduck_types::Value::try_varchar(full_path.as_bytes())?,
        )],
        filter: None,
        order_bys: roastedduck_ast::OrderModifier { orders: Vec::new() },
        distinct: false,
        is_operator: false,
        export_state: false,
        catalog: String::new().into(),
    };
    let has_glob = full_path.iter().any(|byte| matches!(byte, b'*' | b'?' | b'['));
    let alias = if !alias.is_empty() {
        ByteString::from(alias)
    } else if !has_glob {
        extract_base_name(&full_path)
    } else {
        base.table_name.clone()
    };
    Ok(Some(TableRef {
        external_dependency: roastedduck_ast::ExternalDependency::default(),
        alias,
        sample: None,
        query_location: None,
        kind: TableRefKind::TableFunction(roastedduck_ast::TableFunctionRef {
            function: Box::new(roastedduck_ast::ParsedExpression::new(
                roastedduck_ast::ExpressionType::Function,
                roastedduck_ast::ParsedExpressionKind::Function(Box::new(function)),
            )),
            column_name_alias: base.column_name_alias.clone(),
            with_ordinality: roastedduck_ast::OrdinalityType::WithoutOrdinality,
        }),
    }))
}

/// The replacement scans clients added (the rest of `Binder::BindWithReplacementScan`):
/// the first that reads the table names the reference to bind in its place, which takes
/// the user's alias, else the table's name, and the user's column aliases. A scan's error
/// fails the bind.
fn client_replacement_scan(
    context: &crate::ClientContext,
    base: &BaseTableRef,
    alias: &[u8],
) -> Result<Option<TableRef>> {
    let input = crate::context::ReplacementScanInput {
        context,
        catalog_name: &base.catalog_name,
        schema_name: &base.schema_name,
        table_name: &base.table_name,
    };
    for scan in &context.replacement_scans {
        let Some(mut replacement) = (scan.0)(&input)? else {
            continue;
        };
        if !alias.is_empty() {
            replacement.alias = alias.into();
        } else if replacement.alias.is_empty() {
            replacement.alias.clone_from(&base.table_name);
        }
        match &mut replacement.kind {
            TableRefKind::TableFunction(function) => {
                function.column_name_alias.clone_from(&base.column_name_alias);
            }
            TableRefKind::Subquery(subquery) => {
                subquery.column_name_alias.clone_from(&base.column_name_alias);
            }
            _ => {}
        }
        return Ok(Some(replacement));
    }
    Ok(None)
}

/// A table found while extracting table names: the name is recorded and the table binds
/// as a dummy scan (DuckDB's `EXTRACT_NAMES` branch of `Binder::Bind(BaseTableRef &)`).
fn bind_extracted_table_name(
    scope: &mut BindScope<'_>,
    base: &BaseTableRef,
    alias: &[u8],
    mode: BindingMode,
) -> Result<BoundStatement> {
    let name = if mode == BindingMode::ExtractQualifiedNames {
        TableRef {
            external_dependency: roastedduck_ast::ExternalDependency::default(),
            alias: ByteString::from(alias),
            sample: None,
            query_location: None,
            kind: TableRefKind::BaseTable(Box::new(base.clone())),
        }
        .to_sql_bytes()
    } else {
        base.table_name.clone()
    };
    scope.binder.add_table_name(name);
    let table_index = scope.generate_table_index();
    let binding_alias = if alias.is_empty() { &base.table_name } else { alias };
    let types = vec![LogicalType::INTEGER];
    let names = vec![roastedduck_common::byte_format!("__dummy_col{table_index}")];
    scope
        .bind_context
        .add_generic_binding(table_index, binding_alias, names.clone(), types.clone())?;
    let plan = LogicalOperator::new(LogicalKind::DummyScan { table_index });
    Ok(BoundStatement::new(plan, names, types))
}

/// A table, view or CTE name in FROM (DuckDB's `Binder::Bind(BaseTableRef &)`).
fn bind_base_table(
    scope: &mut BindScope<'_>,
    base: &mut BaseTableRef,
    alias: &[u8],
    location: Option<u64>,
    external_replacement: &mut Option<TableRef>,
) -> Result<BoundStatement> {
    let cte_alias = BindingAlias::qualified("", &base.schema_name, &base.table_name);
    let lookup = {
        let (holder, defining) = scope.split();
        reference_cte(holder, defining, &cte_alias)?
    };
    if let CteLookup::Found(cte_index, cte_names, types) = lookup {
        let index = scope.generate_table_index();
        let alias = if alias.is_empty() {
            base.table_name.clone()
        } else {
            ByteString::from(alias)
        };
        let names = alias_column_names(&alias, &cte_names, &base.column_name_alias)?;
        scope
            .bind_context
            .add_generic_binding(index, &alias, names.clone(), types.clone())?;
        let plan = LogicalOperator::new(LogicalKind::CteRef {
            table_index: index,
            cte_index,
            chunk_types: types.clone(),
            bound_columns: names.clone(),
            correlated_columns: 0,
            is_recurring: base.schema_name == "recurring",
        });
        return Ok(BoundStatement::new(plan, names, types));
    }
    if let Some(at_clause) = base.at_clause.as_mut() {
        // `Binder::BindAtClause`: the clause binds and evaluates as a constant; DuckDB's own
        // catalogs then refuse it (`Catalog::LookupEntry`).
        let bound = {
            let (inner, outer) = scope.split();
            let mut binder = crate::expression_binder::ConstantBinder::new(inner, outer, "AT clause");
            binder.bind(&mut at_clause.expr, true)?
        };
        scope.context.evaluator.evaluate_scalar(&bound)?;
        return Err(Error::binder("Catalog type does not support time travel"));
    }
    if !base.schema_name.is_empty()
        && base.catalog_name.is_empty()
        && scope.context.catalog(&base.schema_name).is_some()
    {
        base.catalog_name = std::mem::take(&mut base.schema_name);
    }
    let context = scope.context.clone();
    let found = context.get_entry(
        &base.catalog_name,
        &base.schema_name,
        CatalogType::Table,
        &base.table_name,
        true,
    )?;
    let mode = scope.binder.binding_mode();
    if mode.extracts_names()
        && found
            .as_ref()
            .is_none_or(|found| matches!(found.entry.kind, EntryKind::Table(_)))
    {
        return bind_extracted_table_name(scope, base, alias, mode);
    }
    let Some(found) = found else {
        if let Some(mut replacement) = csv_replacement_scan(base, alias)? {
            return bind_table_ref(scope, &mut replacement);
        }
        if let Some(mut replacement) = client_replacement_scan(&context, base, alias)? {
            if !replacement.external_dependency.is_empty() {
                // Binding consumes table-function arguments into bind data. Keep the
                // original call for a relation/view that will bind again later.
                *external_replacement = Some(replacement.clone());
            }
            return bind_table_ref(scope, &mut replacement);
        }
        // An existing file no replacement scan reads (`ExtensionHelper::IsFullPath`).
        let full_path = replacement_full_path(base);
        if full_path.iter().any(|byte| matches!(byte, b'.' | b'/' | b'\\'))
            && replacement_path_is_file(&full_path)
        {
            return Err(Error::binder(roastedduck_common::byte_format!(
                "No extension found that is capable of reading the file \"{full_path}\"\n* If this file is a supported file format you can explicitly use the reader functions, such as read_csv, read_json or read_parquet"
            )));
        }
        if matches!(lookup, CteLookup::Circular) {
            return Err(Error::binder(roastedduck_common::byte_format!(
                "Circular reference to CTE \"{}\", use WITH RECURSIVE to use recursive CTEs.",
                base.table_name
            ))
            .with_position(location));
        }
        let error = context
            .get_entry(
                &base.catalog_name,
                &base.schema_name,
                CatalogType::Table,
                &base.table_name,
                false,
            )
            .err()
            .unwrap_or_else(|| Error::internal("table lookup"));
        // Missing entries point at the table name; a missing catalog has no position.
        return Err(if error.kind() == roastedduck_common::ErrorKind::Catalog {
            error.with_position(location)
        } else {
            error
        });
    };
    match &found.entry.kind {
        EntryKind::Table(table) => {
            let table_index = scope.generate_table_index();
            let types: Vec<LogicalType> = table
                .columns
                .iter()
                .map(|column| column.logical_type.clone())
                .collect();
            let column_names: Vec<ByteString> =
                table.columns.iter().map(|column| column.name.clone()).collect();
            let names = alias_column_names(&base.table_name, &column_names, &base.column_name_alias)?;
            let binding_alias = if alias.is_empty() {
                BindingAlias::qualified(&found.catalog, &found.schema, found.entry.name())
            } else {
                BindingAlias::new(alias)
            };
            let shared_column_ids = crate::operator::SharedColumnIds::default();
            scope.bind_context.add_binding(
                Binding::new(
                    BindingKind::Table {
                        bound_column_ids: Arc::clone(&shared_column_ids),
                        entry: Some(Arc::clone(&found.entry)),
                        virtual_columns: vec![(
                            crate::operator::COLUMN_IDENTIFIER_ROW_ID,
                            "rowid".into(),
                            LogicalType::BIGINT,
                        )],
                    },
                    binding_alias,
                    types.clone(),
                    names.clone(),
                    table_index,
                )?
                .with_entry_parent(&found.catalog, &found.schema),
            );
            let get = LogicalGet {
                table_index,
                function: TableFunction::new("seq_scan", Vec::new()),
                bind_data: None,
                returned_types: types.clone(),
                names: column_names,
                column_ids: Vec::<ColumnIndex>::new(),
                projection_ids: Vec::new(),
                parameters: Vec::new(),
                named_parameters: Vec::new(),
                input_table_types: Vec::new(),
                input_table_names: Vec::new(),
                projected_input: Vec::new(),
                ordinality_index: None,
                shared_column_ids: Some(shared_column_ids),
                table: Some(Arc::clone(&found.entry)),
                table_schema: Some((found.catalog.clone(), found.schema.clone())),
                table_filters: roastedduck_storage::TableFilterSet::default(),
                dynamic_filters: None,
                sample_options: None,
                file_filters: crate::operator::FileFilterInfo::default(),
                row_group_order: None,
                is_create_index: false,
            };
            Ok(BoundStatement::new(
                LogicalOperator::new(LogicalKind::Get(Box::new(get))),
                names,
                types,
            ))
        }
        EntryKind::View(view) => {
            let mut view_binder = scope.binder.child_of_type(BinderType::View)?;
            view_binder.add_bound_view(&found.entry)?;
            view_binder.can_contain_nulls = true;
            // A view resolves names where it is stored first.
            view_binder.context = Arc::new(
                view_binder
                    .context
                    .with_view_search_path(&found.catalog, &found.schema),
            );
            let mut column_aliases = view.aliases.clone();
            for (i, column_alias) in base.column_name_alias.iter().enumerate() {
                match column_aliases.get_mut(i) {
                    Some(existing) => existing.clone_from(column_alias),
                    None => column_aliases.push(column_alias.clone()),
                }
            }
            let mut subquery = SubqueryRef {
                subquery: Box::new(view.query.clone()),
                column_name_alias: column_aliases,
            };
            let bound = {
                // A view binder has its own query state (DuckDB's `BinderType::VIEW_BINDER`):
                // the view cannot see the columns of enclosing queries or LATERAL joins.
                let mut view_scope = BindScope {
                    binder: &mut view_binder,
                    outer: None,
                };
                bind_subquery_ref(&mut view_scope, &mut subquery, alias)?
            };
            if !view_binder.correlated_columns.is_empty() {
                return Err(Error::binder(
                    "Contents of view were altered - view bound correlated columns",
                ));
            }
            let index = bound.plan.root_index()?;
            let names = alias_column_names(alias, &bound.names, &subquery.column_name_alias)?;
            let view_alias = if alias.is_empty() {
                BindingAlias::qualified(&found.catalog, &found.schema, found.entry.name())
            } else {
                BindingAlias::new(alias)
            };
            scope.bind_context.add_binding(
                Binding::new(
                    BindingKind::Entry {
                        entry: Arc::clone(&found.entry),
                    },
                    view_alias,
                    bound.types.clone(),
                    names,
                    index,
                )?
                .with_entry_parent(&found.catalog, &found.schema),
            );
            Ok(bound)
        }
        _ => Err(Error::internal("Catalog entry type")),
    }
}
