//! The transform functions of DuckDB's `transform_alter.cpp`: `ALTER TABLE / VIEW /
//! SEQUENCE / DATABASE / SCHEMA`.
//!
//! DuckDB's `AlterTableInfo` subclasses are [`AlterInfo`]s here; the table-level
//! options build them with an empty `AlterEntryData` that `TransformAlterTableStmt`
//! fills in.

use rustdb_parser::hash_order::CiHashMap;

use super::create_table::{base_table_ref, column_definition, on_entry_not_found};
use super::prelude::*;

/// `DEFAULT_SCHEMA`.
const DEFAULT_SCHEMA: &str = "main";

/// DuckDB's `AlterEntryData`: the entry an `ALTER` applies to.
#[derive(Clone, Debug)]
struct AlterEntryData {
    catalog: ByteString,
    schema: ByteString,
    name: ByteString,
    if_not_found: OnEntryNotFound,
}

impl AlterEntryData {
    /// `AlterInfo::GetAlterEntryData`.
    fn of(info: &AlterInfo) -> Self {
        Self {
            catalog: info.catalog.clone(),
            schema: info.schema.clone(),
            name: info.name.clone(),
            if_not_found: info.if_not_found,
        }
    }

    /// An `AlterInfo` of `kind` on this entry.
    fn alter_info(&self, kind: AlterInfoKind) -> AlterInfo {
        AlterInfo {
            catalog: self.catalog.clone(),
            schema: self.schema.clone(),
            name: self.name.clone(),
            if_not_found: self.if_not_found,
            allow_internal: false,
            kind,
        }
    }
}

/// An `AlterInfo` of `kind` with DuckDB's `AlterEntryData()`: no entry yet (its
/// uninitialized `if_not_found` is always overwritten by the callers).
fn unnamed_alter_info(kind: AlterInfoKind) -> AlterInfo {
    AlterInfo {
        catalog: String::new().into(),
        schema: String::new().into(),
        name: String::new().into(),
        if_not_found: OnEntryNotFound::ThrowException,
        allow_internal: false,
        kind,
    }
}

/// An `AlterTableInfo` with DuckDB's `AlterEntryData()`.
fn alter_table_info(info: AlterTableInfo) -> AlterInfo {
    unnamed_alter_info(AlterInfoKind::AlterTable(info))
}

/// An `AlterStatement`.
fn alter_statement(info: AlterInfo) -> SqlStatement {
    SqlStatement::new(SqlStatementKind::Alter(Box::new(info)))
}

/// `PEGTransformerFactory::AddToMultiStatement`.
fn add_to_multi_statement(multi_statement: &mut Vec<SqlStatement>, alter_info: AlterInfo) {
    multi_statement.push(alter_statement(alter_info));
}

/// `PEGTransformerFactory::AddUpdateToMultiStatement`: `UPDATE table SET column =
/// expression`.
fn add_update_to_multi_statement(
    multi_statement: &mut Vec<SqlStatement>,
    column_name: &[u8],
    table_data: &AlterEntryData,
    original_expression: &ParsedExpression,
) {
    let table_ref = rustdb_parser::build::table_ref(TableRefKind::BaseTable(Box::new(BaseTableRef {
        schema_name: table_data.schema.clone(),
        table_name: table_data.name.clone(),
        column_name_alias: Vec::new(),
        catalog_name: table_data.catalog.clone(),
        at_clause: None,
    })));
    let update_statement = UpdateStatement {
        table: Box::new(table_ref),
        from_table: None,
        set_info: Box::new(UpdateSetInfo {
            condition: None,
            columns: vec![column_name.to_owned().into()],
            expressions: vec![original_expression.clone()],
        }),
        returning_list: Vec::new(),
        cte_map: CommonTableExpressionMap::default(),
        prioritize_table_when_binding: true,
    };
    multi_statement.push(SqlStatement::new(SqlStatementKind::Update(Box::new(
        update_statement,
    ))));
}

/// `PEGTransformerFactory::TransformAndMaterializeAlter`: `ADD COLUMN c DEFAULT
/// <non-constant>` becomes `ALTER TABLE t ADD COLUMN c DEFAULT NULL; UPDATE t SET c =
/// <expr>; ALTER TABLE t ALTER c SET DEFAULT <expr>`, so that the values are
/// materialized once and WAL replays see the same values.
fn transform_and_materialize_alter(
    data: &AlterEntryData,
    info_with_null_placeholder: AlterInfo,
    column_name: &[u8],
    expression: ParsedExpression,
) -> SqlStatement {
    let mut multi_statement = Vec::new();
    // 1. `ALTER TABLE t ADD COLUMN col <type> DEFAULT NULL;`
    add_to_multi_statement(&mut multi_statement, info_with_null_placeholder);
    // 2. `UPDATE t SET col = <expression>;`
    add_update_to_multi_statement(&mut multi_statement, column_name, data, &expression);
    // 3. `ALTER TABLE t ALTER col SET DEFAULT <expression>;`
    add_to_multi_statement(
        &mut multi_statement,
        data.alter_info(AlterInfoKind::AlterTable(AlterTableInfo::SetDefault(
            SetDefaultInfo {
                column_name: column_name.to_owned().into(),
                expression: Some(Box::new(expression)),
            },
        ))),
    );
    SqlStatement::new(SqlStatementKind::Multi(multi_statement))
}

impl PegTransformer {
    /// `TransformAlterStatement`: `ALTER ...`; adding a column with a non-constant
    /// default becomes a multi-statement (`TransformAndMaterializeAlter`).
    pub(super) fn transform_alter_statement(&mut self, parse_result: &ParseResult) -> Result<SqlStatement> {
        let info: AlterInfo = self.transform_child(parse_result.list()?, 1)?;
        let AlterInfoKind::AlterTable(AlterTableInfo::AddColumn(add_column)) = &info.kind else {
            return Ok(alter_statement(info));
        };
        let column_entry = &add_column.new_column;
        // `ColumnDefinition::HasDefaultValue` / `DefaultValue`.
        let default_value = column_entry
            .expression
            .as_deref()
            .filter(|_| column_entry.category != TableColumnType::Generated);
        let Some(default_value) = default_value else {
            return Ok(alter_statement(info));
        };
        if default_value.expression_class() == ExpressionClass::Constant {
            return Ok(alter_statement(info));
        }
        let mut null_column = column_entry.clone();
        null_column.expression = Some(Box::new(ParsedExpression::constant(Value::default())));
        let alter_entry_data = AlterEntryData::of(&info);
        // DuckDB passes whether the table may be missing as `if_column_not_exists`.
        let null_placeholder = alter_entry_data.alter_info(AlterInfoKind::AlterTable(
            AlterTableInfo::AddColumn(Box::new(AddColumnInfo {
                new_column: null_column,
                if_column_not_exists: info.if_not_found == OnEntryNotFound::ReturnNull,
            })),
        ));
        Ok(transform_and_materialize_alter(
            &alter_entry_data,
            null_placeholder,
            &column_entry.name,
            default_value.clone(),
        ))
    }

    /// `TransformAlterOptions`: what is altered.
    pub(super) fn transform_alter_options(&mut self, parse_result: &ParseResult) -> Result<AlterInfo> {
        self.transform(parse_result.list()?.choice(0)?)
    }

    /// `TransformAlterTableStmt`: `TABLE [IF EXISTS] table option`; one option only.
    pub(super) fn transform_alter_table_stmt(&mut self, parse_result: &ParseResult) -> Result<AlterInfo> {
        let list = parse_result.list()?;
        let if_exists = list.has(1)?;
        let table = base_table_ref(self.transform_child(list, 2)?)?;
        let alter_option_list = list.child(3)?.list_elements()?;
        if alter_option_list.len() > 1 {
            return Err(Error::parser("Only one ALTER command per statement is supported"));
        }
        let alter_option = alter_option_list
            .first()
            .ok_or_else(|| Error::internal("ALTER TABLE without an option"))?;
        let mut result: AlterInfo = self.transform(alter_option)?;
        result.if_not_found = on_entry_not_found(if_exists);
        result.catalog = table.catalog_name;
        result.schema = table.schema_name;
        result.name = table.table_name;
        Ok(result)
    }

    /// `TransformAlterDatabaseStmt`: `DATABASE [IF EXISTS] name SET ALIAS TO new_name`.
    pub(super) fn transform_alter_database_stmt(&mut self, parse_result: &ParseResult) -> Result<AlterInfo> {
        let list = parse_result.list()?;
        let if_exists = list.has(1)?;
        let not_found = on_entry_not_found(if_exists);
        let catalog_name = list.identifier(2)?.to_owned();
        let new_name = list.identifier(6)?.to_owned();
        let data = AlterEntryData {
            catalog: catalog_name.into(),
            schema: String::new().into(),
            name: String::new().into(),
            if_not_found: not_found,
        };
        Ok(
            data.alter_info(AlterInfoKind::AlterDatabase(AlterDatabaseInfo::RenameDatabase(
                RenameDatabaseInfo {
                    new_name: new_name.into(),
                },
            ))),
        )
    }

    /// `TransformAlterViewStmt`: `VIEW [IF EXISTS] view RENAME TO name`.
    pub(super) fn transform_alter_view_stmt(&mut self, parse_result: &ParseResult) -> Result<AlterInfo> {
        let list = parse_result.list()?;
        let if_exists = list.has(1)?;
        let base_table = base_table_ref(self.transform_child(list, 2)?)?;
        let alter_table_info: AlterInfo = self.transform_child(list, 3)?;
        let AlterInfoKind::AlterTable(AlterTableInfo::RenameTable(rename_table)) = alter_table_info.kind
        else {
            return Err(Error::internal("Expected a RENAME TO in ALTER VIEW"));
        };
        let data = AlterEntryData {
            catalog: base_table.catalog_name,
            schema: base_table.schema_name,
            name: base_table.table_name,
            if_not_found: on_entry_not_found(if_exists),
        };
        Ok(
            data.alter_info(AlterInfoKind::AlterView(AlterViewInfo::RenameView(
                RenameViewInfo {
                    new_view_name: rename_table.new_table_name,
                },
            ))),
        )
    }

    /// `TransformAlterSchemaStmt`: not supported.
    pub(super) fn transform_alter_schema_stmt(&mut self, _parse_result: &ParseResult) -> Result<AlterInfo> {
        Err(Error::not_implemented("Altering schemas is not yet supported"))
    }

    /// `TransformAlterSequenceStmt`: `SEQUENCE [IF EXISTS] name options`.
    pub(super) fn transform_alter_sequence_stmt(&mut self, parse_result: &ParseResult) -> Result<AlterInfo> {
        let list = parse_result.list()?;
        let if_exists = list.has(1)?;
        let sequence_name: QualifiedName = self.transform_child(list, 2)?;
        let mut alter_info: AlterInfo = self.transform_child(list, 3)?;
        if sequence_name.schema.is_empty() {
            alter_info.schema = sequence_name.catalog;
        } else {
            alter_info.catalog = sequence_name.catalog;
            alter_info.schema = sequence_name.schema;
        }
        alter_info.name = sequence_name.name;
        alter_info.if_not_found = on_entry_not_found(if_exists);
        Ok(alter_info)
    }

    /// `TransformQualifiedSequenceName`: `[catalog.][schema.]name`.
    pub(super) fn transform_qualified_sequence_name(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<QualifiedName> {
        let list = parse_result.list()?;
        let mut result = QualifiedName::default();
        if let Some(catalog) = self.transform_optional(list, 0)? {
            result.catalog = catalog;
        }
        if let Some(schema) = self.transform_optional(list, 1)? {
            result.schema = schema;
        }
        result.name = list.identifier(2)?.into();
        Ok(result)
    }

    /// `TransformAlterSequenceOptions`: `RENAME TO name` (an `AlterTableInfo` in DuckDB)
    /// or sequence options.
    pub(super) fn transform_alter_sequence_options(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<AlterInfo> {
        self.transform(parse_result.list()?.choice(0)?)
    }

    /// `TransformSetSequenceOption`: only `OWNED BY` is supported.
    pub(super) fn transform_set_sequence_option(&mut self, parse_result: &ParseResult) -> Result<AlterInfo> {
        let mut owned_info = None;
        for seq_option_pr in parse_result.list()?.repeat(0)? {
            let (name, option): (String, SequenceOption) = self.transform(seq_option_pr)?;
            if name == "owned" {
                if owned_info.is_some() {
                    return Err(Error::parser("Owned by value should be passed at most once"));
                }
                let SequenceOption::Qualified(_, owned_by) = option else {
                    return Err(Error::internal("Expected a qualified name for OWNED BY"));
                };
                let schema = if owned_by.schema.is_empty() {
                    ByteString::from(DEFAULT_SCHEMA)
                } else {
                    owned_by.schema
                };
                owned_info = Some(unnamed_alter_info(AlterInfoKind::ChangeOwnership(
                    ChangeOwnershipInfo {
                        entry_catalog_type: CatalogType::SequenceEntry,
                        owner_schema: schema,
                        owner_name: owned_by.name,
                    },
                )));
            }
        }
        owned_info.ok_or_else(|| Error::not_implemented("ALTER SEQUENCE option not yet supported"))
    }

    /// `TransformAlterTableOptions`.
    pub(super) fn transform_alter_table_options(&mut self, parse_result: &ParseResult) -> Result<AlterInfo> {
        self.transform(parse_result.list()?.choice(0)?)
    }

    /// `TransformAddColumn`: `ADD [COLUMN] [IF NOT EXISTS] column`; a dotted name adds a
    /// struct field.
    pub(super) fn transform_add_column(&mut self, parse_result: &ParseResult) -> Result<AlterInfo> {
        let list = parse_result.list()?;
        let new_column: AddColumnEntry = self.transform_child(list, 3)?;
        let if_not_exists = list.has(2)?;
        let Some((name, parent_path)) = new_column.column_path.split_last() else {
            return Err(Error::internal("ADD COLUMN without a column name"));
        };
        let mut column = column_definition(name.clone(), new_column.ty);
        if let Some(default_value) = new_column.default_value {
            column.expression = Some(Box::new(default_value));
        }
        if parent_path.is_empty() {
            return Ok(alter_table_info(AlterTableInfo::AddColumn(Box::new(
                AddColumnInfo {
                    new_column: column,
                    if_column_not_exists: if_not_exists,
                },
            ))));
        }
        Ok(alter_table_info(AlterTableInfo::AddField(Box::new(
            AddFieldInfo {
                new_field: column,
                if_field_not_exists: if_not_exists,
                column_path: parent_path.to_vec(),
            },
        ))))
    }

    /// `TransformAddColumnEntry`: `name [type] [DEFAULT expr]`; generated columns and
    /// other constraints are not supported.
    pub(super) fn transform_add_column_entry(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<AddColumnEntry> {
        let list = parse_result.list()?;
        let mut new_column = AddColumnEntry {
            ty: LogicalType::INVALID,
            column_path: self.transform_child(list, 0)?,
            default_value: None,
        };
        if let Some(ty) = self.transform_optional(list, 1)? {
            new_column.ty = ty;
        }
        if list.has(2)? {
            return Err(Error::parser(
                "Adding generated columns after table creation is not supported yet",
            ));
        }
        let Some(constraints_repeat) = list.optional(3)? else {
            return Ok(new_column);
        };
        for constraint_entry in constraints_repeat.repeat()? {
            let constraint = constraint_entry.list()?.choice(0)?;
            if constraint.name == "DefaultValue" {
                if new_column.default_value.is_some() {
                    return Err(Error::parser("Cannot define a default value twice"));
                }
                new_column.default_value = Some(self.transform(constraint)?);
            } else {
                return Err(Error::parser("Adding columns with constraints not yet supported"));
            }
        }
        Ok(new_column)
    }

    /// `TransformDropColumn`: `DROP [COLUMN] [IF EXISTS] column [CASCADE | RESTRICT]`.
    pub(super) fn transform_drop_column(&mut self, parse_result: &ParseResult) -> Result<AlterInfo> {
        let list = parse_result.list()?;
        let cascade = self.transform_optional(list, 4)?.unwrap_or(false);
        let if_exists = list.has(2)?;
        let nested_column: ColumnRefExpression = self.transform_child(list, 3)?;
        if let [column_name] = nested_column.column_names.as_slice() {
            return Ok(alter_table_info(AlterTableInfo::RemoveColumn(RemoveColumnInfo {
                removed_column: column_name.clone(),
                if_column_exists: if_exists,
                cascade,
            })));
        }
        Ok(alter_table_info(AlterTableInfo::RemoveField(RemoveFieldInfo {
            column_path: nested_column.column_names,
            if_column_exists: if_exists,
            cascade,
        })))
    }

    /// `TransformAlterColumn`: `ALTER [COLUMN] column entry`; the column is the first
    /// part of the name.
    pub(super) fn transform_alter_column(&mut self, parse_result: &ParseResult) -> Result<AlterInfo> {
        let list = parse_result.list()?;
        let nested_column_name: ColumnRefExpression = self.transform_child(list, 2)?;
        let mut alter_column_entry: AlterInfo = self.transform_child(list, 3)?;
        let column_name = nested_column_name
            .column_names
            .first()
            .cloned()
            .ok_or_else(|| Error::internal("ALTER COLUMN without a column name"))?;
        match &mut alter_column_entry.kind {
            AlterInfoKind::AlterTable(AlterTableInfo::SetDefault(set_default)) => {
                set_default.column_name = column_name;
            }
            AlterInfoKind::AlterTable(AlterTableInfo::DropNotNull(drop_not_null)) => {
                drop_not_null.column_name = column_name;
            }
            AlterInfoKind::AlterTable(AlterTableInfo::SetNotNull(set_not_null)) => {
                set_not_null.column_name = column_name;
            }
            AlterInfoKind::AlterTable(AlterTableInfo::AlterColumnType(change_column_type)) => {
                change_column_type.column_name = column_name;
                if change_column_type.expression.is_none() {
                    let column_ref = ParsedExpression::new(
                        ExpressionType::ColumnRef,
                        ParsedExpressionKind::ColumnRef(nested_column_name),
                    );
                    change_column_type.expression = Some(Box::new(ParsedExpression::cast(
                        column_ref,
                        change_column_type.target_type.clone(),
                    )));
                }
            }
            _ => {
                return Err(Error::not_implemented(
                    "Unrecognized type for alter column encountered",
                ));
            }
        }
        Ok(alter_column_entry)
    }

    /// `TransformAlterColumnEntry`.
    pub(super) fn transform_alter_column_entry(&mut self, parse_result: &ParseResult) -> Result<AlterInfo> {
        self.transform(parse_result.list()?.choice(0)?)
    }

    /// `TransformDropDefault`: `DROP DEFAULT`.
    pub(super) fn transform_drop_default(&mut self, _parse_result: &ParseResult) -> Result<AlterInfo> {
        Ok(alter_table_info(AlterTableInfo::SetDefault(SetDefaultInfo {
            column_name: String::new().into(),
            expression: None,
        })))
    }

    /// `TransformChangeNullability`: `DROP | SET NOT NULL`.
    pub(super) fn transform_change_nullability(&mut self, parse_result: &ParseResult) -> Result<AlterInfo> {
        let drop_or_set: String = self.transform_child(parse_result.list()?, 0)?;
        if drop_or_set.eq_ignore_ascii_case("drop") {
            return Ok(alter_table_info(AlterTableInfo::DropNotNull(DropNotNullInfo {
                column_name: String::new().into(),
            })));
        }
        Ok(alter_table_info(AlterTableInfo::SetNotNull(SetNotNullInfo {
            column_name: String::new().into(),
        })))
    }

    /// `TransformAlterType`: `[SET DATA] TYPE [type] [USING expr]`.
    pub(super) fn transform_alter_type(&mut self, parse_result: &ParseResult) -> Result<AlterInfo> {
        let list = parse_result.list()?;
        let target_type = self.transform_optional(list, 2)?.unwrap_or(LogicalType::UNKNOWN);
        let using_expr_opt = list.optional(3)?;
        if target_type == LogicalType::UNKNOWN && using_expr_opt.is_none() {
            return Err(Error::parser(
                "Omitting the type is only possible in combination with USING",
            ));
        }
        let expr: Option<ParsedExpression> = self.transform_optional(list, 3)?;
        Ok(alter_table_info(AlterTableInfo::AlterColumnType(
            ChangeColumnTypeInfo {
                column_name: String::new().into(),
                target_type,
                expression: expr.map(Box::new),
            },
        )))
    }

    /// `TransformUsingExpression`: `USING expr`.
    pub(super) fn transform_using_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        self.transform_child(parse_result.list()?, 1)
    }

    /// `TransformDropOrSet`: the keyword.
    pub(super) fn transform_drop_or_set(&mut self, parse_result: &ParseResult) -> Result<String> {
        Ok(parse_result.list()?.choice(0)?.keyword()?.to_owned())
    }

    /// `TransformAddOrDropDefault`.
    pub(super) fn transform_add_or_drop_default(&mut self, parse_result: &ParseResult) -> Result<AlterInfo> {
        self.transform(parse_result.list()?.choice(0)?)
    }

    /// `TransformAddDefault`: `SET DEFAULT expr`.
    pub(super) fn transform_add_default(&mut self, parse_result: &ParseResult) -> Result<AlterInfo> {
        let expr: ParsedExpression = self.transform_child(parse_result.list()?, 2)?;
        Ok(alter_table_info(AlterTableInfo::SetDefault(SetDefaultInfo {
            column_name: String::new().into(),
            expression: Some(Box::new(expr)),
        })))
    }

    /// `TransformRenameColumn`: `RENAME [COLUMN] column TO name`; a dotted name renames
    /// a struct field.
    pub(super) fn transform_rename_column(&mut self, parse_result: &ParseResult) -> Result<AlterInfo> {
        let list = parse_result.list()?;
        let nested_column: ColumnRefExpression = self.transform_child(list, 2)?;
        let new_column_name = list.identifier(4)?.to_owned();
        if let [old_name] = nested_column.column_names.as_slice() {
            return Ok(alter_table_info(AlterTableInfo::RenameColumn(RenameColumnInfo {
                old_name: old_name.clone(),
                new_name: new_column_name.into(),
            })));
        }
        Ok(alter_table_info(AlterTableInfo::RenameField(RenameFieldInfo {
            column_path: nested_column.column_names,
            new_name: new_column_name.into(),
        })))
    }

    /// `TransformRenameAlter`: `RENAME TO name`.
    pub(super) fn transform_rename_alter(&mut self, parse_result: &ParseResult) -> Result<AlterInfo> {
        let new_table_name = parse_result.list()?.identifier(2)?.to_owned();
        Ok(alter_table_info(AlterTableInfo::RenameTable(RenameTableInfo {
            new_table_name: new_table_name.into(),
        })))
    }

    /// `TransformSetPartitionedBy`: `SET PARTITIONED BY (expr, ...)`.
    pub(super) fn transform_set_partitioned_by(&mut self, parse_result: &ParseResult) -> Result<AlterInfo> {
        let partition_keys = parse_result
            .list()?
            .child(3)?
            .parens_content()?
            .list_elements()?
            .into_iter()
            .map(|expr| self.transform(expr))
            .collect::<Result<Vec<ParsedExpression>>>()?;
        Ok(alter_table_info(AlterTableInfo::SetPartitionedBy(
            SetPartitionedByInfo { partition_keys },
        )))
    }

    /// `TransformResetPartitionedBy`: `RESET PARTITIONED BY`.
    pub(super) fn transform_reset_partitioned_by(
        &mut self,
        _parse_result: &ParseResult,
    ) -> Result<AlterInfo> {
        Ok(alter_table_info(AlterTableInfo::SetPartitionedBy(
            SetPartitionedByInfo {
                partition_keys: Vec::new(),
            },
        )))
    }

    /// `TransformAddConstraint`: `ADD constraint`.
    pub(super) fn transform_add_constraint(&mut self, parse_result: &ParseResult) -> Result<AlterInfo> {
        let constraint: Constraint = self.transform_child(parse_result.list()?, 1)?;
        Ok(alter_table_info(AlterTableInfo::AddConstraint(
            AddConstraintInfo {
                constraint: Box::new(constraint),
            },
        )))
    }

    /// `TransformSequenceName`.
    pub(super) fn transform_sequence_name(&mut self, parse_result: &ParseResult) -> Result<String> {
        Ok(parse_result.list()?.identifier(0)?.to_owned())
    }

    /// `TransformSetSortedBy`: `SET SORTED BY (order, ...)`.
    pub(super) fn transform_set_sorted_by(&mut self, parse_result: &ParseResult) -> Result<AlterInfo> {
        let orders: Vec<OrderByNode> = self.transform(parse_result.list()?.child(3)?.parens_content()?)?;
        Ok(alter_table_info(AlterTableInfo::SetSortedBy(SetSortedByInfo {
            orders,
        })))
    }

    /// `TransformResetSortedBy`: `RESET SORTED BY`.
    pub(super) fn transform_reset_sorted_by(&mut self, _parse_result: &ParseResult) -> Result<AlterInfo> {
        Ok(alter_table_info(AlterTableInfo::SetSortedBy(SetSortedByInfo {
            orders: Vec::new(),
        })))
    }

    /// `TransformSetOptions`: `SET (name = value, ...)`.
    pub(super) fn transform_set_options(&mut self, parse_result: &ParseResult) -> Result<AlterInfo> {
        let table_options: CaseInsensitiveMap<ParsedExpression, ByteString> =
            self.transform_child(parse_result.list()?, 1)?;
        Ok(alter_table_info(AlterTableInfo::SetTableOptions(
            SetTableOptionsInfo { table_options },
        )))
    }

    /// `TransformResetOptions`: `RESET (name, ...)`; the options cannot have values.
    pub(super) fn transform_reset_options(&mut self, parse_result: &ParseResult) -> Result<AlterInfo> {
        let options_map: CaseInsensitiveMap<ParsedExpression, ByteString> =
            self.transform_child(parse_result.list()?, 1)?;
        // A `case_insensitive_set_t`, filled in the iteration order of the options.
        let mut option_names: CiHashMap<(), ByteString> = CiHashMap::default();
        for (name, value) in options_map.iter() {
            let ParsedExpressionKind::Constant(const_expr) = &value.kind else {
                return Err(Error::parser(rustdb_common::byte_format!(
                    "Reset option \"{name}\" cannot set any value. Did you mean to use SET?"
                )));
            };
            if !const_expr.value.is_null() {
                return Err(Error::parser(rustdb_common::byte_format!(
                    "Reset option \"{name}\" cannot set any value. Did you mean to use SET?"
                )));
            }
            option_names.insert(name.to_owned(), ());
        }
        Ok(alter_table_info(AlterTableInfo::ResetTableOptions(
            ResetTableOptionsInfo {
                table_options: option_names
                    .into_ordered()
                    .into_iter()
                    .map(|(name, ())| name)
                    .collect(),
            },
        )))
    }
}
