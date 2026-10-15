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
    pub transaction: Arc<Transaction>,
    pub casts: Arc<CastFunctionSet>,
    pub evaluator: Arc<dyn ExpressionEvaluator>,
    pub config: PlannerConfig,
    /// Prepared statements of the connection, by name.
    pub prepared_statements: rustdb_common::CaseInsensitiveMap<Arc<PreparedStatementData>>,
}

/// A catalog entry found by a lookup, with where it was found.
#[derive(Clone, Debug)]
pub struct EntryLookup {
    pub catalog: String,
    pub schema: String,
    pub entry: Arc<CatalogEntry>,
}

impl ClientContext {
    /// The catalog called `name` (case-insensitive).
    pub fn catalog(&self, name: &str) -> Option<&Arc<Catalog>> {
        self.catalogs
            .iter()
            .find(|catalog| catalog.name().eq_ignore_ascii_case(name))
    }

    /// The name of the default catalog.
    pub fn default_catalog(&self) -> &str {
        self.catalogs.first().map_or("", |catalog| catalog.name())
    }

    /// The `(catalog, schema)` pairs searched for unqualified names (DuckDB's default
    /// `CatalogSearchPath`).
    fn search_path(&self, catalog: &str, schema: &str) -> Vec<(String, String)> {
        match (catalog.is_empty(), schema.is_empty()) {
            (true, true) => self
                .catalogs
                .iter()
                .map(|catalog| (catalog.name().to_owned(), DEFAULT_SCHEMA.to_owned()))
                .collect(),
            (true, false) => self
                .catalogs
                .iter()
                .map(|catalog| (catalog.name().to_owned(), schema.to_owned()))
                .collect(),
            (false, true) => vec![(catalog.to_owned(), DEFAULT_SCHEMA.to_owned())],
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
                return Ok(Some(EntryLookup {
                    catalog: catalog_name.clone(),
                    schema: schema_name.clone(),
                    entry,
                }));
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

    /// The best similar entry name in each schema (DuckDB's `SimilarEntriesInSchemas`).
    fn similar_entries(
        &self,
        schemas: &[(String, String)],
        ty: CatalogType,
        name: &str,
    ) -> (f64, Vec<String>) {
        let mut best = 0.0;
        let mut results: Vec<String> = Vec::new();
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
                _ => &schema.functions,
            };
            let mut entry_score = 0.0;
            let mut entry_name = String::new();
            for entry in set.scan(&self.transaction) {
                let score = similarity_rating(&entry.name().to_lowercase(), name);
                if score > entry_score {
                    entry_score = score;
                    entry.name().clone_into(&mut entry_name);
                }
            }
            if entry_name.is_empty() {
                continue;
            }
            if results.is_empty() || best <= entry_score {
                if best < entry_score {
                    results.clear();
                }
                best = entry_score;
                results.push(entry_name);
            }
        }
        (best, results)
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
            suggestions.extend(unseen);
        } else {
            suggestions.extend(seen);
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

/// Attaches `location` to a missing-entry error of a catalog lookup; errors about a
/// missing catalog or schema have no position in DuckDB.
pub fn position_missing_entry(error: Error, location: Option<u64>) -> Error {
    if error.kind() == rustdb_common::ErrorKind::Catalog {
        error.with_position(location)
    } else {
        error
    }
}
