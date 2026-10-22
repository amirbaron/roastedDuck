//! Functions, types and casts a client adds to a database while it runs: the catalog and
//! configuration writes of DuckDB's C API (`duckdb_register_scalar_function_set`,
//! `duckdb_register_table_function`, `duckdb_register_logical_type`, ...).
//!
//! Catalog objects go to the `system` catalog in the connection's transaction, like
//! `ClientContext::RunFunctionInTransaction` around `Catalog::GetSystemCatalog(...)
//! .CreateFunction(...)` with `OnCreateConflict::ALTER_ON_CONFLICT`: they are rolled back
//! with an explicit transaction and become visible to other connections on commit.

use std::sync::{Arc, PoisonError};

use rustdb_ast::OnCreateConflict;
use rustdb_catalog::{Catalog, CatalogEntry, CatalogType, DEFAULT_SCHEMA, EntryKind, Transaction, TypeEntry};
use rustdb_common::{Error, Result};
use rustdb_function::cast::BoundCastInfo;
use rustdb_function::{AggregateFunctionSet, CopyFunction, ScalarFunctionSet, TableFunctionSet};
use rustdb_types::LogicalType;

use super::Connection;

impl Connection {
    /// Runs `create` against the system catalog in the connection's transaction, which
    /// is started and committed if none is open (`RunFunctionInTransaction`).
    fn create_in_system_catalog(
        &mut self,
        create: impl FnOnce(&Transaction, &Catalog) -> Result<()>,
    ) -> Result<()> {
        let transaction = self.transactions.current();
        let system = Arc::clone(&self.database.instance().system_catalog);
        let created = create(&transaction, &system);
        let ended = self.transactions.end_statement(created.is_ok(), false);
        created.and(ended)
    }

    /// Adds the scalar function set to the system catalog (`CreateScalarFunctionInfo` with
    /// `ALTER_ON_CONFLICT`): a scalar function of that name gains the overloads, those
    /// with the same signature replacing its own
    /// (`ScalarFunctionCatalogEntry::AlterEntry`).
    pub fn create_scalar_function_set(&mut self, set: ScalarFunctionSet) -> Result<()> {
        self.create_in_system_catalog(|transaction, system| {
            let name = set.name.to_string();
            let Some(existing) =
                system.get_entry(transaction, CatalogType::ScalarFunction, DEFAULT_SCHEMA, &name)
            else {
                return create_function(transaction, system, &name, EntryKind::ScalarFunction(set));
            };
            let EntryKind::ScalarFunction(current) = &existing.kind else {
                // `CatalogEntry::AlterEntry` of the other function kinds.
                return Err(Error::internal("Unsupported alter type for catalog entry!"));
            };
            let mut merged = current.clone();
            merged.merge_function_set(set, true);
            let mut kind = EntryKind::ScalarFunction(merged);
            kind.set_function_catalog(&system.name(), DEFAULT_SCHEMA);
            let mut entry = CatalogEntry::new(existing.name(), kind);
            entry.oid = existing.oid;
            entry.internal = existing.internal;
            system.replace_entry(
                transaction,
                DEFAULT_SCHEMA,
                CatalogType::ScalarFunction,
                &name,
                entry,
            )
        })
    }

    /// Adds the aggregate function set to the system catalog (`CreateAggregateFunctionInfo`
    /// with `ALTER_ON_CONFLICT`): an existing function of that name cannot be altered.
    pub fn create_aggregate_function_set(&mut self, set: AggregateFunctionSet) -> Result<()> {
        self.create_in_system_catalog(|transaction, system| {
            let name = set.name.to_string();
            if system
                .get_entry(transaction, CatalogType::AggregateFunction, DEFAULT_SCHEMA, &name)
                .is_some()
            {
                // `CreateInfo::GetAlterInfo`.
                return Err(Error::not_implemented(
                    "GetAlterInfo not implemented for this type",
                ));
            }
            create_function(transaction, system, &name, EntryKind::AggregateFunction(set))
        })
    }

    /// Adds the table function set to the system catalog (`CreateTableFunctionInfo` with
    /// `ALTER_ON_CONFLICT`): when a table function of that name exists, nothing is created
    /// (`DuckSchemaEntry::AddEntryInternal`).
    pub fn create_table_function_set(&mut self, set: TableFunctionSet) -> Result<()> {
        self.create_in_system_catalog(|transaction, system| {
            let name = set.name.to_string();
            if system
                .get_entry(transaction, CatalogType::TableFunction, DEFAULT_SCHEMA, &name)
                .is_some()
            {
                return Ok(());
            }
            create_function(transaction, system, &name, EntryKind::TableFunction(set))
        })
    }

    /// Adds the COPY format to the system catalog (`CreateCopyFunctionInfo` with
    /// `ALTER_ON_CONFLICT`): when a format of that name exists, nothing is created.
    pub fn create_copy_function(&mut self, function: CopyFunction) -> Result<()> {
        self.create_in_system_catalog(|transaction, system| {
            let name = function.name.to_string();
            if system
                .get_entry(transaction, CatalogType::CopyFunction, DEFAULT_SCHEMA, &name)
                .is_some()
            {
                return Ok(());
            }
            create_function(
                transaction,
                system,
                &name,
                EntryKind::CopyFunction(Box::new(function)),
            )
        })
    }

    /// Creates the type `name` in the system catalog (`CreateTypeInfo`, temporary and
    /// internal, as `duckdb_register_logical_type` makes it); an existing type of that
    /// name is an error.
    pub fn create_type(&mut self, name: &str, ty: LogicalType) -> Result<()> {
        self.create_in_system_catalog(|transaction, system| {
            let mut entry = CatalogEntry::new(name, EntryKind::Type(TypeEntry { user_type: ty }));
            entry.temporary = true;
            entry.internal = true;
            system
                .create_entry(
                    transaction,
                    DEFAULT_SCHEMA,
                    entry,
                    OnCreateConflict::ErrorOnConflict,
                )
                .map(drop)
        })
    }

    /// Registers the cast from `source` to `target` (DuckDB's
    /// `CastFunctionSet::RegisterCastFunction` on the database's casts); an implicit cast
    /// cost of `-1` allows it only explicitly.
    pub fn register_cast_function(
        &mut self,
        source: LogicalType,
        target: LogicalType,
        cast: BoundCastInfo,
        implicit_cost: i64,
    ) {
        let instance = self.database.instance();
        let mut casts = instance.casts.write().unwrap_or_else(PoisonError::into_inner);
        let mut registered = (**casts).clone();
        registered.register_cast_function(source, target, cast, implicit_cost);
        *casts = Arc::new(registered);
    }
}

/// Creates the internal function entry `name` in `main` of the system catalog.
fn create_function(
    transaction: &Transaction,
    system: &Catalog,
    name: &str,
    mut kind: EntryKind,
) -> Result<()> {
    kind.set_function_catalog(&system.name(), DEFAULT_SCHEMA);
    let mut entry = CatalogEntry::new(name, kind);
    entry.internal = true;
    system
        .create_entry(
            transaction,
            DEFAULT_SCHEMA,
            entry,
            OnCreateConflict::ErrorOnConflict,
        )
        .map(drop)
}
