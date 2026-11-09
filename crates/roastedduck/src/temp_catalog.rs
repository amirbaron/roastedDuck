//! The connection's temporary catalog as functions see it during a statement (the
//! `ClientContext` catalog calls of DuckDB functions that keep their own tables, such as
//! `CSVRejectsTable`).

use std::sync::Arc;

use roastedduck_ast::{
    ColumnDefinition, ColumnList, CompressionType, CreateInfo, CreateInfoKind, CreateTableInfo,
    LogicalDependencyList, OnCreateConflict, TableColumnType,
};
use roastedduck_catalog::{Catalog, CatalogType, DEFAULT_SCHEMA, EntryKind, Transaction, TypeEntry};
use roastedduck_common::{CaseInsensitiveMap, Error, OrderedMap, Result, STANDARD_VECTOR_SIZE};
use roastedduck_execution::operators::{CatalogTableCreator, TableCreator};
use roastedduck_function::cast::ValueCast;
use roastedduck_types::{DataChunk, LogicalType, Value, Vector};

/// The `temp` catalog in the transaction of the running statement.
#[derive(Debug)]
pub(crate) struct StatementTempCatalog {
    pub(crate) catalog: Arc<Catalog>,
    pub(crate) transaction: Arc<Transaction>,
}

impl roastedduck_function::TempCatalog for StatementTempCatalog {
    fn table_exists(&self, name: &str) -> Result<bool> {
        Ok(self
            .catalog
            .get_entry(&self.transaction, CatalogType::Table, DEFAULT_SCHEMA, name)
            .is_some_and(|entry| matches!(entry.kind, EntryKind::Table(_))))
    }

    fn create_type(&self, name: &str, ty: LogicalType) -> Result<()> {
        let mut entry = self
            .catalog
            .new_entry(name, EntryKind::Type(TypeEntry { user_type: ty }));
        entry.temporary = true;
        self.catalog.create_entry(
            &self.transaction,
            DEFAULT_SCHEMA,
            entry,
            OnCreateConflict::IgnoreOnConflict,
        )?;
        Ok(())
    }

    fn create_table(&self, name: &str, columns: &[(String, LogicalType)]) -> Result<()> {
        let columns = columns
            .iter()
            .map(|(name, ty)| ColumnDefinition {
                name: name.clone().into(),
                logical_type: ty.clone(),
                expression: None,
                category: TableColumnType::Standard,
                compression_type: CompressionType::CompressionAuto,
                comment: Value::default(),
                tags: OrderedMap::default(),
            })
            .collect();
        let info = CreateInfo {
            catalog: (&*self.catalog.name()).into(),
            schema: DEFAULT_SCHEMA.to_owned().into(),
            temporary: true,
            internal: false,
            on_conflict: OnCreateConflict::IgnoreOnConflict,
            sql: String::new().into(),
            comment: Value::default(),
            tags: CaseInsensitiveMap::default(),
            dependencies: LogicalDependencyList::default(),
            kind: CreateInfoKind::Table(Box::new(CreateTableInfo {
                table: name.to_owned().into(),
                columns: ColumnList { columns },
                constraints: Vec::new(),
                query: None,
                partition_keys: Vec::new(),
                sort_keys: Vec::new(),
                options: CaseInsensitiveMap::default(),
            })),
        };
        let creator = CatalogTableCreator {
            catalog: Arc::clone(&self.catalog),
            schema: DEFAULT_SCHEMA.to_owned().into(),
            info,
            io: Arc::clone(self.catalog.io()?),
        };
        creator.create(&self.transaction)?;
        Ok(())
    }

    fn append(&self, name: &str, rows: &[Vec<Value>]) -> Result<()> {
        let entry = self
            .catalog
            .get_entry(&self.transaction, CatalogType::Table, DEFAULT_SCHEMA, name)
            .ok_or_else(|| Error::catalog(format!("Table with name {name} does not exist!")))?;
        let EntryKind::Table(table) = &entry.kind else {
            return Err(Error::internal(format!("{name} is not a table")));
        };
        let storage = table
            .storage
            .as_ref()
            .ok_or_else(|| Error::internal(format!("table {name} without storage")))?;
        let types: Vec<LogicalType> = table
            .physical_columns()
            .map(|(_, _, column)| column.logical_type.clone())
            .collect();
        for rows in rows.chunks(STANDARD_VECTOR_SIZE) {
            let mut columns: Vec<Vector> = types
                .iter()
                .map(|ty| Vector::flat(ty.clone(), rows.len()))
                .collect();
            for (row, values) in rows.iter().enumerate() {
                for ((column, value), ty) in columns.iter_mut().zip(values).zip(&types) {
                    if value.logical_type() == ty || value.is_null() {
                        column.set_value(row, value)?;
                    } else {
                        column.set_value(row, &value.default_cast_as(ty)?)?;
                    }
                }
            }
            let chunk = DataChunk::new(columns, rows.len());
            storage.append_chunk(
                &self.transaction,
                &chunk,
                roastedduck_storage::TableConstraints::default(),
            )?;
        }
        Ok(())
    }
}
