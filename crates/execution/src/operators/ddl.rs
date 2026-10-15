//! Catalog statements: `CREATE SCHEMA`, `CREATE TABLE`, `CREATE VIEW` and `DROP`
//! (DuckDB's `PhysicalCreateSchema`, `PhysicalCreateTable`, `PhysicalCreateView`,
//! `PhysicalDrop` and the catalog methods they call).

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use rustdb_ast::{
    CatalogType as AstCatalogType, Constraint, CreateIndexInfo, CreateInfo, CreateInfoKind, DropInfo,
    ForeignKeyConstraint, ForeignKeyType, IndexConstraintType as AstIndexConstraintType, OnCreateConflict,
    OnEntryNotFound, TableColumnType,
};
use rustdb_catalog::{
    Catalog, CatalogEntry, CatalogType, EntryKind, IndexEntry, SequenceEntry, TableEntry, ViewEntry,
};
use rustdb_common::{Error, Result};
use rustdb_planner::{Expression, ExpressionKind};
use rustdb_storage::index::IndexConstraintType;
use rustdb_storage::index::art::{Art, IndexKeyExecutor, IndexKeyExpressions};
use rustdb_storage::{ColumnDefinition, DataTable, TableIoManager, Transaction};
use rustdb_types::{DataChunk, LogicalType};

use super::insert::TableCreator;
use crate::expression_executor::ExpressionExecutor;
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

/// `CREATE SEQUENCE` (DuckDB's `DuckSchemaEntry::CreateSequence`).
#[derive(Debug)]
pub struct CreateSequenceAction {
    pub catalog: Arc<Catalog>,
    pub info: CreateInfo,
}

impl DdlAction for CreateSequenceAction {
    fn execute(&self, transaction: &Transaction) -> Result<()> {
        let CreateInfoKind::Sequence(sequence) = &self.info.kind else {
            return Err(Error::internal("expected a sequence"));
        };
        let mut entry = CatalogEntry::new(
            sequence.name.clone(),
            EntryKind::Sequence(SequenceEntry::new(sequence)),
        );
        entry.temporary = self.info.temporary;
        self.catalog
            .create_entry(transaction, &self.info.schema, entry, self.info.on_conflict)?;
        Ok(())
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

/// `CREATE TYPE` and `CREATE MACRO` (DuckDB's `DuckSchemaEntry::CreateType` and
/// `CreateFunction` with a `CreateMacroInfo`): the info is already bound.
#[derive(Debug)]
pub struct CreateEntryAction {
    pub catalog: Arc<Catalog>,
    pub info: CreateInfo,
}

impl DdlAction for CreateEntryAction {
    fn execute(&self, transaction: &Transaction) -> Result<()> {
        let entry = rustdb_catalog::checkpoint::entry_from_create_info(&self.info)?;
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
        let storage = DataTable::new(
            Arc::clone(&self.io),
            self.schema.clone(),
            table.table.clone(),
            columns,
        );
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
            && let Some(existing) =
                self.catalog
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

/// Evaluates the key expressions of an expression index over table chunks.
#[derive(Debug)]
struct IndexExpressionExecutor {
    executor: std::sync::Mutex<ExpressionExecutor>,
    names: Vec<String>,
}

impl IndexKeyExecutor for IndexExpressionExecutor {
    fn execute(&self, table_chunk: &DataChunk) -> Result<DataChunk> {
        let mut keys = DataChunk::default();
        self.executor
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .execute(table_chunk, &mut keys)?;
        Ok(keys)
    }

    fn names(&self) -> &[String] {
        &self.names
    }
}

/// `CREATE INDEX` (DuckDB's `PhysicalCreateIndex` with the ART build): builds the index
/// over the table's rows, adds it to the table and creates its catalog entry.
#[derive(Debug)]
pub struct CreateIndexAction {
    pub catalog: Arc<Catalog>,
    pub info: CreateInfo,
    pub table: Arc<CatalogEntry>,
    /// The key expressions over the table's physical columns.
    pub expressions: Vec<Expression>,
}

impl CreateIndexAction {
    /// An empty ART over the keys.
    fn new_art(&self, storage: &DataTable, index: &CreateIndexInfo) -> Result<Art> {
        let constraint = match index.constraint_type {
            AstIndexConstraintType::Unique => IndexConstraintType::Unique,
            AstIndexConstraintType::Primary => IndexConstraintType::Primary,
            AstIndexConstraintType::Foreign => IndexConstraintType::Foreign,
            AstIndexConstraintType::None => IndexConstraintType::None,
        };
        let references: Option<Vec<usize>> = self
            .expressions
            .iter()
            .map(|expression| match &expression.kind {
                ExpressionKind::Reference(reference) => usize::try_from(reference.index).ok(),
                _ => None,
            })
            .collect();
        if let Some(columns) = references {
            return storage.new_art(&index.name, constraint, &columns);
        }
        let columns: Vec<usize> = index
            .column_ids
            .iter()
            .filter_map(|&c| usize::try_from(c).ok())
            .collect();
        let key_types = self.expressions.iter().map(|e| e.return_type.clone()).collect();
        let executor = IndexExpressionExecutor {
            executor: std::sync::Mutex::new(ExpressionExecutor::new(self.expressions.clone())),
            names: self.expressions.iter().map(Expression::name).collect(),
        };
        storage.new_art_with_expressions(
            &index.name,
            constraint,
            &columns,
            key_types,
            IndexKeyExpressions::Custom(Arc::new(executor)),
        )
    }
}

impl DdlAction for CreateIndexAction {
    fn execute(&self, transaction: &Transaction) -> Result<()> {
        let CreateInfoKind::Index(index) = &self.info.kind else {
            return Err(Error::internal("expected an index"));
        };
        let schema = &self.info.schema;
        if self
            .catalog
            .get_entry(transaction, CatalogType::Index, schema, &index.name)
            .is_some()
        {
            if self.info.on_conflict == OnCreateConflict::IgnoreOnConflict {
                return Ok(());
            }
            return Err(Error::catalog(format!(
                "Index with name \"{}\" already exists!",
                index.name
            )));
        }
        if self
            .expressions
            .iter()
            .any(|expression| !expression.is_consistent())
        {
            return Err(Error::binder(
                "Index keys cannot contain expressions with side effects.",
            ));
        }
        if !index.index_type_name.is_empty() && !index.index_type_name.eq_ignore_ascii_case("ART") {
            return Err(Error::binder(format!(
                "Unknown index type: {}",
                index.index_type_name
            )));
        }
        let EntryKind::Table(table) = &self.table.kind else {
            return Err(Error::binder("Indexes can only be created on DuckDB tables."));
        };
        let storage = table
            .storage
            .clone()
            .ok_or_else(|| Error::binder("Indexes can only be created on DuckDB tables."))?;
        let art = self.new_art(&storage, index)?;
        storage.create_index(transaction, art)?;
        let mut entry = CatalogEntry::new(
            index.name.clone(),
            EntryKind::Index(IndexEntry {
                info: (**index).clone(),
                schema: schema.clone(),
                storage: Some(Arc::clone(storage.info())),
            }),
        );
        entry.temporary = self.info.temporary;
        if let Err(error) = self
            .catalog
            .create_entry(transaction, schema, entry, self.info.on_conflict)
        {
            storage.remove_index(&index.name);
            return Err(error);
        }
        Ok(())
    }
}
