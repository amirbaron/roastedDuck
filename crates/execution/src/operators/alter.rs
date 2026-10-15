//! `ALTER TABLE` / `ALTER VIEW` (DuckDB's `PhysicalAlter`, `Catalog::Alter` and the
//! storage side of `DuckTableEntry::AlterEntry`). The binder checked the statement and
//! computed the new definition; this changes storage and replaces the catalog entry.

use std::sync::Arc;

use rustdb_catalog::{Catalog, CatalogEntry, EntryKind, StorageHook, TableEntry};
use rustdb_common::{Error, ErrorKind, Result};
use rustdb_planner::Expression;
use rustdb_planner::operator::AlterAction;
use rustdb_storage::index::IndexConstraintType;
use rustdb_storage::index::art::IndexKeyExpressions;
use rustdb_storage::{ColumnDefinition, DataTable, StorageIndex, Transaction};
use rustdb_types::{DataChunk, Vector};

use super::ddl::DdlAction;
use crate::expression_executor::ExpressionExecutor;

/// Runs a bound `ALTER`.
#[derive(Debug)]
pub struct AlterEntryAction {
    pub catalog: Arc<Catalog>,
    pub schema: String,
    pub entry: Option<Arc<CatalogEntry>>,
    pub action: AlterAction,
    pub dependents_allowed: bool,
    /// The statement the WAL logs.
    pub info: Option<Box<rustdb_ast::AlterInfo>>,
}

/// Evaluates `expression` over `input` into a single vector.
fn evaluate(expression: &Expression, input: &DataChunk) -> Result<Vector> {
    let mut executor = ExpressionExecutor::new(vec![expression.clone()]);
    let mut output = DataChunk::default();
    executor.execute(input, &mut output)?;
    output
        .into_columns()
        .into_iter()
        .next()
        .ok_or_else(|| Error::internal("expression produced no column"))
}

impl AlterEntryAction {
    /// `DependencyManager::AlterObject`: a table with indexes can only get new columns
    /// and defaults.
    fn check_dependents(&self, transaction: &Transaction, entry: &CatalogEntry) -> Result<()> {
        if self.dependents_allowed {
            return Ok(());
        }
        let has_indexes = matches!(entry.kind, EntryKind::Table(_))
            && !self
                .catalog
                .table_indexes(transaction, &self.schema, entry.name())
                .is_empty();
        if !has_indexes
            && self
                .catalog
                .dependents(transaction, &self.schema, entry)
                .is_empty()
        {
            return Ok(());
        }
        Err(Error::new(
            ErrorKind::Dependency,
            format!(
                "Cannot alter entry \"{}\" because there are entries that depend on it.",
                entry.name()
            ),
        ))
    }

    /// Replaces the table entry by `definition` stored in `storage`.
    fn replace(
        &self,
        transaction: &Transaction,
        entry: &CatalogEntry,
        mut definition: TableEntry,
        storage: Option<Arc<DataTable>>,
        hook: StorageHook,
    ) -> Result<()> {
        definition.storage = storage;
        let dependencies = rustdb_catalog::dependencies::altered_table_dependencies(
            &definition,
            &entry.dependencies,
            "main",
        );
        let mut new_entry = CatalogEntry::new(entry.name(), EntryKind::Table(definition));
        new_entry.temporary = entry.temporary;
        new_entry.dependencies = dependencies;
        self.catalog.alter_entry(
            transaction,
            &self.schema,
            entry.catalog_type(),
            entry.name(),
            new_entry,
            hook,
            self.info.as_deref().cloned(),
        )
    }

    /// Runs a storage change that creates a new version of `storage`; if the catalog
    /// refuses the alter, the old version becomes the table again.
    fn replace_storage(
        &self,
        transaction: &Transaction,
        entry: &CatalogEntry,
        definition: &TableEntry,
        storage: &Arc<DataTable>,
        change: impl FnOnce() -> Result<Arc<DataTable>>,
    ) -> Result<()> {
        let new_storage = change()?;
        let result = self.replace(
            transaction,
            entry,
            definition.clone(),
            Some(new_storage),
            StorageHook::AlteredTable(Arc::clone(storage)),
        );
        if result.is_err() {
            storage.set_as_main_table();
        }
        result
    }
}

impl DdlAction for AlterEntryAction {
    fn execute(&self, transaction: &Transaction) -> Result<()> {
        let Some(entry) = &self.entry else {
            return Ok(());
        };
        let storage = match &entry.kind {
            EntryKind::Table(table) => table.storage.clone(),
            _ => None,
        };
        match &self.action {
            AlterAction::Nothing => Ok(()),
            AlterAction::Rename(new_name) => {
                self.check_dependents(transaction, entry)?;
                let kind = match &entry.kind {
                    EntryKind::Table(table) => EntryKind::Table(table.clone()),
                    EntryKind::View(view) => EntryKind::View(view.clone()),
                    _ => return Err(Error::internal("rename of an unexpected entry")),
                };
                let mut renamed = CatalogEntry::new(new_name.clone(), kind);
                renamed.temporary = entry.temporary;
                renamed.dependencies.clone_from(&entry.dependencies);
                let hook = match &storage {
                    Some(storage) => {
                        StorageHook::RenamedTable(Arc::clone(storage.info()), entry.name().to_owned())
                    }
                    None => StorageHook::None,
                };
                self.catalog.rename_entry(
                    transaction,
                    &self.schema,
                    entry.catalog_type(),
                    entry.name(),
                    renamed,
                    hook,
                    self.info.as_deref().cloned(),
                )?;
                if let Some(storage) = &storage {
                    storage.info().set_table_name(new_name.clone());
                }
                Ok(())
            }
            AlterAction::Definition(definition) => {
                self.check_dependents(transaction, entry)?;
                self.replace(transaction, entry, definition.clone(), storage, StorageHook::None)
            }
            AlterAction::AddColumn {
                definition,
                name,
                logical_type,
                default,
            } => {
                let storage = storage.ok_or_else(|| Error::internal("ALTER of a table without storage"))?;
                self.check_dependents(transaction, entry)?;
                let column = ColumnDefinition::new(name.clone(), logical_type.clone());
                let fill = |count: usize| evaluate(default, &DataChunk::new(Vec::new(), count));
                self.replace_storage(transaction, entry, definition, &storage, || {
                    storage.add_column(transaction, &column, &fill)
                })
            }
            AlterAction::RemoveColumn { definition, column } => {
                let storage = storage.ok_or_else(|| Error::internal("ALTER of a table without storage"))?;
                storage.verify_remove_column(*column)?;
                self.check_dependents(transaction, entry)?;
                self.replace_storage(transaction, entry, definition, &storage, || {
                    storage.remove_column(transaction, *column)
                })
            }
            AlterAction::AlterType {
                definition,
                column,
                target,
                expression,
                bound_columns,
            } => {
                let storage = storage.ok_or_else(|| Error::internal("ALTER of a table without storage"))?;
                storage.verify_alter_type(*column)?;
                self.check_dependents(transaction, entry)?;
                let mut scanned: Vec<StorageIndex> = bound_columns
                    .iter()
                    .map(|&c| StorageIndex::new(c as u64))
                    .collect();
                if scanned.is_empty() {
                    scanned.push(StorageIndex::row_id());
                }
                let cast = |chunk: &DataChunk| evaluate(expression, chunk);
                self.replace_storage(transaction, entry, definition, &storage, || {
                    storage.alter_type(transaction, *column, target, &scanned, &cast)
                })
            }
            AlterAction::SetNotNull { definition, column } => {
                let storage = storage.ok_or_else(|| Error::internal("ALTER of a table without storage"))?;
                self.check_dependents(transaction, entry)?;
                self.replace_storage(transaction, entry, definition, &storage, || {
                    let names: Vec<String> = definition
                        .physical_columns()
                        .map(|(_, _, column)| column.name.clone())
                        .collect();
                    storage.add_not_null_constraint_named(transaction, *column, &names)
                })
            }
            AlterAction::AddPrimaryKey {
                definition,
                keys,
                index_name,
                existing,
            } => {
                let storage = storage.ok_or_else(|| Error::internal("ALTER of a table without storage"))?;
                // The keys are named after the columns' current names.
                let names: Vec<String> = definition
                    .physical_columns()
                    .map(|(_, _, column)| column.name.clone())
                    .collect();
                let key_names = keys.iter().filter_map(|&key| names.get(key).cloned()).collect();
                let key_types = keys
                    .iter()
                    .filter_map(|&key| storage.columns().get(key).map(|column| column.ty.clone()))
                    .collect();
                let mut art = storage.new_art_with_expressions(
                    index_name,
                    IndexConstraintType::Primary,
                    keys,
                    key_types,
                    IndexKeyExpressions::Columns(key_names),
                )?;
                storage.build_index(transaction, &mut art, true)?;
                if storage.indexes().find(index_name).is_some() {
                    return Err(Error::catalog(format!(
                        "an index with that name already exists for this table: {index_name}"
                    )));
                }
                if let Some(existing) = existing {
                    return Err(Error::catalog(format!(
                        "table \"{}\" can have only one primary key: {existing}",
                        entry.name()
                    )));
                }
                self.check_dependents(transaction, entry)?;
                storage.add_index(art)?;
                let hook = StorageHook::CreatedIndex(Arc::clone(storage.info()), index_name.clone());
                let result = self.replace(
                    transaction,
                    entry,
                    definition.clone(),
                    Some(Arc::clone(&storage)),
                    hook,
                );
                if result.is_err() {
                    storage.remove_index(index_name);
                }
                result
            }
        }
    }
}
