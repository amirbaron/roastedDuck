//! What the binder needs from the running connection (the parts of DuckDB's
//! `ClientContext` the planner uses): catalog access in a transaction, settings, casts,
//! prepared statements and constant folding.

use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;

use rustdb_ast::{OrderByNullType, OrderType};
use rustdb_catalog::{Catalog, CatalogEntry, CatalogType, DEFAULT_SCHEMA, Transaction};
use rustdb_common::string_util::similarity_rating;
use rustdb_common::{Error, Result};
use rustdb_function::FunctionClient;
use rustdb_function::cast::CastFunctionSet;
use rustdb_types::Value;

use crate::expression::Expression;
use crate::prepared::PreparedStatementData;

/// Evaluates foldable expressions at bind time (DuckDB's `ExpressionExecutor::EvaluateScalar`,
/// which lives in the execution layer above the planner).
pub trait ExpressionEvaluator: Send + Sync + fmt::Debug {
    /// The value of a foldable expression.
    fn evaluate_scalar(&self, expression: &Expression) -> Result<Value>;
}

/// The settings the binder reads (DuckDB's `ClientConfig`/`DBConfig` options).
#[derive(Clone, Debug)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "one flag per boolean setting, like DuckDB's options"
)]
pub struct PlannerConfig {
    /// `default_order`: the direction of `ORDER BY x` without `ASC`/`DESC`.
    pub default_order: OrderType,
    /// `default_null_order`.
    pub default_null_order: DefaultNullOrder,
    /// `max_expression_depth`.
    pub max_expression_depth: u64,
    /// `order_by_non_integer_literal`: allow `ORDER BY 'a'`.
    pub order_by_non_integer_literal: bool,
    /// `catalog_error_max_schemas`.
    pub catalog_error_max_schemas: usize,
    /// `scalar_subquery_error_on_multiple_rows`: scalar subqueries returning several rows
    /// raise an error (instead of returning one of them).
    pub scalar_subquery_error_on_multiple_rows: bool,
    /// `default_collation`: the collation of VARCHAR values without one, lowercase.
    pub default_collation: String,
    /// `pivot_filter_threshold`: up to this many pivot columns, PIVOT uses filtered
    /// aggregates instead of the LIST-based plan.
    pub pivot_filter_threshold: u64,
    /// `pivot_limit`: the maximum number of pivot columns.
    pub pivot_limit: u64,
    /// `enable_view_dependencies`: views depend on the entries they reference.
    pub enable_view_dependencies: bool,
    /// `enable_macro_dependencies`: macros depend on the entries they reference.
    pub enable_macro_dependencies: bool,
}

/// The `default_null_order` setting (DuckDB's `DefaultOrderByNullType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DefaultNullOrder {
    NullsFirst,
    NullsLast,
    NullsFirstOnAscLastOnDesc,
    NullsLastOnAscFirstOnDesc,
}

impl Default for PlannerConfig {
    fn default() -> Self {
        Self {
            default_order: OrderType::Ascending,
            default_null_order: DefaultNullOrder::NullsLast,
            max_expression_depth: 1000,
            order_by_non_integer_literal: false,
            catalog_error_max_schemas: 100,
            scalar_subquery_error_on_multiple_rows: true,
            default_collation: String::new(),
            pivot_filter_threshold: 20,
            pivot_limit: 100_000,
            enable_view_dependencies: false,
            enable_macro_dependencies: false,
        }
    }
}

impl PlannerConfig {
    /// The direction of an order term (DuckDB's `DBConfig::ResolveOrder`).
    pub fn resolve_order(&self, order: OrderType) -> OrderType {
        if order == OrderType::OrderDefault {
            self.default_order
        } else {
            order
        }
    }

    /// The NULL placement of an order term (DuckDB's `DBConfig::ResolveNullOrder`).
    pub fn resolve_null_order(&self, order: OrderType, null_order: OrderByNullType) -> OrderByNullType {
        if null_order != OrderByNullType::OrderDefault {
            return null_order;
        }
        match (self.default_null_order, order == OrderType::Ascending) {
            (DefaultNullOrder::NullsFirst, _)
            | (DefaultNullOrder::NullsFirstOnAscLastOnDesc, true)
            | (DefaultNullOrder::NullsLastOnAscFirstOnDesc, false) => OrderByNullType::NullsFirst,
            _ => OrderByNullType::NullsLast,
        }
    }
}

/// The context a statement is bound in.
#[derive(Clone, Debug)]
pub struct ClientContext {
    /// The attached catalogs, the default one first; the `system` catalog holds built-ins.
    pub catalogs: Vec<Arc<Catalog>>,
    /// The `(catalog, schema)` entries of `search_path` / `USE`; empty for the default
    /// (DuckDB's `CatalogSearchPath::set_paths`).
    pub search_path: Vec<(String, String)>,
    pub transaction: Arc<Transaction>,
    pub casts: Arc<CastFunctionSet>,
    pub evaluator: Arc<dyn ExpressionEvaluator>,
    pub config: PlannerConfig,
    /// Prepared statements of the connection, by name.
    pub prepared_statements: rustdb_common::CaseInsensitiveMap<Arc<PreparedStatementData>>,
    /// Connection state functions use (random engine, ...).
    pub function_client: Arc<FunctionClient>,
    /// The entries looked up while recording (DuckDB's catalog lookup callback of the
    /// binder, which collects the dependencies of a created entry).
    pub dependency_recorder: Arc<std::sync::Mutex<Option<Vec<EntryLookup>>>>,
    /// `SET VARIABLE` values of the connection (DuckDB's `ClientConfig::user_variables`).
    pub user_variables: rustdb_common::CaseInsensitiveMap<Value>,
}

/// A catalog entry found by a lookup, with where it was found.
#[derive(Clone, Debug)]
pub struct EntryLookup {
    pub catalog: String,
    pub schema: String,
    pub entry: Arc<CatalogEntry>,
}

impl ClientContext {
    /// Starts recording the entries looked up; returns the recording it interrupts.
    pub fn start_recording_dependencies(&self) -> Option<Vec<EntryLookup>> {
        self.dependency_recorder
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .replace(Vec::new())
    }

    /// Stops recording and returns the entries looked up since the start; `previous`
    /// (from [`ClientContext::start_recording_dependencies`]) resumes.
    pub fn stop_recording_dependencies(&self, previous: Option<Vec<EntryLookup>>) -> Vec<EntryLookup> {
        let mut recorder = self
            .dependency_recorder
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let recorded = recorder.take().unwrap_or_default();
        *recorder = previous;
        if let Some(outer) = recorder.as_mut() {
            outer.extend(recorded.iter().cloned());
        }
        recorded
    }

    /// The catalog called `name` (case-insensitive).
    pub fn catalog(&self, name: &str) -> Option<&Arc<Catalog>> {
        self.catalogs
            .iter()
            .find(|catalog| catalog.name().eq_ignore_ascii_case(name))
    }

    /// The name of the default catalog (`DatabaseManager::GetDefaultDatabase`): the
    /// catalog of the first `search_path` entry, else the database opened first.
    pub fn default_catalog(&self) -> &str {
        match self.search_path.first() {
            Some((catalog, _)) => catalog,
            None => self.catalogs.first().map_or("", |catalog| catalog.name()),
        }
    }

    /// `Binder::BindSchemaOrCatalog`: a lone `schema` that names an attached database
    /// stands for that database, unless the default catalog also has a schema of that
    /// name (an ambiguous reference).
    pub fn bind_schema_or_catalog(&self, catalog: &str, schema: &str) -> Result<(String, String)> {
        if !catalog.is_empty() || schema.is_empty() || self.catalog(schema).is_none() {
            return Ok((catalog.to_owned(), schema.to_owned()));
        }
        let mut catalog_names: Vec<String> = self
            .search_path
            .iter()
            .filter(|(_, path_schema)| path_schema.eq_ignore_ascii_case(schema))
            .map(|(path_catalog, _)| path_catalog.clone())
            .collect();
        if catalog_names.is_empty() {
            catalog_names.push(self.default_catalog().to_owned());
        }
        for catalog_name in catalog_names {
            if self
                .catalog(&catalog_name)
                .is_some_and(|found| found.schema(&self.transaction, schema).is_some())
            {
                return Err(Error::binder(format!(
                    "Ambiguous reference to catalog or schema \"{schema}\" - use a fully qualified path like \"{catalog_name}.{schema}\""
                )));
            }
        }
        Ok((schema.to_owned(), String::new()))
    }

    /// The default schema (`CatalogSearchPath::GetDefault`).
    pub fn default_schema(&self) -> &str {
        self.search_path
            .first()
            .map_or(DEFAULT_SCHEMA, |(_, schema)| schema.as_str())
    }

    /// The default schema of `catalog` (`CatalogSearchPath::GetDefaultSchema`): that of
    /// the first search path entry in it, else `main`.
    pub fn default_schema_for(&self, catalog: &str) -> String {
        self.search_path
            .iter()
            .find(|(path_catalog, _)| path_catalog.eq_ignore_ascii_case(catalog))
            .map_or_else(|| DEFAULT_SCHEMA.to_owned(), |(_, schema)| schema.clone())
    }

    /// The catalog an entry of `schema` goes to by default
    /// (`CatalogSearchPath::GetDefaultCatalog`).
    pub fn default_catalog_for(&self, schema: &str) -> String {
        self.search_path
            .iter()
            .find(|(_, path_schema)| path_schema.eq_ignore_ascii_case(schema))
            .map_or_else(
                || self.default_catalog().to_owned(),
                |(catalog, _)| catalog.clone(),
            )
    }

    /// Every `(catalog, schema)` searched for unqualified names, in order
    /// (`CatalogSearchPath::Get`): the set entries, the default database's `main`, then
    /// `system.main`.
    pub fn full_search_path(&self) -> Vec<(String, String)> {
        let mut paths = self.search_path.clone();
        paths.push((self.default_catalog().to_owned(), DEFAULT_SCHEMA.to_owned()));
        paths.push(("system".to_owned(), DEFAULT_SCHEMA.to_owned()));
        paths.push(("system".to_owned(), "pg_catalog".to_owned()));
        let mut unique: Vec<(String, String)> = Vec::with_capacity(paths.len());
        for path in paths {
            if !unique
                .iter()
                .any(|(c, s)| c.eq_ignore_ascii_case(&path.0) && s.eq_ignore_ascii_case(&path.1))
            {
                unique.push(path);
            }
        }
        unique
    }

    /// The `(catalog, schema)` pairs searched for a name qualified by `catalog` and
    /// `schema`, either of which may be empty (`Catalog::GetCatalogEntries`).
    fn search_path(&self, catalog: &str, schema: &str) -> Vec<(String, String)> {
        match (catalog.is_empty(), schema.is_empty()) {
            (true, true) => self.full_search_path(),
            (true, false) if is_default_schema(schema) => vec![("system".to_owned(), schema.to_lowercase())],
            (true, false) => {
                let mut entries: Vec<(String, String)> = self
                    .full_search_path()
                    .into_iter()
                    .filter(|(_, path_schema)| path_schema.eq_ignore_ascii_case(schema))
                    .map(|(path_catalog, _)| (path_catalog, schema.to_owned()))
                    .collect();
                if entries.is_empty() {
                    entries.push((self.default_catalog().to_owned(), schema.to_owned()));
                }
                entries
            }
            (false, true) => vec![(catalog.to_owned(), self.default_schema_for(catalog))],
            (false, false) => vec![(catalog.to_owned(), schema.to_owned())],
        }
    }

    fn lookup_in(
        &self,
        catalog: &str,
        schema: &str,
        ty: CatalogType,
        name: &str,
    ) -> Option<Arc<CatalogEntry>> {
        self.catalog(catalog)?
            .get_entry(&self.transaction, ty, schema, name)
    }

    /// Looks up an entry like DuckDB's `CatalogEntryRetriever::GetEntry`: through the search
    /// path for unqualified names. `Ok(None)` if not found and `if_exists`; otherwise a
    /// missing entry is a Catalog Error with "Did you mean" suggestions.
    pub fn get_entry(
        &self,
        catalog: &str,
        schema: &str,
        ty: CatalogType,
        name: &str,
        if_exists: bool,
    ) -> Result<Option<EntryLookup>> {
        let path = self.search_path(catalog, schema);
        for (catalog_name, schema_name) in &path {
            if let Some(entry) = self.lookup_in(catalog_name, schema_name, ty, name) {
                let lookup = EntryLookup {
                    catalog: catalog_name.clone(),
                    schema: schema_name.clone(),
                    entry,
                };
                if let Some(recorded) = self
                    .dependency_recorder
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .as_mut()
                {
                    recorded.push(lookup.clone());
                }
                return Ok(Some(lookup));
            }
        }
        if if_exists {
            return Ok(None);
        }
        if !catalog.is_empty() && self.catalog(catalog).is_none() {
            return Err(Error::binder(format!("Catalog \"{catalog}\" does not exist!")));
        }
        // A table in a named schema that exists nowhere: the error is about the schema.
        if ty == CatalogType::Table
            && !schema.is_empty()
            && !path.iter().any(|(catalog_name, schema_name)| {
                self.catalog(catalog_name)
                    .is_some_and(|catalog| catalog.schema(&self.transaction, schema_name).is_some())
            })
        {
            return Err(Error::catalog(format!(
                "Table with name \"{schema}.{name}\" does not exist because schema \"{schema}\" does not exist."
            )));
        }
        Err(self.missing_entry_error(&path, ty, name))
    }

    /// The best similar entry name in each schema, with the catalog and schema it is in
    /// (DuckDB's `SimilarEntriesInSchemas`).
    fn similar_entries(
        &self,
        schemas: &[(String, String)],
        ty: CatalogType,
        name: &str,
    ) -> (f64, Vec<(String, String, String)>) {
        let mut best = 0.0;
        let mut results: Vec<(String, String, String)> = Vec::new();
        for (catalog_name, schema_name) in schemas {
            let Some(catalog) = self.catalog(catalog_name) else {
                continue;
            };
            let Some(schema) = catalog.schema(&self.transaction, schema_name) else {
                continue;
            };
            let Some(schema) = schema.as_schema() else {
                continue;
            };
            let set = match ty {
                CatalogType::Table | CatalogType::View => &schema.tables,
                CatalogType::TableFunction | CatalogType::TableMacro => &schema.table_functions,
                CatalogType::Sequence => &schema.sequences,
                CatalogType::Index => &schema.indexes,
                CatalogType::Type => &schema.types,
                CatalogType::Collation => &schema.collations,
                CatalogType::PragmaFunction => &schema.pragma_functions,
                CatalogType::CopyFunction => &schema.copy_functions,
                _ => &schema.functions,
            };
            let mut entry_score = 0.0;
            let mut entry_name = String::new();
            let mut consider = |candidate: &str| {
                let score = similarity_rating(&candidate.to_lowercase(), name);
                if score > entry_score {
                    entry_score = score;
                    candidate.clone_into(&mut entry_name);
                }
            };
            if ty == CatalogType::Type && schema_name.eq_ignore_ascii_case(DEFAULT_SCHEMA) {
                // The builtin types of every `main` schema (`DefaultTypeGenerator`).
                for builtin in rustdb_types::builtin_type_names() {
                    consider(builtin);
                }
            }
            for entry in set.scan(&self.transaction) {
                consider(entry.name());
            }
            if entry_name.is_empty() {
                continue;
            }
            if results.is_empty() || best <= entry_score {
                if best < entry_score {
                    results.clear();
                }
                best = entry_score;
                results.push((catalog.name().to_owned(), schema_name.clone(), entry_name));
            }
        }
        (best, results)
    }

    /// `name` in `catalog.schema`, qualified only as far as the search path requires
    /// (DuckDB's `FindMinimalQualification` with `SimilarCatalogEntry::GetQualifiedName`).
    fn minimally_qualified(&self, catalog: &str, schema: &str, name: &str) -> String {
        let reaches = |paths: Vec<(String, String)>| {
            paths
                .iter()
                .any(|(c, s)| c.eq_ignore_ascii_case(catalog) && s.eq_ignore_ascii_case(schema))
        };
        if reaches(self.search_path("", schema)) {
            format!("{schema}.{name}")
        } else if reaches(self.search_path(catalog, "")) {
            format!("{catalog}.{name}")
        } else {
            format!("{catalog}.{schema}.{name}")
        }
    }

    /// DuckDB's `Catalog::CreateMissingEntryException`.
    fn missing_entry_error(&self, searched: &[(String, String)], ty: CatalogType, name: &str) -> Error {
        const UNSEEN_PENALTY: f64 = 0.2;
        let (seen_score, seen) = self.similar_entries(searched, ty, name);
        let mut unseen_schemas = Vec::new();
        for catalog in &self.catalogs {
            for schema in catalog.schemas().scan(&self.transaction) {
                let key = (catalog.name().to_owned(), schema.name().to_owned());
                if unseen_schemas.len() < self.config.catalog_error_max_schemas {
                    unseen_schemas.push(key);
                }
            }
        }
        let unseen_schemas: Vec<(String, String)> = unseen_schemas
            .into_iter()
            .filter(|key| !searched.contains(key))
            .collect();
        let (unseen_score, unseen) = self.similar_entries(&unseen_schemas, ty, name);
        let mut suggestions = BTreeSet::new();
        if !unseen.is_empty() && (unseen_score >= 1.0 || unseen_score - UNSEEN_PENALTY > seen_score) {
            // Entries outside the search path need (minimal) qualification.
            for (catalog, schema, entry) in unseen {
                suggestions.insert(self.minimally_qualified(&catalog, &schema, &entry));
            }
        } else {
            suggestions.extend(seen.into_iter().map(|(_, _, entry)| entry));
        }
        let mut suggestions: Vec<String> = suggestions.into_iter().collect();
        let did_you_mean = if suggestions.len() > 2 {
            let last = suggestions.pop().unwrap_or_default();
            format!("{}, or {last}", suggestions.join(", "))
        } else {
            suggestions.join(" or ")
        };
        let hint = if did_you_mean.is_empty() {
            String::new()
        } else {
            format!("\nDid you mean \"{did_you_mean}\"?")
        };
        Error::catalog(format!(
            "{} with name {name} does not exist!{hint}",
            ty.display_name()
        ))
    }
}

/// Whether `schema` is one of the schemas of the system catalog every database shows
/// (DuckDB's `DefaultSchemaGenerator::IsDefaultSchema`).
pub fn is_default_schema(schema: &str) -> bool {
    schema.eq_ignore_ascii_case("information_schema") || schema.eq_ignore_ascii_case("pg_catalog")
}

/// Attaches `location` to a missing-entry error of a catalog lookup; errors about a
/// missing catalog or schema have no position in DuckDB.
pub fn position_missing_entry(error: Error, location: Option<u64>) -> Error {
    if error.kind() == rustdb_common::ErrorKind::Catalog {
        error.with_position(location)
    } else {
        error
    }
}
