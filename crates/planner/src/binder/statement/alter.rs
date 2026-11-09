//! `ALTER TABLE` and `ALTER VIEW` (DuckDB's `Binder::Bind(AlterStatement &)` and the
//! checks of `DuckTableEntry::AlterEntry`, which are done here so that execution only
//! changes storage and the catalog).

use std::sync::Arc;

use roastedduck_ast::visit::{VisitorMut, walk_expression};
use roastedduck_ast::{
    AlterInfo, AlterInfoKind, AlterTableInfo, AlterViewInfo, Constraint, ForeignKeyType, NotNullConstraint,
    OnEntryNotFound, ParsedExpression, ParsedExpressionKind, TableColumnType, UniqueConstraint,
};
use roastedduck_catalog::{CatalogType, EntryKind, TableColumn, TableEntry};
use roastedduck_common::{ByteString, Error, LogicalIndex, Result};
use roastedduck_types::{LogicalType, LogicalTypeId};

use super::super::expression::bind_logical_type_in_schema;
use super::super::{BindScope, BoundStatement};
use super::table::{
    bind_constraints, bind_default_value, bind_schema_or_catalog, check_columns, missing_column_error,
    unique_keys,
};
use crate::expression_binder::{AlterBinder, ExpressionBinder};
use crate::operator::{AlterAction, BoundConstraint, LogicalAlter, LogicalKind, LogicalOperator};
use crate::prepared::StatementReturnType;

/// Binds `ALTER TABLE` / `ALTER VIEW`.
pub fn bind_alter(scope: &mut BindScope<'_>, info: &mut AlterInfo) -> Result<BoundStatement> {
    scope.with_properties(|properties| properties.return_type = StatementReturnType::Nothing);
    let ty = match &info.kind {
        // `SetColumnCommentInfo::TryResolveCatalogEntry` looks up a table or a view.
        AlterInfoKind::AlterTable(_) | AlterInfoKind::SetColumnComment(_) => CatalogType::Table,
        AlterInfoKind::AlterView(_) => CatalogType::View,
        AlterInfoKind::ChangeOwnership(change) => CatalogType::from_ast(change.entry_catalog_type)
            .ok_or_else(|| Error::internal("ALTER OWNED BY of an unexpected entry"))?,
        AlterInfoKind::SetComment(comment) => CatalogType::from_ast(comment.entry_catalog_type)
            .ok_or_else(|| Error::internal("COMMENT ON an unexpected entry"))?,
        // `ALTER DATABASE` changes the database manager, not a catalog.
        AlterInfoKind::AlterDatabase(_) => {
            return Ok(BoundStatement::new(
                LogicalOperator::new(LogicalKind::AlterDatabase(info.clone())),
                vec!["Success".into()],
                vec![LogicalType::BOOLEAN],
            ));
        }
    };
    let (catalog, schema) = bind_schema_or_catalog(scope, &info.catalog, &info.schema)?;
    let if_exists = info.if_not_found == OnEntryNotFound::ReturnNull;
    let found = scope
        .context
        .get_entry(&catalog, &schema, ty, &info.name, if_exists)?;
    let plan = match found {
        None => LogicalAlter {
            catalog,
            schema,
            entry: None,
            action: AlterAction::Nothing,
            dependents_allowed: true,
            info: None,
        },
        Some(found) => {
            if found.catalog.eq_ignore_ascii_case("system") {
                return Err(Error::binder("Can not comment on System Catalog entries"));
            }
            let action = match &info.kind {
                AlterInfoKind::AlterView(AlterViewInfo::RenameView(rename)) => {
                    if !matches!(found.entry.kind, EntryKind::View(_)) {
                        return Err(Error::catalog("Can only modify table with ALTER TABLE statement"));
                    }
                    AlterAction::Rename(rename.new_view_name.clone())
                }
                AlterInfoKind::SetComment(comment) => AlterAction::SetComment(comment.comment_value.clone()),
                AlterInfoKind::SetColumnComment(comment) => AlterAction::SetColumnComment {
                    column: comment.column_name.clone(),
                    comment: comment.comment_value.clone(),
                },
                AlterInfoKind::ChangeOwnership(change) => AlterAction::ChangeOwnership {
                    owner_schema: change.owner_schema.clone(),
                    owner_name: change.owner_name.clone(),
                },
                AlterInfoKind::AlterTable(alter) => {
                    let EntryKind::Table(table) = &found.entry.kind else {
                        if is_add_primary_key(alter) {
                            // `Binder::BindAlterAddIndex`.
                            return Err(Error::binder(roastedduck_common::byte_format!(
                                "Cannot execute the `ALTER TABLE` statement on `{}`, only `Table` entries are accepted.",
                                found.entry.catalog_type().display_name()
                            )));
                        }
                        return Err(Error::catalog("Can only modify view with ALTER VIEW statement"));
                    };
                    bind_alter_table(
                        scope,
                        (&found.catalog, &found.schema),
                        found.entry.name(),
                        table,
                        alter,
                    )?
                }
                _ => return Err(Error::internal("unexpected ALTER")),
            };
            let dependents_allowed = matches!(
                &info.kind,
                AlterInfoKind::AlterTable(AlterTableInfo::AddColumn(_) | AlterTableInfo::SetDefault(_))
            );
            let mut logged = info.clone();
            logged.catalog.clone_from(&found.catalog);
            logged.schema.clone_from(&found.schema);
            LogicalAlter {
                catalog: found.catalog,
                schema: found.schema,
                entry: Some(Arc::clone(&found.entry)),
                action,
                dependents_allowed,
                info: Some(Box::new(logged)),
            }
        }
    };
    Ok(BoundStatement::new(
        LogicalOperator::new(LogicalKind::Alter(Box::new(plan))),
        vec!["Success".into()],
        vec![LogicalType::BOOLEAN],
    ))
}

/// The logical index of column `name`; a missing column is an error.
fn column_index(table: &TableEntry, table_name: &[u8], name: &[u8]) -> Result<usize> {
    table
        .column(name)
        .map(|(index, _)| index)
        .ok_or_else(|| missing_column_error(table, table_name, name))
}

/// A table definition with `columns` and `constraints` (DuckDB re-binds the
/// `CreateTableInfo`); the NOT NULL columns follow from the constraints.
fn definition(columns: Vec<TableColumn>, constraints: Vec<Constraint>) -> TableEntry {
    let not_null_columns = constraints
        .iter()
        .filter_map(|constraint| match constraint {
            Constraint::NotNull(not_null) => usize::try_from(not_null.index.0).ok(),
            _ => None,
        })
        .collect();
    TableEntry {
        columns,
        not_null_columns,
        constraints,
        storage: None,
    }
}

/// The key column names of a foreign key on this table's side.
fn foreign_key_columns(fk: &roastedduck_ast::ForeignKeyConstraint) -> Vec<ByteString> {
    match fk.fk_type {
        ForeignKeyType::FkTypeForeignKeyTable => fk.fk_columns.clone(),
        ForeignKeyType::FkTypeSelfReferenceTable => {
            let mut columns = fk.pk_columns.clone();
            columns.extend(fk.fk_columns.iter().cloned());
            columns
        }
        ForeignKeyType::FkTypePrimaryKeyTable => fk.pk_columns.clone(),
    }
}

/// `DuckTableEntry::AlterEntry` for `ALTER TABLE`.
fn bind_alter_table(
    scope: &mut BindScope<'_>,
    table_schema: (&[u8], &[u8]),
    table_name: &[u8],
    table: &TableEntry,
    alter: &AlterTableInfo,
) -> Result<AlterAction> {
    match alter {
        AlterTableInfo::RenameColumn(rename) => {
            rename_column(table, table_name, &rename.old_name, &rename.new_name)
        }
        AlterTableInfo::RenameTable(rename) => Ok(AlterAction::Rename(rename.new_table_name.clone())),
        AlterTableInfo::AddColumn(add) => add_column(
            scope,
            table_schema,
            table,
            &add.new_column,
            add.if_column_not_exists,
        ),
        AlterTableInfo::RemoveColumn(remove) => remove_column(scope, table, table_name, remove),
        AlterTableInfo::SetDefault(set) => {
            let index = column_index(table, table_name, &set.column_name)?;
            let mut columns = table.columns.clone();
            let column = &mut columns[index];
            if column.generated.is_some() {
                return Err(Error::new_bytes(
                    roastedduck_common::ErrorKind::Binder,
                    [
                        "Cannot SET DEFAULT for generated column \"".as_bytes(),
                        column.name.as_bytes(),
                        "\"".as_bytes(),
                    ]
                    .concat(),
                ));
            }
            column.default_value = set.expression.as_deref().cloned();
            let ty = column.logical_type.clone();
            let path = super::table::table_catalog_schema(scope, table);
            super::table::bind_default_value_in(scope, column.default_value.as_ref(), &ty, path.as_ref())?;
            Ok(AlterAction::Definition(definition(
                columns,
                table.constraints.clone(),
            )))
        }
        AlterTableInfo::SetNotNull(set) => {
            let index = column_index(table, table_name, &set.column_name)?;
            if table.columns[index].generated.is_some() {
                return Err(Error::binder("Unsupported constraint for generated column!"));
            }
            if table.not_null_columns.contains(&index) {
                return Ok(AlterAction::Definition(definition(
                    table.columns.clone(),
                    table.constraints.clone(),
                )));
            }
            let mut constraints = table.constraints.clone();
            constraints.push(Constraint::NotNull(NotNullConstraint {
                index: LogicalIndex(index as u64),
            }));
            let column = table
                .logical_to_physical(index)
                .ok_or_else(|| Error::internal("NOT NULL on a generated column"))?;
            Ok(AlterAction::SetNotNull {
                definition: definition(table.columns.clone(), constraints),
                column,
            })
        }
        AlterTableInfo::DropNotNull(drop) => {
            let index = column_index(table, table_name, &drop.column_name)?;
            let mut constraints = table.constraints.clone();
            if let Some(position) = constraints
                .iter()
                .position(|c| matches!(c, Constraint::NotNull(not_null) if not_null.index.0 == index as u64))
            {
                constraints.remove(position);
            }
            Ok(AlterAction::Definition(definition(
                table.columns.clone(),
                constraints,
            )))
        }
        AlterTableInfo::AlterColumnType(change) => alter_column_type(
            scope,
            table_schema,
            table,
            table_name,
            &change.column_name,
            &change.target_type,
            change.expression.as_deref(),
        ),
        AlterTableInfo::AddConstraint(add) => add_constraint(table, table_name, &add.constraint),
        AlterTableInfo::AddField(add) => {
            let mut add = (**add).clone();
            add.new_field.logical_type = bind_logical_type_in_schema(
                &scope.context,
                &add.new_field.logical_type,
                table_schema.0,
                table_schema.1,
            )?;
            let change = super::alter_struct::add_field(table, &add)?;
            field_change(scope, table_schema, table, table_name, change)
        }
        AlterTableInfo::RemoveField(remove) => field_change(
            scope,
            table_schema,
            table,
            table_name,
            super::alter_struct::remove_field(table, remove)?,
        ),
        AlterTableInfo::RenameField(rename) => field_change(
            scope,
            table_schema,
            table,
            table_name,
            Some(super::alter_struct::rename_field(table, rename)?),
        ),
        AlterTableInfo::SetPartitionedBy(_) => Err(Error::not_implemented(
            "SET PARTITIONED BY is not supported for DuckDB tables",
        )),
        AlterTableInfo::SetSortedBy(_) => Err(Error::not_implemented(
            "SET SORTED BY is not supported for DuckDB tables",
        )),
        AlterTableInfo::SetTableOptions(_) => Err(Error::not_implemented(
            "SET (<options>) is not supported for DuckDB tables",
        )),
        AlterTableInfo::ResetTableOptions(_) => Err(Error::not_implemented(
            "RESET (<options>) is not supported for DuckDB tables",
        )),
        other @ AlterTableInfo::ForeignKeyConstraint(_) => Err(Error::not_implemented(
            roastedduck_common::byte_format!("ALTER TABLE {}", other.alter_table_type().name()),
        )),
    }
}

/// A struct field change as the `ALTER COLUMN TYPE` DuckDB turns it into
/// (`ChangeColumnType` with the `remap_struct` expression); `None` changes nothing.
fn field_change(
    scope: &mut BindScope<'_>,
    table_schema: (&[u8], &[u8]),
    table: &TableEntry,
    table_name: &[u8],
    change: Option<super::alter_struct::FieldChange>,
) -> Result<AlterAction> {
    let Some(change) = change else {
        return Ok(AlterAction::Nothing);
    };
    alter_column_type(
        scope,
        table_schema,
        table,
        table_name,
        &change.column,
        &change.new_type,
        Some(&change.expression),
    )
}

/// The generated columns whose expressions reference column `index`, directly or through
/// other generated columns (`ColumnDependencyManager::GetDependents`).
fn generated_dependents(table: &TableEntry, index: usize) -> Vec<usize> {
    let name_of = |i: usize| table.columns[i].name.as_bytes();
    let mut dependents: Vec<usize> = Vec::new();
    let mut frontier = vec![index];
    while let Some(current) = frontier.pop() {
        let name = name_of(current).to_owned();
        for (i, column) in table.columns.iter().enumerate() {
            let Some(generated) = &column.generated else {
                continue;
            };
            if dependents.contains(&i) || i == index {
                continue;
            }
            let mut finder = FindColumnRef {
                name: &name,
                found: false,
            };
            let mut copy = generated.clone();
            finder.visit_expression(&mut copy);
            if finder.found {
                dependents.push(i);
                frontier.push(i);
            }
        }
    }
    dependents
}

/// Whether an expression references column `name`.
struct FindColumnRef<'a> {
    name: &'a [u8],
    found: bool,
}

impl VisitorMut for FindColumnRef<'_> {
    fn visit_expression(&mut self, expression: &mut ParsedExpression) {
        if let ParsedExpressionKind::ColumnRef(column) = &expression.kind
            && column
                .column_names
                .last()
                .is_some_and(|last| last.as_bytes().eq_ignore_ascii_case(self.name))
        {
            self.found = true;
        }
        walk_expression(self, expression);
    }
}

/// Renames column references to `old` in parsed expressions (`RenameExpression`).
struct RenameColumnRefs<'a> {
    old: &'a [u8],
    new: &'a [u8],
}

impl VisitorMut for RenameColumnRefs<'_> {
    fn visit_expression(&mut self, expression: &mut ParsedExpression) {
        if let ParsedExpressionKind::ColumnRef(column) = &mut expression.kind
            && let Some(last) = column.column_names.last_mut()
            && last.as_bytes() == self.old
        {
            *last = self.new.into();
        }
        walk_expression(self, expression);
    }
}

/// `DuckTableEntry::RenameColumn`.
fn rename_column(table: &TableEntry, table_name: &[u8], old: &[u8], new: &[u8]) -> Result<AlterAction> {
    let index = column_index(table, table_name, old)?;
    if table
        .columns
        .iter()
        .enumerate()
        .any(|(i, column)| i != index && column.name.eq_ignore_ascii_case(new))
    {
        return Err(Error::catalog(roastedduck_common::byte_format!(
            "Column with name {new} already exists!"
        )));
    }
    let old = table.columns[index].name.clone();
    let dependents = generated_dependents(table, index);
    let mut columns = table.columns.clone();
    columns[index].name = new.into();
    let mut renamer = RenameColumnRefs { old: &old, new };
    for dependent in dependents {
        if let Some(generated) = &mut columns[dependent].generated {
            renamer.visit_expression(generated);
        }
    }
    let mut constraints = Vec::with_capacity(table.constraints.len());
    for constraint in &table.constraints {
        let mut copy = constraint.clone();
        match &mut copy {
            Constraint::NotNull(_) => {}
            Constraint::Check(check) => renamer.visit_expression(&mut check.expression),
            Constraint::Unique(unique) => {
                for column in &mut unique.columns {
                    if column.as_bytes() == old.as_bytes() {
                        *column = new.into();
                    }
                }
            }
            Constraint::ForeignKey(fk) => {
                if foreign_key_columns(fk)
                    .iter()
                    .any(|name| name.as_bytes() == old.as_bytes())
                {
                    return Err(Error::new_bytes(
                        roastedduck_common::ErrorKind::Catalog,
                        [
                            "Cannot rename column \"".as_bytes(),
                            old.as_bytes(),
                            "\" because this is involved in the foreign key constraint".as_bytes(),
                        ]
                        .concat(),
                    ));
                }
            }
        }
        constraints.push(copy);
    }
    Ok(AlterAction::Definition(definition(columns, constraints)))
}

/// `DuckTableEntry::AddColumn`.
fn add_column(
    scope: &mut BindScope<'_>,
    (catalog, schema): (&[u8], &[u8]),
    table: &TableEntry,
    column: &roastedduck_ast::ColumnDefinition,
    if_not_exists: bool,
) -> Result<AlterAction> {
    if table.column(&column.name).is_some() {
        if if_not_exists {
            return Ok(AlterAction::Nothing);
        }
        return Err(Error::new_bytes(
            roastedduck_common::ErrorKind::Catalog,
            [
                "Column with name ".as_bytes(),
                column.name.as_bytes(),
                " already exists!".as_bytes(),
            ]
            .concat(),
        ));
    }
    if column.category == TableColumnType::Generated {
        return Err(Error::parser(
            "Adding generated columns after table creation is not supported yet",
        ));
    }
    let logical_type = bind_logical_type_in_schema(&scope.context, &column.logical_type, catalog, schema)?;
    let default_value = column.expression.as_deref().cloned();
    let default = bind_default_value(scope, default_value.as_ref(), &logical_type)?;
    let mut columns = table.columns.clone();
    columns.push(TableColumn {
        name: column.name.clone(),
        logical_type: logical_type.clone(),
        default_value,
        generated: None,
        comment: column.comment.clone(),
        compression_type: column.compression_type,
    });
    Ok(AlterAction::AddColumn {
        definition: definition(columns, table.constraints.clone()),
        name: column.name.clone(),
        logical_type,
        default,
    })
}

/// `DuckTableEntry::RemoveColumn` with `UpdateConstraintsOnColumnDrop`.
fn remove_column(
    scope: &mut BindScope<'_>,
    table: &TableEntry,
    table_name: &[u8],
    remove: &roastedduck_ast::RemoveColumnInfo,
) -> Result<AlterAction> {
    let Some((removed, _)) = table.column(&remove.removed_column) else {
        if remove.if_column_exists {
            return Ok(AlterAction::Nothing);
        }
        return Err(missing_column_error(table, table_name, &remove.removed_column));
    };
    if table.columns.len() == 1 {
        return Err(Error::catalog(
            "Cannot drop column: table only has one column remaining!",
        ));
    }
    let removed_name = &remove.removed_column;
    let dependents = generated_dependents(table, removed);
    if !dependents.is_empty() && !remove.cascade {
        return Err(Error::catalog(
            "Cannot drop column: column is a dependency of 1 or more generated column(s)",
        ));
    }
    let mut removed_columns = dependents;
    removed_columns.push(removed);
    if removed_columns.len() >= table.columns.len() {
        return Err(Error::catalog(
            "Cannot drop column: table only has one column remaining!",
        ));
    }
    let adjust = |index: usize| index - removed_columns.iter().filter(|&&r| r < index).count();
    let Some(physical) = table.logical_to_physical(removed) else {
        // A generated column has no storage: only the definition changes.
        let mut constraints = Vec::with_capacity(table.constraints.len());
        for constraint in &table.constraints {
            let mut copy = constraint.clone();
            match &mut copy {
                Constraint::NotNull(not_null) => {
                    not_null.index = LogicalIndex(adjust(not_null.index.0 as usize) as u64);
                }
                Constraint::Unique(unique) if unique.index.0 != u64::MAX => {
                    unique.index = LogicalIndex(adjust(unique.index.0 as usize) as u64);
                }
                _ => {}
            }
            constraints.push(copy);
        }
        let columns = table
            .columns
            .iter()
            .enumerate()
            .filter(|(i, _)| !removed_columns.contains(i))
            .map(|(_, column)| column.clone())
            .collect();
        return Ok(AlterAction::Definition(definition(columns, constraints)));
    };
    let bound = bind_constraints(scope, table_name, table)?;
    let mut constraints = Vec::with_capacity(table.constraints.len());
    for (constraint, bound) in table.constraints.iter().zip(&bound) {
        match (constraint, bound) {
            (Constraint::NotNull(not_null), _) => {
                let index = usize::try_from(not_null.index.0).unwrap_or(usize::MAX);
                if index != removed {
                    constraints.push(Constraint::NotNull(NotNullConstraint {
                        index: LogicalIndex(adjust(index) as u64),
                    }));
                }
            }
            (Constraint::Check(_), BoundConstraint::Check { bound_columns, .. }) => {
                if bound_columns.contains(&physical) {
                    if bound_columns.len() > 1 {
                        return Err(Error::catalog(roastedduck_common::byte_format!(
                            "Cannot drop column \"{removed_name}\" because there is a CHECK constraint that depends on it"
                        )));
                    }
                } else {
                    constraints.push(constraint.clone());
                }
            }
            (Constraint::Unique(unique), _) => {
                let mut copy = unique.clone();
                if unique.index.0 != u64::MAX {
                    let index = usize::try_from(unique.index.0).unwrap_or(usize::MAX);
                    if index == removed {
                        return Err(Error::catalog(roastedduck_common::byte_format!(
                            "Cannot drop column \"{removed_name}\" because there is a UNIQUE constraint that depends on it"
                        )));
                    }
                    copy.index = LogicalIndex(adjust(index) as u64);
                } else if unique.columns.iter().any(|name| name == removed_name) {
                    return Err(Error::catalog(roastedduck_common::byte_format!(
                        "Cannot drop column \"{removed_name}\" because it is referenced in unique constraint UNIQUE({})",
                        ByteString::join(&unique.columns, ", ")
                    )));
                }
                constraints.push(Constraint::Unique(copy));
            }
            (Constraint::ForeignKey(fk), _) => {
                if foreign_key_columns(fk).iter().any(|name| name == removed_name) {
                    return Err(Error::catalog(roastedduck_common::byte_format!(
                        "Cannot drop column \"{removed_name}\" because there is a FOREIGN KEY constraint that depends on it"
                    )));
                }
                constraints.push(constraint.clone());
            }
            (Constraint::Check(_), _) => return Err(Error::internal("unbound CHECK constraint")),
        }
    }
    let columns = table
        .columns
        .iter()
        .enumerate()
        .filter(|(i, _)| !removed_columns.contains(i))
        .map(|(_, column)| column.clone())
        .collect();
    Ok(AlterAction::RemoveColumn {
        definition: definition(columns, constraints),
        column: physical,
    })
}

/// `DuckTableEntry::ChangeColumnType`.
fn alter_column_type(
    scope: &mut BindScope<'_>,
    (catalog, schema): (&[u8], &[u8]),
    table: &TableEntry,
    table_name: &[u8],
    column_name: &[u8],
    target_type: &LogicalType,
    expression: Option<&ParsedExpression>,
) -> Result<AlterAction> {
    let mut target = if target_type.id() == LogicalTypeId::Unknown {
        None
    } else {
        Some(bind_logical_type_in_schema(
            &scope.context,
            target_type,
            catalog,
            schema,
        )?)
    };
    let index = column_index(table, table_name, column_name)?;
    let mut expression = expression
        .cloned()
        .unwrap_or_else(|| ParsedExpression::column_ref(vec![table.columns[index].name.clone()]));
    let columns = check_columns(table);
    let mut binder = scope.binder.child()?;
    let mut alter_binder = AlterBinder::new(&mut binder, table_name, &columns, target.clone());
    let bound = alter_binder.bind(&mut expression, true)?;
    let bound_columns = alter_binder.bound_columns.clone();
    if table.columns[index].generated.is_some() {
        return Err(Error::not_implemented(
            "Changing types of generated columns is not supported yet",
        ));
    }
    if let Some(&dependent) = generated_dependents(table, index).first() {
        return Err(Error::new_bytes(
            roastedduck_common::ErrorKind::Binder,
            [
                "This column is referenced by the generated column \"".as_bytes(),
                table.columns[dependent].name.as_bytes(),
                "\", so its type can not be changed".as_bytes(),
            ]
            .concat(),
        ));
    }
    let target = match target.take() {
        Some(target) => target,
        None => bound.return_type.clone(),
    };
    let physical = table
        .logical_to_physical(index)
        .ok_or_else(|| Error::not_implemented("Changing types of generated columns is not supported yet"))?;
    let bound_constraints = bind_constraints(scope, table_name, table)?;
    for (constraint, bound_constraint) in table.constraints.iter().zip(&bound_constraints) {
        match (constraint, bound_constraint) {
            (_, BoundConstraint::Check { bound_columns, .. }) if bound_columns.contains(&physical) => {
                return Err(Error::binder(
                    "Cannot change the type of a column that has a CHECK constraint specified",
                ));
            }
            (_, BoundConstraint::Unique { keys, .. }) if keys.contains(&physical) => {
                return Err(Error::binder(
                    "Cannot change the type of a column that has a UNIQUE or PRIMARY KEY constraint specified",
                ));
            }
            (Constraint::ForeignKey(fk), _) => {
                let keys: Vec<u64> = match fk.fk_type {
                    ForeignKeyType::FkTypeForeignKeyTable => fk.fk_keys.iter().map(|k| k.0).collect(),
                    ForeignKeyType::FkTypePrimaryKeyTable => fk.pk_keys.iter().map(|k| k.0).collect(),
                    ForeignKeyType::FkTypeSelfReferenceTable => {
                        fk.pk_keys.iter().chain(&fk.fk_keys).map(|k| k.0).collect()
                    }
                };
                if keys.contains(&(physical as u64)) {
                    return Err(Error::binder(
                        "Cannot change the type of a column that has a FOREIGN KEY constraint specified",
                    ));
                }
            }
            _ => {}
        }
    }
    let mut new_columns = table.columns.clone();
    new_columns[index].logical_type = target.clone();
    Ok(AlterAction::AlterType {
        definition: definition(new_columns, table.constraints.clone()),
        column: physical,
        target,
        expression: bound,
        bound_columns,
    })
}

/// `UniqueConstraint::ToString`.
fn unique_to_string(table: &TableEntry, unique: &UniqueConstraint) -> ByteString {
    let mut names = unique.columns.clone();
    if names.is_empty()
        && let Some(column) = usize::try_from(unique.index.0)
            .ok()
            .and_then(|i| table.columns.get(i))
    {
        names.push(column.name.clone());
    }
    let names: Vec<ByteString> = names
        .iter()
        .map(|name| roastedduck_ast::keyword::write_optionally_quoted_bytes(name))
        .collect();
    let base = if unique.is_primary_key {
        "PRIMARY KEY("
    } else {
        "UNIQUE("
    };
    roastedduck_common::byte_format!("{base}{})", ByteString::join(&names, ", "))
}

/// `AlterInfo::IsAddPrimaryKey`.
fn is_add_primary_key(alter: &AlterTableInfo) -> bool {
    matches!(alter, AlterTableInfo::AddConstraint(add)
        if matches!(&*add.constraint, Constraint::Unique(unique) if unique.is_primary_key))
}

/// `ALTER TABLE ... ADD PRIMARY KEY` (`Binder::BindAlterAddIndex` with
/// `DuckTableEntry::AddConstraint`).
fn add_constraint(table: &TableEntry, table_name: &[u8], constraint: &Constraint) -> Result<AlterAction> {
    let Constraint::Unique(unique) = constraint else {
        return Err(Error::internal(
            "unsupported constraint type in ALTER TABLE statement",
        ));
    };
    let keys = unique_keys(table_name, table, unique)?;
    let mut names = unique.columns.clone();
    if names.is_empty() {
        names = keys
            .iter()
            .filter_map(|&physical| table.physical_to_logical(physical))
            .map(|logical| table.columns[logical].name.clone())
            .collect();
    }
    let kind = if unique.is_primary_key {
        "PRIMARY"
    } else {
        "UNIQUE"
    };
    let index_name =
        roastedduck_common::byte_format!("{kind}_{table_name}_{}", ByteString::join(&names, "_"));
    let existing = table.constraints.iter().find_map(|constraint| match constraint {
        Constraint::Unique(existing) if existing.is_primary_key => Some(unique_to_string(table, existing)),
        _ => None,
    });
    let mut constraints = table.constraints.clone();
    constraints.push(constraint.clone());
    if unique.is_primary_key {
        for &key in &keys {
            let Some(logical) = table.physical_to_logical(key) else {
                continue;
            };
            if !table.not_null_columns.contains(&logical) {
                constraints.push(Constraint::NotNull(NotNullConstraint {
                    index: LogicalIndex(logical as u64),
                }));
            }
        }
    }
    Ok(AlterAction::AddPrimaryKey {
        definition: definition(table.columns.clone(), constraints),
        keys,
        index_name,
        existing: if unique.is_primary_key { existing } else { None },
    })
}
