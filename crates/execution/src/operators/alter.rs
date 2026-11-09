//! `ALTER TABLE` / `ALTER VIEW` (DuckDB's `PhysicalAlter`, `Catalog::Alter` and the
//! storage side of `DuckTableEntry::AlterEntry`). The binder checked the statement and
//! computed the new definition; this changes storage and replaces the catalog entry.

use std::sync::Arc;

use roastedduck_catalog::{Catalog, CatalogEntry, EntryKind, StorageHook, TableEntry};
use roastedduck_common::{Error, ErrorKind, Result};
use roastedduck_planner::Expression;
use roastedduck_planner::operator::AlterAction;
use roastedduck_storage::index::IndexConstraintType;
use roastedduck_storage::index::art::IndexKeyExpressions;
use roastedduck_storage::{ColumnDefinition, DataTable, StorageIndex, Transaction};
use roastedduck_types::{DataChunk, Vector};

use super::ddl::DdlAction;
use crate::expression_executor::ExpressionExecutor;

/// Runs a bound `ALTER`.
#[derive(Debug)]
pub struct AlterEntryAction {
    pub catalog: Arc<Catalog>,
    pub schema: roastedduck_common::ByteString,
    pub entry: Option<Arc<CatalogEntry>>,
    pub action: AlterAction,
    pub dependents_allowed: bool,
    /// The statement the WAL logs.
    pub info: Option<Box<roastedduck_ast::AlterInfo>>,
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
    /// Restores an ADD PRIMARY KEY WAL tree and changes the table definition.
    /// The caller publishes the returned index only after replay commit.
    pub fn execute_replayed_primary_key(
        &self,
        transaction: &Transaction,
        stored: &roastedduck_storage::index::art::ArtStorageInfo,
    ) -> Result<(
        Arc<roastedduck_storage::DataTableInfo>,
        roastedduck_storage::index::IndexSlot,
    )> {
        let AlterAction::AddPrimaryKey {
            definition,
            keys,
            index_name,
            ..
        } = &self.action
        else {
            return Err(Error::internal("primary-key WAL replay requires ADD PRIMARY KEY"));
        };
        let storage = self
            .entry
            .as_ref()
            .and_then(|entry| match &entry.kind {
                EntryKind::Table(table) => table.storage.as_ref(),
                _ => None,
            })
            .ok_or_else(|| Error::internal("primary-key WAL replay without table storage"))?;
        let mut art = primary_key_art(storage, definition, keys, index_name)?;
        if !storage.restore_index(&mut art, stored)? {
            return Err(Error::serialization(
                "primary-key WAL entry without native allocator metadata",
            ));
        }
        self.execute_internal(transaction, true)?;
        Ok((
            Arc::clone(storage.info()),
            roastedduck_storage::index::IndexSlot::from_bound(art),
        ))
    }

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
        Err(Error::new_bytes(
            ErrorKind::Dependency,
            roastedduck_common::byte_format!(
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
        let dependencies = roastedduck_catalog::dependencies::altered_table_dependencies(
            &definition,
            &entry.dependencies,
            "main",
        );
        let mut new_entry = self.catalog.new_entry(entry.name(), EntryKind::Table(definition));
        new_entry.inherit_links(entry);
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
        self.execute_internal(transaction, false)
    }
}

fn primary_key_art(
    storage: &Arc<DataTable>,
    definition: &TableEntry,
    keys: &[usize],
    index_name: &roastedduck_common::ByteString,
) -> Result<roastedduck_storage::index::Art> {
    let names: Vec<_> = definition
        .physical_columns()
        .map(|(_, _, column)| column.name.clone())
        .collect();
    let key_names = keys.iter().filter_map(|&key| names.get(key).cloned()).collect();
    let key_types = keys
        .iter()
        .filter_map(|&key| storage.columns().get(key).map(|column| column.ty.clone()))
        .collect();
    storage.new_art_with_expressions(
        index_name,
        IndexConstraintType::Primary,
        keys,
        key_types,
        IndexKeyExpressions::Columns(key_names),
    )
}

impl AlterEntryAction {
    fn execute_internal(&self, transaction: &Transaction, replayed_primary: bool) -> Result<()> {
        let Some(entry) = &self.entry else {
            return Ok(());
        };
        let storage = match &entry.kind {
            EntryKind::Table(table) => table.storage.clone(),
            _ => None,
        };
        match &self.action {
            AlterAction::Nothing => Ok(()),
            AlterAction::SetComment(comment) => self.catalog.set_comment(
                transaction,
                &self.schema,
                entry,
                comment.clone(),
                self.info.as_deref().cloned(),
            ),
            AlterAction::SetColumnComment { column, comment } => self.catalog.set_column_comment(
                transaction,
                &self.schema,
                entry,
                (column, comment.clone()),
                self.info.as_deref().cloned(),
            ),
            AlterAction::ChangeOwnership {
                owner_schema,
                owner_name,
            } => self.catalog.change_ownership(
                transaction,
                &self.schema,
                entry,
                (owner_schema, owner_name),
                self.info.as_deref().cloned(),
            ),
            AlterAction::Rename(new_name) => {
                // `CatalogSet::AlterEntry` finds a name conflict before the dependency
                // manager checks the dependents.
                if !new_name.eq_ignore_ascii_case(entry.name())
                    && self
                        .catalog
                        .get_entry(transaction, entry.catalog_type(), &self.schema, new_name)
                        .is_some()
                {
                    return Err(Error::catalog(roastedduck_common::byte_format!(
                        "Could not rename \"{}\" to \"{new_name}\": another entry with this name already exists!",
                        entry.name()
                    )));
                }
                self.check_dependents(transaction, entry)?;
                let kind = match &entry.kind {
                    EntryKind::Table(table) => EntryKind::Table(table.clone()),
                    EntryKind::View(view) => EntryKind::View(view.clone()),
                    _ => return Err(Error::internal("rename of an unexpected entry")),
                };
                let mut renamed = self.catalog.new_entry(new_name.clone(), kind);
                renamed.inherit_links(entry);
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
                super::ddl::check_type_is_supported(logical_type, &self.catalog)?;
                super::ddl::check_struct_columns(logical_type)?;
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
                super::ddl::check_type_is_supported(target, &self.catalog)?;
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
                    let names: Vec<roastedduck_common::ByteString> = definition
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
                let art = if replayed_primary {
                    None
                } else {
                    let mut art = primary_key_art(&storage, definition, keys, index_name)?;
                    storage.build_index(transaction, &mut art, true)?;
                    Some(art)
                };
                storage.check_main_table_for_index_creation()?;
                if storage.indexes().contains(index_name) {
                    return Err(Error::catalog(roastedduck_common::byte_format!(
                        "an index with that name already exists for this table: {index_name}"
                    )));
                }
                if let Some(existing) = existing {
                    return Err(Error::catalog(roastedduck_common::byte_format!(
                        "table \"{}\" can have only one primary key: {existing}",
                        entry.name()
                    )));
                }
                self.check_dependents(transaction, entry)?;
                // `DuckTableEntry::AddConstraint`: a new version of the storage, sharing
                // the indexes, to which the key's index is added.
                let new_storage = storage.add_unique_constraint(transaction);
                if let Some(art) = art {
                    new_storage.add_index(art)?;
                }
                let hook = StorageHook::AddedPrimaryKey(Arc::clone(&storage), index_name.clone());
                let result = self.replace(transaction, entry, definition.clone(), Some(new_storage), hook);
                if result.is_err() {
                    storage.set_as_main_table();
                    storage.remove_index(index_name);
                }
                result
            }
        }
    }
}
