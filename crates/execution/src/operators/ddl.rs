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
use rustdb_planner::{ClientContext, Expression, ExpressionKind};
use rustdb_storage::TableFilter;
use rustdb_storage::index::IndexConstraintType;
use rustdb_storage::index::art::{Art, IndexKeyExecutor, IndexKeyExpressions, IndexScanPredicate};
use rustdb_storage::storage_info::storage_version_name;
use rustdb_storage::{ColumnDefinition, DataTable, TableIoManager, Transaction};
use rustdb_types::compare::CompareOp;
use rustdb_types::{DataChunk, LogicalType, LogicalTypeId};

use super::ExecutableExpressionFilter;
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
        let mut entry = self.catalog.new_entry(
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
        let mut entry = self.catalog.new_entry(
            view.view_name.clone(),
            EntryKind::View(ViewEntry {
                query: (*view.query).clone(),
                aliases: view.aliases.clone(),
                types: view.types.clone(),
                names: view.names.clone(),
                column_comments: view.column_comments.clone(),
            }),
        );
        entry.dependencies.clone_from(&self.info.dependencies.set);
        entry.temporary = self.info.temporary;
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
        let entry = rustdb_catalog::checkpoint::entry_from_create_info(&self.catalog, &self.info)?;
        self.catalog
            .create_entry(transaction, &self.info.schema, entry, self.info.on_conflict)?;
        Ok(())
    }
}

/// The types nested in `ty`, `ty` first (DuckDB's `TypeVisitor`).
fn visit_types(ty: &LogicalType, visit: &mut dyn FnMut(&LogicalType) -> Result<()>) -> Result<()> {
    visit(ty)?;
    if let Some(child) = ty.child_type() {
        visit_types(child, visit)?;
    }
    if let Some(children) = ty.struct_children() {
        for (_, child) in children {
            visit_types(child, visit)?;
        }
    }
    Ok(())
}

/// The storage version that added VARIANT to the core (`Variant::VERSION_ADDED`).
const VARIANT_VERSION_ADDED: u64 = 7;

/// `CheckTypeIsSupported` (`DuckTableEntry`): a table cannot hold TYPE values, nor
/// VARIANT values in a database file written with a storage version before v1.5.0
/// (in-memory databases use the latest version).
pub(crate) fn check_type_is_supported(ty: &LogicalType, catalog: &Catalog) -> Result<()> {
    visit_types(ty, &mut |ty| {
        match ty.id() {
            LogicalTypeId::Type => {
                return Err(Error::invalid_input(
                    "A table cannot be created with a 'TYPE' column",
                ));
            }
            LogicalTypeId::Variant => {
                if let Some(version) = catalog.storage_version()
                    && version < VARIANT_VERSION_ADDED
                {
                    return Err(Error::invalid_input(rustdb_common::byte_format!(
                        "VARIANT columns are not supported in storage versions prior to {} (database \"{}\" \
                         is using storage version {})",
                        storage_version_name(VARIANT_VERSION_ADDED, false),
                        catalog.name(),
                        storage_version_name(version, false)
                    )));
                }
            }
            _ => {}
        }
        Ok(())
    })
}

/// The check of the `StructColumnData` constructor: stored structs (but not UNIONs or
/// VARIANTs, which have their own column data) need field names.
pub(crate) fn check_struct_columns(ty: &LogicalType) -> Result<()> {
    fn check(ty: &LogicalType) -> Result<()> {
        if ty.id() == LogicalTypeId::Variant {
            return Ok(());
        }
        if ty.id() != LogicalTypeId::Union && ty.is_unnamed_struct() {
            return Err(Error::invalid_input(
                "A table cannot be created from an unnamed struct",
            ));
        }
        if let Some(child) = ty.child_type() {
            check(child)?;
        }
        if let Some(children) = ty.struct_children() {
            for (_, child) in children {
                check(child)?;
            }
        }
        Ok(())
    }
    check(ty)
}

/// Creates a table's storage and catalog entry (DuckDB's `DuckSchemaEntry::CreateTable`
/// with the `DuckTableEntry` constructor), for `CREATE TABLE [AS]`.
#[derive(Debug)]
pub struct CatalogTableCreator {
    pub catalog: Arc<Catalog>,
    pub schema: rustdb_common::ByteString,
    pub info: CreateInfo,
    pub io: Arc<TableIoManager>,
}

impl TableCreator for CatalogTableCreator {
    fn create(&self, transaction: &Transaction) -> Result<Option<Arc<DataTable>>> {
        let CreateInfoKind::Table(table) = &self.info.kind else {
            return Err(Error::internal("expected a table"));
        };
        // `Catalog::SupportsCreateTable`.
        let unsupported = if !table.partition_keys.is_empty() {
            Some("PARTITIONED BY")
        } else if !table.sort_keys.is_empty() {
            Some("SORTED BY")
        } else if !table.options.is_empty() {
            Some("WITH clause")
        } else {
            None
        };
        if let Some(clause) = unsupported {
            return Err(Error::catalog(rustdb_common::byte_format!(
                "{clause} is not supported for tables in a duckdb catalog"
            )));
        }
        let mut entry = TableEntry::from_create_info(table);
        let physical = || {
            table
                .columns
                .columns
                .iter()
                .filter(|c| c.category != TableColumnType::Generated)
        };
        for column in physical() {
            check_type_is_supported(&column.logical_type, &self.catalog)?;
        }
        for column in physical() {
            check_struct_columns(&column.logical_type)?;
        }
        let columns: Vec<ColumnDefinition> = physical()
            .map(|c| {
                ColumnDefinition::new(c.name.clone(), c.logical_type.clone()).with_compression(
                    rustdb_storage::compression::CompressionType::from_u8(c.compression_type as u8)
                        .unwrap_or(rustdb_storage::compression::CompressionType::Auto),
                )
            })
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
        let mut catalog_entry = self.catalog.new_entry(name.clone(), EntryKind::Table(entry));
        catalog_entry.dependencies.clone_from(&self.info.dependencies.set);
        catalog_entry.temporary = self.info.temporary;
        let created =
            self.catalog
                .create_entry(transaction, &self.schema, catalog_entry, self.info.on_conflict)?;
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
    schema: &[u8],
    name: &[u8],
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
    let mut entry = catalog.new_entry(existing.name(), EntryKind::Table(table));
    entry.inherit_links(&existing);
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
                return Err(Error::not_implemented(rustdb_common::byte_format!(
                    "DROP {}",
                    other.name()
                )));
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
                    return Err(Error::catalog(rustdb_common::byte_format!(
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
    names: Vec<rustdb_common::ByteString>,
    /// The single key expression with its column read as `BOUND_REF #0`, the form of
    /// a pushed-down expression filter; `None` for compound keys or keys over several
    /// columns.
    scan_key: Option<Expression>,
}

impl IndexExpressionExecutor {
    fn new(expressions: &[Expression]) -> Self {
        Self {
            executor: std::sync::Mutex::new(ExpressionExecutor::new(expressions.to_vec())),
            names: expressions.iter().map(Expression::name).collect(),
            scan_key: match expressions {
                [expression] => single_column_key(expression),
                _ => None,
            },
        }
    }
}

/// `expression` with its column references renumbered to `#0`, if it reads exactly one
/// column (`TryScanIndex` rebinds the index expression to the scanned column).
fn single_column_key(expression: &Expression) -> Option<Expression> {
    fn collect(expression: &Expression, columns: &mut Vec<u64>) {
        if let ExpressionKind::Reference(reference) = &expression.kind
            && !columns.contains(&reference.index)
        {
            columns.push(reference.index);
        }
        for child in expression.children() {
            collect(child, columns);
        }
    }
    fn renumber(expression: &mut Expression) {
        if let ExpressionKind::Reference(reference) = &mut expression.kind {
            reference.index = 0;
        }
        expression.for_each_child_mut(renumber);
    }
    let mut columns = Vec::new();
    collect(expression, &mut columns);
    if columns.len() != 1 {
        return None;
    }
    let mut key = expression.clone();
    renumber(&mut key);
    Some(key)
}

/// The comparison of an index scan for a comparison expression type.
fn scan_compare_op(expression_type: rustdb_planner::ExpressionType) -> Option<CompareOp> {
    use rustdb_planner::ExpressionType as T;
    Some(match expression_type {
        T::CompareEqual => CompareOp::Equal,
        T::CompareLessthan => CompareOp::LessThan,
        T::CompareLessthanorequalto => CompareOp::LessThanOrEqual,
        T::CompareGreaterthan => CompareOp::GreaterThan,
        T::CompareGreaterthanorequalto => CompareOp::GreaterThanOrEqual,
        _ => return None,
    })
}

/// `FlipComparisonExpression`: the comparison with its sides swapped.
fn flip_compare_op(op: CompareOp) -> CompareOp {
    match op {
        CompareOp::LessThan => CompareOp::GreaterThan,
        CompareOp::LessThanOrEqual => CompareOp::GreaterThanOrEqual,
        CompareOp::GreaterThan => CompareOp::LessThan,
        CompareOp::GreaterThanOrEqual => CompareOp::LessThanOrEqual,
        other => other,
    }
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

    fn names(&self) -> &[rustdb_common::ByteString] {
        &self.names
    }

    /// `ART::TryInitializeScan` with the filter's `ToExpression`: a comparison of the key
    /// expression with a constant.
    fn scan_predicates(&self, filter: &TableFilter) -> Option<Vec<IndexScanPredicate>> {
        let key = self.scan_key.as_ref()?;
        let TableFilter::Expression(filter) = filter else {
            return None;
        };
        let expression = filter
            .as_any()
            .downcast_ref::<ExecutableExpressionFilter>()
            .map(ExecutableExpressionFilter::expression)?;
        let ExpressionKind::Comparison(comparison) = &expression.kind else {
            return None;
        };
        let op = scan_compare_op(expression.expression_type)?;
        let (op, constant) = match (&comparison.left.kind, &comparison.right.kind) {
            (_, ExpressionKind::Constant(constant)) if comparison.left.equals(key) => (op, constant),
            (ExpressionKind::Constant(constant), _) if comparison.right.equals(key) => {
                (flip_compare_op(op), constant)
            }
            _ => return None,
        };
        if constant.value.is_null() {
            return None;
        }
        Some(vec![IndexScanPredicate::Single {
            op,
            value: constant.value.clone(),
        }])
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

/// An empty ART of index `index` over `storage` whose keys are `expressions` over the
/// table's physical columns (`IndexType::create_instance` of the ART).
fn index_art(storage: &DataTable, index: &CreateIndexInfo, expressions: &[Expression]) -> Result<Art> {
    let constraint = match index.constraint_type {
        AstIndexConstraintType::Unique => IndexConstraintType::Unique,
        AstIndexConstraintType::Primary => IndexConstraintType::Primary,
        AstIndexConstraintType::Foreign => IndexConstraintType::Foreign,
        AstIndexConstraintType::None => IndexConstraintType::None,
    };
    let references: Option<Vec<usize>> = expressions
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
    let key_types = expressions.iter().map(|e| e.return_type.clone()).collect();
    let executor = IndexExpressionExecutor::new(expressions);
    storage.new_art_with_expressions(
        &index.name,
        constraint,
        &columns,
        key_types,
        IndexKeyExpressions::Custom(Arc::new(executor)),
    )
}

/// Binds the indexes of table `entry` that were read from storage and not bound yet,
/// building each from the table's rows (DuckDB's `DataTableInfo::BindIndexes` with
/// `IndexBinder::BindIndex`). Called before a statement changes the table.
pub fn bind_table_indexes(client: &Arc<ClientContext>, entry: &Arc<CatalogEntry>) -> Result<()> {
    let EntryKind::Table(table) = &entry.kind else {
        return Ok(());
    };
    let Some(storage) = &table.storage else {
        return Ok(());
    };
    if !storage.indexes().has_unbound() {
        return Ok(());
    }
    storage.bind_indexes(&mut |unbound| {
        let Some(info) = unbound.create_info().downcast_ref::<CreateInfo>() else {
            return Err(Error::internal("unbound index without a CreateInfo"));
        };
        let CreateInfoKind::Index(index) = &info.kind else {
            return Err(Error::internal("unbound index without a CreateIndexInfo"));
        };
        if !unbound.index_type().eq_ignore_ascii_case("ART") {
            return Err(Error::new_bytes(
                rustdb_common::ErrorKind::MissingExtension,
                rustdb_common::byte_format!(
                    "Cannot bind index '{}', unknown index type '{}'. You need to load the extension that provides this index type before table '{}' can be modified.",
                    index.table,
                    unbound.index_type(),
                    index.table
                ),
            ));
        }
        let expressions = rustdb_planner::bind_index_expressions(Arc::clone(client), entry, index)?;
        let mut art = index_art(storage, index, &expressions)?;
        // The index holds the latest committed rows, like the stored one plus the
        // replayed changes (`ApplyBufferedReplays`).
        if !storage.restore_unbound_index(&mut art, unbound)? {
            storage.build_index(&Transaction::system(), &mut art, false)?;
        }
        Ok(art)
    })
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
            return Err(Error::catalog(rustdb_common::byte_format!(
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
            return Err(Error::binder(rustdb_common::byte_format!(
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
        let art = index_art(&storage, index, &self.expressions)?;
        storage.create_index(
            transaction,
            art,
            self.info.on_conflict != OnCreateConflict::IgnoreOnConflict,
        )?;
        let mut entry = self.catalog.new_entry(
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
