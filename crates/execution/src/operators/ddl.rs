//! Catalog statements: `CREATE SCHEMA`, `CREATE TABLE`, `CREATE VIEW` and `DROP`
//! (DuckDB's `PhysicalCreateSchema`, `PhysicalCreateTable`, `PhysicalCreateView`,
//! `PhysicalDrop` and the catalog methods they call).

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use rustdb_ast::{
    CatalogType as AstCatalogType, Constraint, CreateInfo, CreateInfoKind, DropInfo, ForeignKeyConstraint,
    ForeignKeyType, OnEntryNotFound, TableColumnType,
};
use rustdb_catalog::{Catalog, CatalogEntry, CatalogType, EntryKind, TableEntry, ViewEntry};
use rustdb_common::{Error, Result};
use rustdb_storage::index::IndexConstraintType;
use rustdb_storage::{ColumnDefinition, DataTable, TableIoManager, Transaction};
use rustdb_types::{DataChunk, LogicalType};

use super::insert::TableCreator;
use crate::operator::{ErasedSource, PhysicalOperator, PhysicalRef, SinkState};
use crate::{ExecutionContext, Source, SourceResult};

/// A catalog change executed by [`PhysicalDdl`].
pub trait DdlAction: Send + Sync + fmt::Debug {
    fn execute(&self, transaction: &Transaction) -> Result<()>;
}

/// Runs a catalog change once and returns no rows.
#[derive(Debug)]
pub struct PhysicalDdl {
    name: &'static str,
    types: Vec<LogicalType>,
    action: Arc<dyn DdlAction>,
    transaction: Arc<Transaction>,
}

impl PhysicalDdl {
    pub fn new(
        name: &'static str,
        types: Vec<LogicalType>,
        action: Arc<dyn DdlAction>,
        transaction: Arc<Transaction>,
    ) -> Self {
        Self {
            name,
            types,
            action,
            transaction,
        }
    }
}

impl PhysicalOperator for PhysicalDdl {
    fn name(&self) -> &'static str {
        self.name
    }
    fn types(&self) -> &[LogicalType] {
        &self.types
    }
    fn children(&self) -> &[PhysicalRef] {
        &[]
    }
    fn as_source(&self) -> Option<&dyn ErasedSource> {
        Some(self)
    }
}

impl Source for PhysicalDdl {
    type Global = AtomicBool;
    type Local = ();

    fn global_source_state(&self, _: &ExecutionContext, _: Option<&SinkState>) -> Result<AtomicBool> {
        Ok(AtomicBool::new(false))
    }

    fn local_source_state(&self, _: &ExecutionContext, _: &AtomicBool) -> Result<()> {
        Ok(())
    }

    fn get_data(
        &self,
        _: &ExecutionContext,
        chunk: &mut DataChunk,
        done: &AtomicBool,
        (): &mut (),
    ) -> Result<SourceResult> {
        if !done.swap(true, Ordering::AcqRel) {
            self.action.execute(&self.transaction)?;
        }
        chunk.set_len(0);
        Ok(SourceResult::Finished)
    }
}

/// `CREATE SCHEMA`.
#[derive(Debug)]
pub struct CreateSchemaAction {
    pub catalog: Arc<Catalog>,
    pub info: CreateInfo,
}

impl DdlAction for CreateSchemaAction {
    fn execute(&self, transaction: &Transaction) -> Result<()> {
        self.catalog
            .create_schema(transaction, &self.info.schema, self.info.on_conflict)
    }
}

/// `CREATE VIEW` (DuckDB's `DuckSchemaEntry::CreateView`).
#[derive(Debug)]
pub struct CreateViewAction {
    pub catalog: Arc<Catalog>,
    pub info: CreateInfo,
}

impl DdlAction for CreateViewAction {
    fn execute(&self, transaction: &Transaction) -> Result<()> {
        let CreateInfoKind::View(view) = &self.info.kind else {
            return Err(Error::internal("expected a view"));
        };
        let entry = CatalogEntry::new(
            view.view_name.clone(),
            EntryKind::View(ViewEntry {
                query: (*view.query).clone(),
                aliases: view.aliases.clone(),
                types: view.types.clone(),
                names: view.names.clone(),
            }),
        );
        self.catalog
            .create_entry(transaction, &self.info.schema, entry, self.info.on_conflict)?;
        Ok(())
    }
}

/// Creates a table's storage and catalog entry (DuckDB's `DuckSchemaEntry::CreateTable`
/// with the `DuckTableEntry` constructor), for `CREATE TABLE [AS]`.
#[derive(Debug)]
pub struct CatalogTableCreator {
    pub catalog: Arc<Catalog>,
    pub schema: String,
    pub info: CreateInfo,
    pub io: Arc<TableIoManager>,
}

impl TableCreator for CatalogTableCreator {
    fn create(&self, transaction: &Transaction) -> Result<Option<Arc<DataTable>>> {
        let CreateInfoKind::Table(table) = &self.info.kind else {
            return Err(Error::internal("expected a table"));
        };
        let mut entry = TableEntry::from_create_info(table);
        let columns: Vec<ColumnDefinition> = table
            .columns
            .columns
            .iter()
            .filter(|c| c.category != TableColumnType::Generated)
            .map(|c| ColumnDefinition::new(c.name.clone(), c.logical_type.clone()))
            .collect();
        let storage = DataTable::new(Arc::clone(&self.io), self.schema.clone(), table.table.clone(), columns);
        // The indexes of the UNIQUE, PRIMARY KEY and FOREIGN KEY constraints.
        for (i, constraint) in table.constraints.iter().enumerate() {
            match constraint {
                Constraint::Unique(unique) => {
                    let keys = unique_keys(&entry, unique)?;
                    let kind = if unique.is_primary_key {
                        IndexConstraintType::Primary
                    } else {
                        IndexConstraintType::Unique
                    };
                    storage.add_constraint_index(kind, &keys, i)?;
                }
                Constraint::ForeignKey(fk)
                    if matches!(
                        fk.fk_type,
                        ForeignKeyType::FkTypeForeignKeyTable | ForeignKeyType::FkTypeSelfReferenceTable
                    ) =>
                {
                    let keys: Vec<usize> = fk.fk_keys.iter().map(|k| k.0 as usize).collect();
                    storage.add_constraint_index(IndexConstraintType::Foreign, &keys, i)?;
                }
                _ => {}
            }
        }
        entry.storage = Some(Arc::clone(&storage));
        let name = table.table.clone();
        let created = self.catalog.create_entry(
            transaction,
            &self.schema,
            CatalogEntry::new(name.clone(), EntryKind::Table(entry)),
            self.info.on_conflict,
        )?;
        if !created {
            return Ok(None);
        }
        // Register the foreign keys at the referenced tables (`AlterForeignKeyInfo`).
        for constraint in &table.constraints {
            let Constraint::ForeignKey(fk) = constraint else {
                continue;
            };
            if fk.fk_type != ForeignKeyType::FkTypeForeignKeyTable {
                continue;
            }
            alter_referenced_table(&self.catalog, transaction, &fk.schema, &fk.table, |constraints| {
                constraints.push(Constraint::ForeignKey(Box::new(ForeignKeyConstraint {
                    pk_columns: fk.pk_columns.clone(),
                    fk_columns: fk.fk_columns.clone(),
                    fk_type: ForeignKeyType::FkTypePrimaryKeyTable,
                    schema: self.schema.clone(),
                    table: name.clone(),
                    pk_keys: fk.pk_keys.clone(),
                    fk_keys: fk.fk_keys.clone(),
                })));
            })?;
        }
        Ok(Some(storage))
    }
}

/// The physical key columns of a UNIQUE constraint of a new table.
fn unique_keys(entry: &TableEntry, unique: &rustdb_ast::UniqueConstraint) -> Result<Vec<usize>> {
    if unique.index.0 != u64::MAX {
        return entry
            .logical_to_physical(unique.index.0 as usize)
            .map(|p| vec![p])
            .ok_or_else(|| Error::internal("UNIQUE constraint on a generated column"));
    }
    unique
        .columns
        .iter()
        .map(|name| {
            entry
                .column(name)
                .and_then(|(logical, _)| entry.logical_to_physical(logical))
                .ok_or_else(|| Error::internal("UNIQUE constraint on an unknown column"))
        })
        .collect()
}

/// Replaces the table `name` by a version whose constraints `alter` changed.
fn alter_referenced_table(
    catalog: &Catalog,
    transaction: &Transaction,
    schema: &str,
    name: &str,
    alter: impl FnOnce(&mut Vec<Constraint>),
) -> Result<()> {
    let Some(existing) = catalog.get_entry(transaction, CatalogType::Table, schema, name) else {
        return Ok(());
    };
    let EntryKind::Table(table) = &existing.kind else {
        return Ok(());
    };
    let mut table = table.clone();
    alter(&mut table.constraints);
    let mut entry = CatalogEntry::new(existing.name(), EntryKind::Table(table));
    entry.temporary = existing.temporary;
    catalog.replace_entry(transaction, schema, CatalogType::Table, existing.name(), entry)
}

/// `DROP` (DuckDB's `PhysicalDrop` / `DuckSchemaEntry::DropEntry`).
#[derive(Debug)]
pub struct DropAction {
    pub catalog: Arc<Catalog>,
    pub info: DropInfo,
}

impl DdlAction for DropAction {
    fn execute(&self, transaction: &Transaction) -> Result<()> {
        let info = &self.info;
        let if_exists = info.if_not_found == OnEntryNotFound::ReturnNull;
        let ty = match info.catalog_type {
            AstCatalogType::SchemaEntry => CatalogType::Schema,
            AstCatalogType::TableEntry => CatalogType::Table,
            AstCatalogType::ViewEntry => CatalogType::View,
            AstCatalogType::SequenceEntry => CatalogType::Sequence,
            AstCatalogType::MacroEntry => CatalogType::Macro,
            AstCatalogType::TableMacroEntry => CatalogType::TableMacro,
            AstCatalogType::IndexEntry => CatalogType::Index,
            AstCatalogType::TypeEntry => CatalogType::Type,
            other => {
                return Err(Error::not_implemented(format!("DROP {}", other.name())));
            }
        };
        if ty == CatalogType::Table
            && let Some(existing) = self
                .catalog
                .get_entry(transaction, CatalogType::Table, &info.schema, &info.name)
            && let EntryKind::Table(table) = &existing.kind
        {
            // `FindForeignKeyInformation` with `AFT_DELETE`.
            for constraint in &table.constraints {
                let Constraint::ForeignKey(fk) = constraint else {
                    continue;
                };
                if fk.fk_type == ForeignKeyType::FkTypePrimaryKeyTable {
                    return Err(Error::catalog(format!(
                        "Could not drop the table because this table is main key table of the table \"{}\"",
                        fk.table
                    )));
                }
            }
            let name = existing.name().to_owned();
            self.catalog
                .drop_entry(transaction, ty, &info.schema, &info.name, info.cascade, if_exists)?;
            for constraint in &table.constraints {
                let Constraint::ForeignKey(fk) = constraint else {
                    continue;
                };
                if fk.fk_type != ForeignKeyType::FkTypeForeignKeyTable {
                    continue;
                }
                alter_referenced_table(&self.catalog, transaction, &fk.schema, &fk.table, |constraints| {
                    constraints.retain(|c| {
                        !matches!(c, Constraint::ForeignKey(other)
                            if other.fk_type == ForeignKeyType::FkTypePrimaryKeyTable
                                && other.table.eq_ignore_ascii_case(&name))
                    });
                })?;
            }
            return Ok(());
        }
        self.catalog
            .drop_entry(transaction, ty, &info.schema, &info.name, info.cascade, if_exists)
    }
}
