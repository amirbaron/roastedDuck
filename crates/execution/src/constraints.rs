//! Turns a table's bound constraints into what its storage enforces: NOT NULL columns,
//! CHECK constraints evaluated by the expression executor, and foreign keys resolved to
//! the storage of the other table (DuckDB's `VerifyCheckConstraint` and the foreign key
//! lookups of `DataTable::VerifyAppendConstraints` / `VerifyDeleteConstraints`).

use std::sync::Arc;

use rustdb_ast::ForeignKeyType;
use rustdb_catalog::{Catalog, CatalogEntry, CatalogType, EntryKind, Transaction};
use rustdb_common::{Error, ErrorKind, Result};
use rustdb_planner::Expression;
use rustdb_planner::operator::BoundConstraint;
use rustdb_storage::{BoundForeignKey, ConstraintVerifier, DataTable, ForeignKeyKind, TableConstraints};
use rustdb_types::{DataChunk, LogicalType, Vector};

use crate::expression_executor::ExpressionExecutor;

/// One `CHECK` constraint over the physical columns of its table.
#[derive(Debug)]
struct CheckConstraint {
    expression: Expression,
    /// The physical columns the expression reads.
    bound_columns: Vec<usize>,
    /// The constraint as written (`CHECK((i > 0))`), for the error message.
    text: rustdb_common::ByteString,
}

/// Evaluates the generated column expressions and `CHECK` constraints of a table.
#[derive(Debug)]
struct CheckVerifier {
    table: rustdb_common::ByteString,
    /// The types of the table's physical columns.
    types: Vec<LogicalType>,
    checks: Vec<CheckConstraint>,
    /// Generated column expressions with their `name TYPE AS (expression)` text.
    generated: Vec<(Expression, rustdb_common::ByteString)>,
}

impl CheckVerifier {
    /// `VerifyCheckConstraint`: every row whose expression is false (not NULL) fails.
    fn verify(&self, check: &CheckConstraint, chunk: &DataChunk) -> Result<()> {
        let mut executor = ExpressionExecutor::new(vec![check.expression.clone()]);
        let mut result = DataChunk::default();
        executor.execute(chunk, &mut result)?;
        let count = chunk.len();
        let vector = result.column(0).flattened(count);
        if vector.logical_type() != &LogicalType::INTEGER {
            return Err(Error::internal("CHECK constraint does not evaluate to INTEGER"));
        }
        let values = vector.values::<i32>();
        if (0..count).any(|row| !vector.is_null(row) && values[row] == 0) {
            return Err(Error::new_bytes(
                ErrorKind::Constraint,
                rustdb_common::byte_format!(
                    "CHECK constraint failed on table {} with expression {}",
                    self.table,
                    check.text
                ),
            ));
        }
        Ok(())
    }
}

impl ConstraintVerifier for CheckVerifier {
    fn verify_append(&self, chunk: &DataChunk) -> Result<()> {
        // `VerifyGeneratedExpressionSuccess`: a value the expression cannot produce fails
        // the INSERT rather than later SELECTs.
        for (expression, text) in &self.generated {
            let mut executor = ExpressionExecutor::new(vec![expression.clone()]);
            let mut result = DataChunk::default();
            if let Err(error) = executor.execute(chunk, &mut result) {
                if error.kind() == ErrorKind::Internal {
                    return Err(error);
                }
                return Err(Error::new_bytes(
                    ErrorKind::Constraint,
                    rustdb_common::byte_format!(
                        "Incorrect value for generated column \"{text}\" : {}",
                        error.raw_message_bytes()
                    ),
                ));
            }
        }
        for check in &self.checks {
            self.verify(check, chunk)?;
        }
        Ok(())
    }

    /// `VerifyUpdateConstraints`: a CHECK is verified when the update touches one of its
    /// columns; the binder made sure all of them are part of the update then. The values
    /// are put at their table positions of a mock chunk.
    fn verify_update(&self, chunk: &DataChunk, column_ids: &[usize]) -> Result<()> {
        for check in &self.checks {
            if !check
                .bound_columns
                .iter()
                .any(|column| column_ids.contains(column))
            {
                continue;
            }
            let columns = self
                .types
                .iter()
                .enumerate()
                .map(|(column, ty)| {
                    column_ids.iter().position(|&id| id == column).map_or_else(
                        || Vector::constant_null(ty.clone()),
                        |position| chunk.column(position).clone(),
                    )
                })
                .collect();
            self.verify(check, &DataChunk::new(columns, chunk.len()))?;
        }
        Ok(())
    }
}

/// The storage-level constraints of the table `entry` (stored as `storage`) from its
/// bound constraints; foreign keys are resolved to the other table's storage through
/// `catalogs`.
pub fn table_constraints(
    transaction: &Transaction,
    catalogs: &[Arc<Catalog>],
    entry: &CatalogEntry,
    storage: &Arc<DataTable>,
    bound: &[BoundConstraint],
) -> Result<TableConstraints> {
    let mut constraints = TableConstraints::default();
    if let EntryKind::Table(table) = &entry.kind {
        constraints.column_names = table
            .physical_columns()
            .map(|(_, _, column)| column.name.clone())
            .collect();
    }
    // Foreign keys reference tables of the table's own catalog
    // (`Catalog::GetEntry(table.catalog, fk.schema, fk.table)`).
    let own_catalog: Vec<Arc<Catalog>> = catalogs
        .iter()
        .filter(|catalog| catalog.io().is_ok_and(|io| Arc::ptr_eq(io, storage.io())))
        .take(1)
        .cloned()
        .collect();
    let catalogs = if own_catalog.is_empty() {
        catalogs
    } else {
        &own_catalog[..]
    };
    let mut checks = Vec::new();
    let mut generated = Vec::new();
    for constraint in bound {
        match constraint {
            BoundConstraint::NotNull(column) => constraints.not_null.push(*column),
            BoundConstraint::Check {
                expression,
                bound_columns,
                text,
            } => checks.push(CheckConstraint {
                expression: expression.clone(),
                bound_columns: bound_columns.clone(),
                text: text.clone(),
            }),
            BoundConstraint::Unique { .. } => {}
            BoundConstraint::Generated { expression, text } => {
                generated.push((expression.clone(), text.clone()));
            }
            BoundConstraint::ForeignKey(fk) => {
                let other = if fk.fk_type == ForeignKeyType::FkTypeSelfReferenceTable {
                    Some(Arc::clone(storage))
                } else {
                    find_storage(transaction, catalogs, &fk.schema, &fk.table)
                };
                let Some(other) = other else {
                    continue;
                };
                let append = BoundForeignKey {
                    kind: ForeignKeyKind::Append,
                    other: Arc::clone(&other),
                    columns: fk.fk_keys.clone(),
                    other_columns: fk.pk_keys.clone(),
                };
                let delete = BoundForeignKey {
                    kind: ForeignKeyKind::Delete,
                    other,
                    columns: fk.pk_keys.clone(),
                    other_columns: fk.fk_keys.clone(),
                };
                match fk.fk_type {
                    ForeignKeyType::FkTypeForeignKeyTable => constraints.foreign_keys.push(append),
                    ForeignKeyType::FkTypePrimaryKeyTable => constraints.foreign_keys.push(delete),
                    ForeignKeyType::FkTypeSelfReferenceTable => {
                        constraints.foreign_keys.push(append);
                        constraints.foreign_keys.push(delete);
                    }
                }
            }
        }
    }
    if !checks.is_empty() || !generated.is_empty() {
        constraints.verifier = Some(Arc::new(CheckVerifier {
            table: storage.name(),
            types: storage.types(),
            checks,
            generated,
        }));
    }
    Ok(constraints)
}

/// The storage of table `schema.name`, looked up in the first catalog that has it.
fn find_storage(
    transaction: &Transaction,
    catalogs: &[Arc<Catalog>],
    schema: &[u8],
    name: &[u8],
) -> Option<Arc<DataTable>> {
    catalogs.iter().find_map(|catalog| {
        let entry = catalog.get_entry(transaction, CatalogType::Table, schema, name)?;
        match &entry.kind {
            EntryKind::Table(table) => table.storage.clone(),
            _ => None,
        }
    })
}
