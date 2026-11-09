//! DDL statements: CREATE TABLE / VIEW / SCHEMA / SEQUENCE / INDEX / MACRO / TYPE /
//! SECRET, DROP, ALTER, RENAME and COMMENT ON (`transform_create_*.cpp`,
//! `transform_drop.cpp`, `transform_alter_*.cpp`, `transform_rename.cpp`,
//! `transform_comment_on.cpp`, `transform_secret.cpp`, `transform_constraint.cpp`).

use roastedduck_ast::{
    AddColumnInfo, AddConstraintInfo, AddFieldInfo, AlterDatabaseInfo, AlterInfo, AlterInfoKind,
    AlterTableInfo, AlterViewInfo, BaseTableRef, CatalogType, ChangeColumnTypeInfo, ChangeOwnershipInfo,
    CheckConstraint, ColumnDefinition, ColumnList, CommonTableExpressionMap, CompressionType, Constraint,
    CreateIndexInfo, CreateInfo, CreateInfoKind, CreateMacroInfo, CreateSecretInfo, CreateSequenceInfo,
    CreateTableInfo, CreateTypeInfo, CreateViewInfo, DropInfo, DropNotNullInfo, ExpressionClass,
    ExpressionType, ExtraDropInfo, ExtraDropSecretInfo, ForeignKeyConstraint, ForeignKeyType,
    IndexConstraintType, LimitModifier, LogicalDependencyList, MacroFunction, MacroFunctionKind,
    NotNullConstraint, OnCreateConflict, OnEntryNotFound, ParsedExpression, ParsedExpressionKind,
    QueryNodeKind, RemoveColumnInfo, RemoveFieldInfo, RenameColumnInfo, RenameDatabaseInfo, RenameFieldInfo,
    RenameTableInfo, RenameViewInfo, ResetTableOptionsInfo, ResultModifier, ScalarMacroFunction,
    SecretPersistType, SelectStatement, SetColumnCommentInfo, SetCommentInfo, SetDefaultInfo, SetNotNullInfo,
    SetPartitionedByInfo, SetSortedByInfo, SetTableOptionsInfo, SqlStatement, SqlStatementKind,
    TableColumnType, TableMacroFunction, TableRefKind, UniqueConstraint, UpdateSetInfo, UpdateStatement,
};
use std::collections::HashSet;

use roastedduck_common::{ByteString, CaseInsensitiveMap, CaseInsensitiveSet, Error, LogicalIndex, Result};
use roastedduck_function::cast::{CastFunctionSet, ValueCast};
use roastedduck_pg_parser::nodes::{
    AConst, AlterDatabaseStmt, AlterDatabaseType, AlterSeqStmt, AlterTableCmd, AlterTableStmt,
    AlterTableType, ColumnCategory, ColumnDef, CommentOnStmt, ConstrType, Constraint as PgConstraint,
    CreateFunctionStmt, CreateSchemaStmt, CreateSecretStmt, CreateSeqStmt, CreateStmt, CreateTableAsStmt,
    CreateTypeStmt, DefElem, DefElemAction, DropBehavior, DropSecretStmt, DropStmt, FunctionDefinition,
    FunctionParameter, IndexElem, IndexStmt, List, NewTypeKind, Node, ObjectType, OnCommitAction, RangeVar,
    RelPersistence, RenameStmt, Value as PgValue, ViewCheckOption, ViewStmt, foreign_key,
};
use roastedduck_types::{LogicalType, LogicalTypeId, TypeParameter, TypeParameterValue, Value};

use super::build;
use super::helpers::{
    QualifiedName, transform_on_conflict, transform_on_entry_not_found, transform_qualified_name,
};
use super::typename::unbound_type;
use super::{Transformer, cast_node, required, res_target_name, string_list};
use crate::equality::has_subquery;
use crate::hash_order::CiHashMap;

/// `DEFAULT_SCHEMA`.
const DEFAULT_SCHEMA: &str = "main";
/// `TEMP_CATALOG`.
const TEMP_CATALOG: &str = "temp";

/// A `CreateInfo` with the base-class defaults (`CreateInfo(type, schema)`).
fn create_info(schema: impl Into<ByteString>, kind: CreateInfoKind) -> CreateInfo {
    CreateInfo {
        catalog: String::new().into(),
        schema: schema.into(),
        temporary: false,
        internal: false,
        on_conflict: OnCreateConflict::ErrorOnConflict,
        sql: ByteString::new(),
        comment: Value::default(),
        tags: CaseInsensitiveMap::default(),
        dependencies: LogicalDependencyList::default(),
        kind,
    }
}

/// `ColumnDefinition(name, type)`.
fn column_definition(name: ByteString, logical_type: LogicalType) -> ColumnDefinition {
    ColumnDefinition {
        name,
        logical_type,
        expression: None,
        category: TableColumnType::Standard,
        compression_type: CompressionType::CompressionAuto,
        comment: Value::default(),
        tags: roastedduck_common::OrderedMap::default(),
    }
}

const fn is_generated(column: &ColumnDefinition) -> bool {
    matches!(column.category, TableColumnType::Generated)
}

/// `ColumnDefinition::HasDefaultValue`.
fn has_default_value(column: &ColumnDefinition) -> bool {
    !is_generated(column) && column.expression.is_some()
}

/// `ColumnDefinition::SetDefaultValue`.
fn set_default_value(column: &mut ColumnDefinition, value: ParsedExpression) -> Result<()> {
    if is_generated(column) {
        return Err(Error::internal("Calling SetDefaultValue() on a generated column"));
    }
    column.expression = Some(Box::new(value));
    Ok(())
}

/// `VerifyColumnRefs`: generated columns cannot use qualified column references.
fn verify_column_refs(expr: &ParsedExpression) -> Result<()> {
    if let ParsedExpressionKind::ColumnRef(column_ref) = &expr.kind
        && column_ref.column_names.len() > 1
    {
        return Err(Error::parser(
            "Qualified (tbl.name) column references are not allowed inside of generated column expressions",
        ));
    }
    let mut result = Ok(());
    let mut copy = expr.clone();
    crate::equality::for_each_child_mut(&mut copy, &mut |child| {
        if result.is_ok() {
            result = verify_column_refs(child);
        }
    });
    result
}

/// `ColumnDefinition::SetGeneratedExpression`.
fn set_generated_expression(column: &mut ColumnDefinition, expr: ParsedExpression) -> Result<()> {
    column.category = TableColumnType::Generated;
    if has_subquery(&expr) {
        return Err(Error::new_bytes(
            roastedduck_common::ErrorKind::Parser,
            [
                b"Expression of generated column \"".as_slice(),
                column.name.as_bytes(),
                b"\" contains a subquery, which isn't allowed",
            ]
            .concat(),
        ));
    }
    verify_column_refs(&expr)?;
    let expression = if column.logical_type.id() == LogicalTypeId::Any {
        expr
    } else {
        build::cast(column.logical_type.clone(), expr, false)
    };
    column.expression = Some(Box::new(expression));
    Ok(())
}

/// A `ColumnList` being built, with its case-insensitive name map.
#[derive(Default)]
struct ColumnListBuilder {
    list: ColumnList,
    names: HashSet<roastedduck_common::ByteString>,
}

impl ColumnListBuilder {
    /// `LogicalColumnCount`.
    fn len(&self) -> usize {
        self.list.columns.len()
    }

    /// `ColumnList::AddColumn` (with `AddToNameMap`): names must be unique.
    fn add(&mut self, column: ColumnDefinition) -> Result<()> {
        if !self.names.insert(column.name.to_ascii_lowercase()) {
            return Err(Error::new_bytes(
                roastedduck_common::ErrorKind::Catalog,
                [
                    b"Column with name ".as_slice(),
                    column.name.as_bytes(),
                    b" already exists!",
                ]
                .concat(),
            ));
        }
        self.list.columns.push(column);
        Ok(())
    }
}

/// `ForeignKeyActionSupported`.
fn foreign_key_action_supported(action: u8) -> bool {
    matches!(
        action,
        foreign_key::ACTION_NOACTION | foreign_key::ACTION_RESTRICT
    )
}

/// `TransformForeignKeyConstraint`.
fn transform_foreign_key_constraint(
    constraint: &PgConstraint,
    override_fk_column: Option<&[u8]>,
) -> Result<Constraint> {
    if !foreign_key_action_supported(constraint.fk_upd_action)
        || !foreign_key_action_supported(constraint.fk_del_action)
    {
        return Err(Error::parser(
            "FOREIGN KEY constraints cannot use CASCADE, SET NULL or SET DEFAULT",
        ));
    }
    let pktable = required(constraint.pktable.as_deref(), "referenced table")?;
    if pktable.catalogname.is_some() {
        return Err(Error::parser(
            "FOREIGN KEY constraints cannot be defined cross-database",
        ));
    }
    let schema = pktable.schemaname.clone().unwrap_or_default();
    let table = pktable.relname.clone().unwrap_or_default();
    let fk_columns = match override_fk_column {
        Some(column) => vec![column.into()],
        None => string_list(&constraint.fk_attrs)?,
    };
    let pk_columns = string_list(&constraint.pk_attrs)?;
    if !pk_columns.is_empty() && pk_columns.len() != fk_columns.len() {
        return Err(Error::parser(
            "The number of referencing and referenced columns for foreign keys must be the same",
        ));
    }
    if fk_columns.is_empty() {
        return Err(Error::parser(
            "The set of referencing and referenced columns for foreign keys must be not empty",
        ));
    }
    Ok(Constraint::ForeignKey(Box::new(ForeignKeyConstraint {
        pk_columns: pk_columns.into_iter().collect(),
        fk_columns: fk_columns.into_iter().collect(),
        fk_type: ForeignKeyType::FkTypeForeignKeyTable,
        schema,
        table,
        pk_keys: Vec::new(),
        fk_keys: Vec::new(),
    })))
}

/// `TransformTypeName` for the parsed element values of `CREATE TYPE ... AS ENUM (...)`:
/// string constants, unique.
fn transform_enum_values(values: &List) -> Result<LogicalType> {
    let mut names: Vec<roastedduck_common::ByteString> = Vec::with_capacity(values.len());
    for node in values {
        let constant: &AConst = cast_node(node)?;
        let PgValue::String(text) = &constant.val else {
            return Err(Error::parser("Expected a string constant as value"));
        };
        names.push(text.clone());
    }
    let mut seen = std::collections::HashSet::with_capacity(names.len());
    for name in &names {
        if !seen.insert(name.as_bytes()) {
            return Err(Error::invalid_input(roastedduck_common::byte_format!(
                "Attempted to create ENUM type with duplicate value {name}"
            )));
        }
    }
    Ok(LogicalType::enum_type_bytes(names))
}

/// `GenerateCreateEnumStmt`'s `CREATE TEMPORARY TYPE name AS ENUM (query)`.
pub(crate) fn create_enum_type_from_query(name: ByteString, query: SelectStatement) -> SqlStatementKind {
    let mut info = create_info(
        "",
        CreateInfoKind::Type(CreateTypeInfo {
            name,
            logical_type: LogicalType::INVALID,
            query: Some(Box::new(query)),
        }),
    );
    info.temporary = true;
    info.on_conflict = OnCreateConflict::ReplaceOnConflict;
    SqlStatementKind::Create(Box::new(info))
}

/// `EnumUtil::FromString<SecretPersistType>`.
fn secret_persist_type(text: &[u8]) -> Result<SecretPersistType> {
    [
        SecretPersistType::Default,
        SecretPersistType::Temporary,
        SecretPersistType::Persistent,
    ]
    .into_iter()
    .find(|mode| mode.name().as_bytes().eq_ignore_ascii_case(text))
    .ok_or_else(|| {
        crate::candidates::enum_error(text, "SecretPersistType", &["DEFAULT", "TEMPORARY", "PERSISTENT"])
    })
}

/// `GetConstantExpressionValue` of CREATE SECRET: a constant's value, a column's name,
/// or NULL.
fn constant_expression_value(expr: &ParsedExpression) -> Result<Value> {
    Ok(match &expr.kind {
        ParsedExpressionKind::Constant(constant) if expr.expression_type == ExpressionType::ValueConstant => {
            constant.value.clone()
        }
        ParsedExpressionKind::ColumnRef(column_ref) if expr.expression_type == ExpressionType::ColumnRef => {
            let name = if expr.alias.is_empty() {
                column_ref.column_names.last().cloned().unwrap_or_default()
            } else {
                expr.alias.clone()
            };
            Value::try_varchar(name.as_bytes())?
        }
        _ => Value::default(),
    })
}

/// An `AlterInfo` on the entry named by `data`.
fn alter_info(data: &AlterEntryData, kind: AlterInfoKind) -> AlterInfo {
    AlterInfo {
        catalog: data.catalog.clone(),
        schema: data.schema.clone(),
        name: data.name.clone(),
        if_not_found: data.if_not_found,
        allow_internal: false,
        kind,
    }
}

/// DuckDB's `AlterEntryData`.
#[derive(Clone, Debug)]
struct AlterEntryData {
    catalog: ByteString,
    schema: ByteString,
    name: ByteString,
    if_not_found: OnEntryNotFound,
}

fn alter_table(data: &AlterEntryData, info: AlterTableInfo) -> AlterInfo {
    alter_info(data, AlterInfoKind::AlterTable(info))
}

fn alter_statement(info: AlterInfo) -> SqlStatement {
    SqlStatement::new(SqlStatementKind::Alter(Box::new(info)))
}

impl<'a> Transformer<'a> {
    /// `TransformTableOptions`: `WITH (name = value, ...)`; with `throw_if_value`, values
    /// are rejected (RESET).
    fn transform_table_options(
        &mut self,
        options: &mut CiHashMap<ParsedExpression, roastedduck_common::ByteString>,
        list: &'a List,
        throw_if_value: bool,
    ) -> Result<()> {
        for node in list {
            let def_elem: &'a DefElem = cast_node(node)?;
            let lower_name = def_elem
                .defname
                .as_deref()
                .unwrap_or_default()
                .to_ascii_lowercase();
            if options.contains_key(&lower_name) {
                return Err(Error::parser(roastedduck_common::byte_format!(
                    "Duplicate table property \"{lower_name}\""
                )));
            }
            let Some(arg) = &def_elem.arg else {
                options.insert(lower_name, build::constant(Value::default()));
                continue;
            };
            let expr = self.transform_expression(arg)?;
            if throw_if_value {
                return Err(Error::parser(roastedduck_common::byte_format!("\"{expr}\"")));
            }
            options.insert(lower_name, expr);
        }
        Ok(())
    }

    /// `TransformColumnDefinition`.
    fn transform_column_definition(&mut self, cdef: &'a ColumnDef) -> Result<ColumnDefinition> {
        let name = cdef.colname.clone().unwrap_or_default();
        let generated = cdef.category == ColumnCategory::Generated;
        let target_type = match (&cdef.type_name, &cdef.coll_clause) {
            (None, _) if generated => LogicalType::ANY,
            (None, _) => LogicalType::UNKNOWN,
            (Some(type_name), Some(collate)) => {
                if generated {
                    return Err(Error::parser("Collations are not supported on generated columns"));
                }
                let mut type_expr = self.transform_type_expression_internal(type_name)?;
                if !type_expr.name.eq_ignore_ascii_case("VARCHAR") {
                    return Err(Error::parser("Only VARCHAR columns can have collations!"));
                }
                let collation = Self::transform_collation(Some(collate))?;
                type_expr.parameters.push(TypeParameter {
                    name: "collation".to_owned().into(),
                    value: TypeParameterValue::Constant(Value::try_varchar(&collation)?),
                    query_location: None,
                });
                unbound_type(type_expr)
            }
            (Some(type_name), None) => self.transform_type_name(type_name)?,
        };
        Ok(column_definition(name, target_type))
    }

    /// `TransformConstraint` of a table constraint.
    fn transform_constraint(&mut self, constraint: &'a PgConstraint) -> Result<Constraint> {
        match constraint.contype {
            ConstrType::Unique | ConstrType::Primary => {
                if constraint.keys.is_empty() {
                    return Err(Error::parser("UNIQUE USING INDEX is not supported"));
                }
                Ok(Constraint::Unique(UniqueConstraint {
                    is_primary_key: constraint.contype == ConstrType::Primary,
                    index: LogicalIndex(u64::MAX),
                    columns: string_list(&constraint.keys)?.into_iter().collect(),
                }))
            }
            ConstrType::Check => {
                let expression = self.transform_required(constraint.raw_expr.as_ref(), "CHECK expression")?;
                if has_subquery(&expression) {
                    return Err(Error::parser("subqueries prohibited in CHECK constraints"));
                }
                Ok(Constraint::Check(CheckConstraint {
                    expression: Box::new(expression),
                }))
            }
            ConstrType::Foreign => transform_foreign_key_constraint(constraint, None),
            _ => Err(Error::not_implemented("Constraint type not handled yet!")),
        }
    }

    /// `TransformConstraint` of a column constraint; defaults, generated expressions and
    /// compression go into the column itself.
    fn transform_column_constraint(
        &mut self,
        constraint: &'a PgConstraint,
        column: &mut ColumnDefinition,
        index: usize,
    ) -> Result<Option<Constraint>> {
        let index = LogicalIndex(index as u64);
        Ok(match constraint.contype {
            ConstrType::NotNull => Some(Constraint::NotNull(NotNullConstraint { index })),
            ConstrType::Check => Some(self.transform_constraint(constraint)?),
            ConstrType::Primary | ConstrType::Unique => Some(Constraint::Unique(UniqueConstraint {
                is_primary_key: constraint.contype == ConstrType::Primary,
                index,
                columns: vec![column.name.as_bytes().to_owned().into()],
            })),
            ConstrType::Null => None,
            ConstrType::GeneratedVirtual => {
                if has_default_value(column) {
                    return Err(Error::new_bytes(
                        roastedduck_common::ErrorKind::InvalidInput,
                        [
                            b"\"".as_slice(),
                            column.name.as_bytes(),
                            b"\" has a DEFAULT value set, it can not become a GENERATED column",
                        ]
                        .concat(),
                    ));
                }
                let expr = self.transform_required(constraint.raw_expr.as_ref(), "generated expression")?;
                set_generated_expression(column, expr)?;
                None
            }
            ConstrType::GeneratedStored => {
                return Err(Error::invalid_input("Can not create a STORED generated column!"));
            }
            ConstrType::Default => {
                let expr = self.transform_required(constraint.raw_expr.as_ref(), "default value")?;
                set_default_value(column, expr)?;
                None
            }
            ConstrType::Compression => {
                let name = constraint.compression_name.as_deref().unwrap_or_default();
                let compression_type = CompressionType::ALL
                    .iter()
                    .copied()
                    .find(|kind| kind.name().as_bytes().eq_ignore_ascii_case(name))
                    .ok_or_else(|| {
                        let names: Vec<&str> =
                            CompressionType::ALL.iter().map(|value| value.name()).collect();
                        crate::candidates::enum_error(name, "CompressionType", &names)
                    })?;
                if matches!(
                    compression_type,
                    CompressionType::CompressionAuto
                        | CompressionType::CompressionConstant
                        | CompressionType::CompressionEmpty
                ) {
                    return Err(Error::invalid_input(roastedduck_common::byte_format!(
                        "Compression method {name} cannot be forced"
                    )));
                }
                column.compression_type = compression_type;
                None
            }
            ConstrType::Foreign => Some(transform_foreign_key_constraint(
                constraint,
                Some(column.name.as_bytes()),
            )?),
            _ => return Err(Error::not_implemented("Constraint not implemented!")),
        })
    }

    /// `TransformCreateTable`.
    pub(crate) fn transform_create_table(&mut self, stmt: &'a CreateStmt) -> Result<SqlStatementKind> {
        if !stmt.inh_relations.is_empty() {
            return Err(Error::not_implemented("inherited relations not implemented"));
        }
        let relation = required(stmt.relation.as_deref(), "table")?;
        let qname = transform_qualified_name(relation)?;
        let temporary = relation.relpersistence == RelPersistence::Temp;
        if temporary && !matches!(stmt.oncommit, OnCommitAction::PreserveRows | OnCommitAction::Noop) {
            return Err(Error::not_implemented(
                "Only ON COMMIT PRESERVE ROWS is supported",
            ));
        }
        if stmt.table_elts.is_empty() {
            return Err(Error::parser("Table must have at least one column!"));
        }
        let mut columns = ColumnListBuilder::default();
        let mut table = CreateTableInfo {
            table: qname.name.clone(),
            columns: ColumnList::default(),
            constraints: Vec::new(),
            query: None,
            partition_keys: Vec::new(),
            sort_keys: Vec::new(),
            options: CaseInsensitiveMap::default(),
        };
        let mut column_count = 0;
        for element in &stmt.table_elts {
            match element {
                Node::ColumnDef(pg_column) => {
                    let mut column = self.transform_column_definition(pg_column)?;
                    for constraint in &pg_column.constraints {
                        let constraint: &'a PgConstraint = cast_node(constraint)?;
                        let index = columns.len();
                        if let Some(constraint) =
                            self.transform_column_constraint(constraint, &mut column, index)?
                        {
                            table.constraints.push(constraint);
                        }
                    }
                    columns.add(column)?;
                    column_count += 1;
                }
                Node::Constraint(constraint) => {
                    let constraint = self.transform_constraint(constraint)?;
                    table.constraints.push(constraint);
                }
                _ => return Err(Error::not_implemented("ColumnDef type not handled yet")),
            }
        }
        table.partition_keys = self.transform_expressions(&stmt.partition_list)?;
        table.sort_keys = self.transform_expressions(&stmt.sort_list)?;
        let mut options = CiHashMap::new_with_keys();
        self.transform_table_options(&mut options, &stmt.options, false)?;
        table.options = options.into_ordered().into_iter().collect();
        if column_count == 0 {
            return Err(Error::parser("Table must have at least one column!"));
        }
        table.columns = columns.list;
        let mut info = create_info("", CreateInfoKind::Table(Box::new(table)));
        info.catalog = qname.catalog;
        info.schema = qname.schema;
        info.on_conflict = transform_on_conflict(stmt.onconflict);
        info.temporary = temporary;
        Ok(SqlStatementKind::Create(Box::new(info)))
    }

    /// `TransformCreateTableAs`.
    pub(crate) fn transform_create_table_as(
        &mut self,
        stmt: &'a CreateTableAsStmt,
    ) -> Result<SqlStatementKind> {
        if stmt.relkind == ObjectType::Matview {
            return Err(Error::not_implemented("Materialized view not implemented"));
        }
        if stmt.is_select_into {
            return Err(Error::not_implemented(
                "Unimplemented features for CREATE TABLE as",
            ));
        }
        let query_node = required(stmt.query.as_ref(), "query")?;
        if !matches!(query_node, Node::SelectStmt(_)) {
            return Err(Error::parser("CREATE TABLE AS requires a SELECT clause"));
        }
        let into = required(stmt.into.as_deref(), "INTO clause")?;
        let relation = required(into.rel.as_deref(), "table")?;
        let qname = transform_qualified_name(relation)?;
        if qname.name.is_empty() {
            return Err(Error::parser("Empty table name not supported"));
        }
        let mut query = self.transform_select_stmt_node(query_node, false)?;
        let partition_keys = self.transform_expressions(&into.partition_list)?;
        let sort_keys = self.transform_expressions(&into.sort_list)?;
        let mut options = CiHashMap::new_with_keys();
        self.transform_table_options(&mut options, &into.options, false)?;
        if into.skip_data {
            // WITH NO DATA: wrap the query in a LIMIT 0.
            let mut limit_node = build::select_node();
            *limit_node.from_table = build::subquery_ref(query);
            limit_node.select_list.push(build::star());
            let mut node = build::query_node(QueryNodeKind::Select(Box::new(limit_node)));
            node.modifiers.push(ResultModifier::Limit(LimitModifier {
                limit: Some(Box::new(build::constant(Value::bigint(0)))),
                offset: None,
            }));
            query = build::select_statement(node);
        }
        let mut columns = ColumnListBuilder::default();
        for name in string_list(&into.col_names)? {
            columns.add(column_definition(name, LogicalType::UNKNOWN))?;
        }
        let table = CreateTableInfo {
            table: qname.name,
            columns: columns.list,
            constraints: Vec::new(),
            query: Some(Box::new(query)),
            partition_keys,
            sort_keys,
            options: options.into_ordered().into_iter().collect(),
        };
        let mut info = create_info("", CreateInfoKind::Table(Box::new(table)));
        info.catalog = qname.catalog;
        info.schema = qname.schema;
        info.on_conflict = transform_on_conflict(stmt.onconflict);
        info.temporary = relation.relpersistence == RelPersistence::Temp;
        Ok(SqlStatementKind::Create(Box::new(info)))
    }

    /// `TransformCreateSchema`.
    pub(crate) fn transform_create_schema(stmt: &CreateSchemaStmt) -> Result<SqlStatementKind> {
        let mut info = create_info(DEFAULT_SCHEMA, CreateInfoKind::Schema);
        info.catalog = stmt.catalogname.clone().unwrap_or_default();
        info.schema = stmt.schemaname.clone().unwrap_or_default();
        info.on_conflict = transform_on_conflict(stmt.onconflict);
        if !stmt.schema_elts.is_empty() {
            return Err(Error::not_implemented("Schema element not supported yet!"));
        }
        Ok(SqlStatementKind::Create(Box::new(info)))
    }

    /// `TransformCreateView`.
    pub(crate) fn transform_create_view(&mut self, stmt: &'a ViewStmt) -> Result<SqlStatementKind> {
        let view = required(stmt.view.as_deref(), "view")?;
        let qname = transform_qualified_name(view)?;
        let temporary = view.relpersistence == RelPersistence::Temp;
        let mut catalog = qname.catalog;
        if temporary && catalog.is_empty() {
            catalog = TEMP_CATALOG.into();
        }
        let on_conflict = transform_on_conflict(stmt.onconflict);
        let query = self.transform_select_stmt_node(required(stmt.query.as_ref(), "view query")?, false)?;
        self.pivot_entry_check("view")?;
        let mut aliases = Vec::with_capacity(stmt.aliases.len());
        for alias in &stmt.aliases {
            match alias {
                Node::Value(PgValue::String(text)) => aliases.push(text.clone()),
                _ => return Err(Error::not_implemented("View projection type")),
            }
        }
        if !stmt.options.is_empty() {
            return Err(Error::not_implemented("VIEW options"));
        }
        if stmt.with_check_option != ViewCheckOption::None {
            return Err(Error::not_implemented("VIEW CHECK options"));
        }
        let view_info = CreateViewInfo {
            view_name: qname.name,
            aliases: aliases.into_iter().collect(),
            types: Vec::new(),
            query: Box::new(query),
            names: Vec::new(),
            column_comments: roastedduck_common::OrderedMap::default(),
        };
        let mut info = create_info("", CreateInfoKind::View(Box::new(view_info)));
        info.catalog = catalog;
        info.schema = qname.schema;
        info.temporary = temporary;
        info.on_conflict = on_conflict;
        Ok(SqlStatementKind::Create(Box::new(info)))
    }

    /// `TransformCreateSequence`.
    pub(crate) fn transform_create_sequence(stmt: &CreateSeqStmt) -> Result<SqlStatementKind> {
        let sequence_var = required(stmt.sequence.as_deref(), "sequence")?;
        let qname = transform_qualified_name(sequence_var)?;
        let mut sequence = CreateSequenceInfo {
            name: qname.name,
            usage_count: 0,
            increment: 1,
            min_value: 1,
            max_value: i64::MAX,
            start_value: 1,
            cycle: false,
        };
        if !stmt.options.is_empty() {
            let mut default_start_value = sequence.start_value;
            let mut has_start_value = false;
            let mut used: Vec<&str> = Vec::new();
            for node in &stmt.options {
                let def_elem: &DefElem = cast_node(node)?;
                let option_name = def_elem.defname.as_deref().unwrap_or_default();
                let value = def_elem.arg.as_ref();
                let nodef = def_elem.defaction == DefElemAction::Unspec && value.is_none();
                let mut option_value: i64 = 0;
                match value {
                    None => {}
                    Some(Node::Value(PgValue::Integer(integer))) => option_value = *integer,
                    Some(Node::Value(PgValue::Float(text))) => {
                        option_value = Value::try_varchar(text)?
                            .try_cast_as(&CastFunctionSet::new(), &LogicalType::BIGINT, true)
                            .ok()
                            .and_then(|value| value.as_i128())
                            .and_then(|value| i64::try_from(value).ok())
                            .ok_or_else(|| {
                                Error::parser(roastedduck_common::byte_format!(
                                    "Expected an integer argument for option {option_name}"
                                ))
                            })?;
                    }
                    Some(_) => {
                        return Err(Error::parser(roastedduck_common::byte_format!(
                            "Expected an integer argument for option {option_name}"
                        )));
                    }
                }
                let (key, label) = match option_name {
                    b"increment" => ("increment", "Increment"),
                    b"minvalue" => ("minvalue", "Minvalue"),
                    b"maxvalue" => ("maxvalue", "Maxvalue"),
                    b"start" => ("start", "Start"),
                    b"cycle" => ("cycle", "Cycle"),
                    _ => {
                        return Err(Error::parser(roastedduck_common::byte_format!(
                            "Unrecognized option \"{option_name}\" for CREATE SEQUENCE"
                        )));
                    }
                };
                if used.contains(&key) {
                    return Err(Error::parser(roastedduck_common::byte_format!(
                        "{label} should be passed at most once"
                    )));
                }
                used.push(key);
                if nodef {
                    continue;
                }
                match key {
                    "increment" => {
                        sequence.increment = option_value;
                        if option_value == 0 {
                            return Err(Error::parser("Increment must not be zero"));
                        }
                        if option_value < 0 {
                            sequence.max_value = -1;
                            default_start_value = -1;
                            sequence.min_value = i64::MIN;
                        } else {
                            sequence.min_value = 1;
                            default_start_value = 1;
                            sequence.max_value = i64::MAX;
                        }
                    }
                    "minvalue" => {
                        sequence.min_value = option_value;
                        if sequence.increment > 0 {
                            default_start_value = option_value;
                        }
                    }
                    "maxvalue" => {
                        sequence.max_value = option_value;
                        if sequence.increment < 0 {
                            default_start_value = option_value;
                        }
                    }
                    "start" => {
                        has_start_value = true;
                        sequence.start_value = option_value;
                    }
                    _ => sequence.cycle = option_value > 0,
                }
            }
            if !has_start_value {
                sequence.start_value = default_start_value;
            }
        }
        let temporary = sequence_var.relpersistence == RelPersistence::Temp;
        if sequence.max_value <= sequence.min_value {
            return Err(Error::parser(roastedduck_common::byte_format!(
                "MINVALUE ({}) must be less than MAXVALUE ({})",
                sequence.min_value,
                sequence.max_value
            )));
        }
        if sequence.start_value < sequence.min_value {
            return Err(Error::parser(roastedduck_common::byte_format!(
                "START value ({}) cannot be less than MINVALUE ({})",
                sequence.start_value,
                sequence.min_value
            )));
        }
        if sequence.start_value > sequence.max_value {
            return Err(Error::parser(roastedduck_common::byte_format!(
                "START value ({}) cannot be greater than MAXVALUE ({})",
                sequence.start_value,
                sequence.max_value
            )));
        }
        let mut info = create_info("", CreateInfoKind::Sequence(Box::new(sequence)));
        info.catalog = qname.catalog;
        info.schema = qname.schema;
        info.temporary = temporary;
        info.on_conflict = transform_on_conflict(stmt.onconflict);
        Ok(SqlStatementKind::Create(Box::new(info)))
    }

    /// `TransformIndexParameters`.
    fn transform_index_parameters(
        &mut self,
        list: &'a List,
        relation_name: &roastedduck_common::ByteString,
    ) -> Result<Vec<ParsedExpression>> {
        let mut expressions = Vec::with_capacity(list.len());
        for node in list {
            let element: &'a IndexElem = cast_node(node)?;
            if !element.collation.is_empty() {
                return Err(Error::not_implemented("Index with collation not supported yet!"));
            }
            if !element.opclass.is_empty() {
                return Err(Error::not_implemented("Index with opclass not supported yet!"));
            }
            if let Some(name) = &element.name {
                expressions.push(build::qualified_column_ref(
                    name.clone(),
                    relation_name.to_owned(),
                ));
            } else {
                expressions.push(self.transform_required(element.expr.as_ref(), "index expression")?);
            }
        }
        Ok(expressions)
    }

    /// `TransformCreateIndex`.
    pub(crate) fn transform_create_index(&mut self, stmt: &'a IndexStmt) -> Result<SqlStatementKind> {
        let constraint_type = if stmt.unique {
            IndexConstraintType::Unique
        } else {
            IndexConstraintType::None
        };
        let on_conflict = transform_on_conflict(stmt.onconflict);
        let relation = required(stmt.relation.as_deref(), "table")?;
        let relation_name = relation.relname.clone().unwrap_or_default();
        let expressions = self.transform_index_parameters(&stmt.index_params, &relation_name)?;
        let index_type_name = stmt
            .access_method
            .as_deref()
            .unwrap_or_default()
            .to_ascii_uppercase();
        let Some(index_name) = stmt.idxname.clone() else {
            return Err(Error::not_implemented(
                "Please provide an index name, e.g., CREATE INDEX my_name ...",
            ));
        };
        let mut options: CiHashMap<Value, ByteString> = CiHashMap::new_with_keys();
        for node in &stmt.options {
            let def_elem: &'a DefElem = cast_node(node)?;
            let value = match &def_elem.arg {
                Some(arg) => {
                    let expr = self.transform_expression(arg)?;
                    let ParsedExpressionKind::Constant(constant) = expr.kind else {
                        return Err(Error::invalid_input(
                            "Create index option must be a constant value",
                        ));
                    };
                    constant.value
                }
                None => Value::boolean(true),
            };
            options.set(
                def_elem
                    .defname
                    .as_deref()
                    .unwrap_or_default()
                    .to_ascii_lowercase(),
                value,
            );
        }
        if stmt.where_clause.is_some() {
            return Err(Error::not_implemented(
                "Creating partial indexes is not supported currently",
            ));
        }
        let index = CreateIndexInfo {
            name: index_name,
            table: relation_name,
            constraint_type,
            parsed_expressions: expressions,
            scan_types: Vec::new(),
            names: Vec::new(),
            column_ids: Vec::new(),
            options: options.into_ordered().into_iter().collect(),
            index_type_name: index_type_name.into(),
        };
        let mut info = create_info("", CreateInfoKind::Index(Box::new(index)));
        info.on_conflict = on_conflict;
        info.schema = relation.schemaname.clone().unwrap_or_default();
        info.catalog = relation.catalogname.clone().unwrap_or_default();
        Ok(SqlStatementKind::Create(Box::new(info)))
    }

    /// `TransformMacroFunction`: one `AS expr` / `AS TABLE query` with its parameters.
    fn transform_macro_function(&mut self, def: &'a FunctionDefinition) -> Result<MacroFunction> {
        let kind = if let Some(function) = &def.function {
            MacroFunctionKind::Scalar(ScalarMacroFunction {
                expression: Box::new(self.transform_expression(function)?),
            })
        } else if let Some(query) = &def.query {
            MacroFunctionKind::Table(TableMacroFunction {
                query_node: Box::new(self.transform_select_node(query, true)?),
            })
        } else {
            return Err(Error::internal("Macro definition without a body"));
        };
        let mut macro_function = MacroFunction {
            parameters: Vec::new(),
            default_parameters: CaseInsensitiveMap::default(),
            types: Vec::new(),
            kind,
        };
        let mut parameter_names = CaseInsensitiveSet::<ByteString>::default();
        let mut defaults: CiHashMap<ParsedExpression, roastedduck_common::ByteString> =
            CiHashMap::new_with_keys();
        for node in &def.params {
            let Node::FunctionParameter(param) = node else {
                return Err(Error::internal("TODO"));
            };
            let param: &'a FunctionParameter = param;
            let name = param.name.clone().unwrap_or_default();
            if !parameter_names.insert(name.clone()) {
                return Err(Error::parser(roastedduck_common::byte_format!(
                    "Duplicate parameter '{name}' in macro definition"
                )));
            }
            macro_function
                .parameters
                .push(build::column_ref(vec![name.clone()]));
            macro_function.types.push(match &param.type_name {
                Some(type_name) => self.transform_type_name(type_name)?,
                None => LogicalType::UNKNOWN,
            });
            if let Some(default_value) = &param.default_value {
                let mut default_expr = self.transform_expression(default_value)?;
                default_expr.alias = name.clone();
                defaults.set(name, default_expr);
            } else if !defaults.is_empty() {
                return Err(Error::parser(
                    "Parameter without a default follows parameter with a default",
                ));
            }
        }
        macro_function.default_parameters = defaults.into_ordered().into_iter().collect();
        Ok(macro_function)
    }

    /// `TransformCreateFunction`: `CREATE MACRO`.
    pub(crate) fn transform_create_function(
        &mut self,
        stmt: &'a CreateFunctionStmt,
    ) -> Result<SqlStatementKind> {
        let name = required(stmt.name.as_deref(), "macro name")?;
        let qname = transform_qualified_name(name)?;
        let mut macros = Vec::with_capacity(stmt.functions.len());
        for node in &stmt.functions {
            let def: &'a FunctionDefinition = cast_node(node)?;
            macros.push(self.transform_macro_function(def)?);
        }
        self.pivot_entry_check("macro")?;
        let is_scalar = matches!(
            macros.first().map(|m| &m.kind),
            Some(MacroFunctionKind::Scalar(_))
        );
        let temporary = match name.relpersistence {
            RelPersistence::Temp => true,
            RelPersistence::Unlogged => {
                return Err(Error::parser(roastedduck_common::byte_format!(
                    "Unlogged flag not supported for macros: '{}'",
                    qname.name
                )));
            }
            RelPersistence::Permanent => false,
        };
        let macro_info = CreateMacroInfo {
            name: qname.name,
            macros,
        };
        let kind = if is_scalar {
            CreateInfoKind::Macro(macro_info)
        } else {
            CreateInfoKind::TableMacro(macro_info)
        };
        let mut info = create_info("", kind);
        info.catalog = qname.catalog;
        info.schema = qname.schema;
        info.temporary = temporary;
        info.on_conflict = transform_on_conflict(stmt.onconflict);
        Ok(SqlStatementKind::Create(Box::new(info)))
    }

    /// `TransformCreateType`: `CREATE TYPE name AS ENUM (...)` / `AS type`.
    pub(crate) fn transform_create_type(&mut self, stmt: &'a CreateTypeStmt) -> Result<SqlStatementKind> {
        let type_name = required(stmt.type_name.as_deref(), "type name")?;
        let qname = transform_qualified_name(type_name)?;
        let (logical_type, query) = match stmt.kind {
            NewTypeKind::Enum => {
                if let Some(query) = &stmt.query {
                    let query = self.transform_select_stmt_node(query, false)?;
                    (LogicalType::INVALID, Some(Box::new(query)))
                } else {
                    (transform_enum_values(&stmt.vals)?, None)
                }
            }
            NewTypeKind::Alias => {
                let of_type = required(stmt.of_type.as_deref(), "type")?;
                (self.transform_type_name(of_type)?, None)
            }
            NewTypeKind::None => return Err(Error::internal("Unknown kind of new type")),
        };
        let mut info = create_info(
            DEFAULT_SCHEMA,
            CreateInfoKind::Type(CreateTypeInfo {
                name: qname.name,
                logical_type,
                query,
            }),
        );
        info.catalog = qname.catalog;
        info.schema = qname.schema;
        info.temporary = type_name.relpersistence == RelPersistence::Temp;
        info.on_conflict = transform_on_conflict(stmt.onconflict);
        Ok(SqlStatementKind::Create(Box::new(info)))
    }

    /// `TransformCreateSecretOptions`.
    fn transform_create_secret_options(
        &mut self,
        info: &mut CreateSecretInfo,
        options: &'a List,
    ) -> Result<()> {
        let mut generic: CiHashMap<ParsedExpression, roastedduck_common::ByteString> =
            CiHashMap::new_with_keys();
        for node in options {
            let def_elem: &'a DefElem = cast_node(node)?;
            let lower_name = def_elem
                .defname
                .as_deref()
                .unwrap_or_default()
                .to_ascii_lowercase();
            let arg = def_elem.arg.as_ref();
            match lower_name.as_slice() {
                b"scope" => info.scope = self.transform_opt_boxed(arg)?,
                b"type" | b"provider" => {
                    let mut expr = self.transform_required(arg, "secret option")?;
                    if expr.expression_type == ExpressionType::ColumnRef {
                        expr = build::constant(constant_expression_value(&expr)?);
                    }
                    if lower_name == b"type" {
                        info.secret_type = Some(Box::new(expr));
                    } else {
                        info.provider = Some(Box::new(expr));
                    }
                }
                _ => {
                    if generic.contains_key(&lower_name) {
                        return Err(Error::binder(roastedduck_common::byte_format!(
                            "Duplicate query param found while parsing create secret: '{lower_name}'"
                        )));
                    }
                    let mut expr = self.transform_required(arg, "secret option")?;
                    if expr.expression_type == ExpressionType::ColumnRef {
                        expr = build::constant(constant_expression_value(&expr)?);
                    }
                    generic.set(lower_name, expr);
                }
            }
        }
        info.options = generic.into_ordered().into_iter().collect();
        Ok(())
    }

    /// `TransformSecret`: `CREATE SECRET`.
    pub(crate) fn transform_secret(&mut self, stmt: &'a CreateSecretStmt) -> Result<SqlStatementKind> {
        let on_conflict = transform_on_conflict(stmt.onconflict);
        let persist_type = secret_persist_type(
            &stmt
                .persist_type
                .as_deref()
                .unwrap_or_default()
                .to_ascii_uppercase(),
        )?;
        let mut secret = CreateSecretInfo {
            persist_type,
            storage_type: ByteString::new(),
            name: String::new().into(),
            scope: None,
            secret_type: None,
            provider: None,
            options: CaseInsensitiveMap::default(),
        };
        if let Some(name) = &stmt.secret_name {
            secret.name = name.to_ascii_lowercase();
        }
        if let Some(storage) = &stmt.secret_storage {
            secret.storage_type = storage.to_ascii_lowercase();
        }
        self.transform_create_secret_options(&mut secret, &stmt.options)?;
        let Some(secret_type) = &secret.secret_type else {
            return Err(Error::parser(
                "Failed to create secret - secret must have a type defined",
            ));
        };
        if secret.name.is_empty() {
            let value = constant_expression_value(secret_type)?;
            if value.is_null() {
                return Err(Error::invalid_input(
                    "Can not combine a non-constant expression for the secret type with a default-named secret. \
                     Either provide an explicit secret name or use a constant expression for the secret type.",
                ));
            }
            secret.name = roastedduck_common::byte_format!(
                "__default_{}",
                value.to_varchar_bytes()?.to_ascii_lowercase()
            );
        }
        let mut info = create_info(DEFAULT_SCHEMA, CreateInfoKind::Secret(Box::new(secret)));
        info.on_conflict = on_conflict;
        Ok(SqlStatementKind::Create(Box::new(info)))
    }

    /// `TransformDrop`.
    pub(crate) fn transform_drop(stmt: &DropStmt) -> Result<SqlStatementKind> {
        if stmt.objects.len() != 1 {
            return Err(Error::not_implemented("Can only drop one object at a time"));
        }
        let catalog_type = match stmt.remove_type {
            ObjectType::Table => CatalogType::TableEntry,
            ObjectType::Schema => CatalogType::SchemaEntry,
            ObjectType::Index => CatalogType::IndexEntry,
            ObjectType::View => CatalogType::ViewEntry,
            ObjectType::Sequence => CatalogType::SequenceEntry,
            ObjectType::Function => CatalogType::MacroEntry,
            ObjectType::TableMacro => CatalogType::TableMacroEntry,
            ObjectType::Type => CatalogType::TypeEntry,
            _ => return Err(Error::not_implemented("Cannot drop this type yet")),
        };
        let mut info = DropInfo {
            catalog_type,
            catalog: String::new().into(),
            schema: String::new().into(),
            name: String::new().into(),
            if_not_found: OnEntryNotFound::ThrowException,
            cascade: false,
            allow_drop_internal: false,
            extra_drop_info: None,
        };
        let Some(Node::List(names)) = stmt.objects.first() else {
            return Err(Error::internal("DROP object is not a name list"));
        };
        let names = string_list(names)?;
        if stmt.remove_type == ObjectType::Schema {
            match names.as_slice() {
                [catalog, name] => {
                    info.catalog = catalog.into();
                    info.name = name.into();
                }
                [name] => info.name = name.into(),
                _ => return Err(Error::parser("Expected \"catalog.schema\" or \"schema\"")),
            }
        } else {
            match names.as_slice() {
                [catalog, schema, name] => {
                    info.catalog = catalog.into();
                    info.schema = schema.into();
                    info.name = name.into();
                }
                [schema, name] => {
                    info.schema = schema.into();
                    info.name = name.into();
                }
                [name] => info.name = name.into(),
                _ => {
                    return Err(Error::parser(
                        "Expected \"catalog.schema.name\", \"schema.name\"or \"name\"",
                    ));
                }
            }
        }
        info.cascade = stmt.behavior == DropBehavior::Cascade;
        info.if_not_found = transform_on_entry_not_found(stmt.missing_ok);
        Ok(SqlStatementKind::Drop(Box::new(info)))
    }

    /// `TransformDropSecret`.
    pub(crate) fn transform_drop_secret(stmt: &DropSecretStmt) -> Result<SqlStatementKind> {
        let persist_mode = secret_persist_type(
            &stmt
                .persist_type
                .as_deref()
                .unwrap_or_default()
                .to_ascii_uppercase(),
        )?;
        let secret_storage = stmt.secret_storage.clone().unwrap_or_default();
        if persist_mode == SecretPersistType::Temporary && !secret_storage.is_empty() {
            return Err(Error::parser(
                "Can not combine TEMPORARY with specifying a storage for drop secret",
            ));
        }
        let info = DropInfo {
            catalog_type: CatalogType::SecretEntry,
            catalog: String::new().into(),
            schema: String::new().into(),
            name: stmt.secret_name.clone().unwrap_or_default(),
            if_not_found: if stmt.missing_ok {
                OnEntryNotFound::ReturnNull
            } else {
                OnEntryNotFound::ThrowException
            },
            cascade: false,
            allow_drop_internal: false,
            extra_drop_info: Some(Box::new(ExtraDropInfo::Secret(ExtraDropSecretInfo {
                persist_mode,
                secret_storage,
            }))),
        };
        Ok(SqlStatementKind::Drop(Box::new(info)))
    }

    /// `TransformAlter`: `ALTER TABLE` / `ALTER VIEW`.
    pub(crate) fn transform_alter(&mut self, stmt: &'a AlterTableStmt) -> Result<SqlStatementKind> {
        let relation = required(stmt.relation.as_deref(), "table")?;
        let [command] = stmt.cmds.as_slice() else {
            return Err(Error::parser("Only one ALTER command per statement is supported"));
        };
        let qname = transform_qualified_name(relation)?;
        let command: &'a AlterTableCmd = cast_node(command)?;
        let data = AlterEntryData {
            catalog: qname.catalog,
            schema: qname.schema,
            name: qname.name,
            if_not_found: transform_on_entry_not_found(stmt.missing_ok),
        };
        let is_table = stmt.relkind == ObjectType::Table;
        let info = match command.subtype {
            AlterTableType::AddColumn => {
                let (Some(Node::List(name_list)), Some(column_def)) =
                    (command.def_list.first(), command.def_list.last())
                else {
                    return Err(Error::internal("ADD COLUMN without a column"));
                };
                let column_def: &'a ColumnDef = cast_node(column_def)?;
                if !is_table {
                    return Err(Error::parser("Adding columns is only supported for tables"));
                }
                if column_def.category == ColumnCategory::Generated {
                    return Err(Error::parser(
                        "Adding generated columns after table creation is not supported yet",
                    ));
                }
                let mut column = self.transform_column_definition(column_def)?;
                for constraint in &column_def.constraints {
                    let constraint: &'a PgConstraint = cast_node(constraint)?;
                    if self
                        .transform_column_constraint(constraint, &mut column, 0)?
                        .is_some()
                    {
                        return Err(Error::parser("Adding columns with constraints not yet supported"));
                    }
                }
                let mut names = string_list(name_list)?;
                let Some(last) = names.pop() else {
                    return Err(Error::internal("Expected a name"));
                };
                column.name = last;
                if !names.is_empty() {
                    alter_table(
                        &data,
                        AlterTableInfo::AddField(Box::new(AddFieldInfo {
                            new_field: column,
                            if_field_not_exists: command.missing_ok,
                            column_path: names.into_iter().collect(),
                        })),
                    )
                } else if command.missing_ok
                    || !has_default_value(&column)
                    || column
                        .expression
                        .as_ref()
                        .is_some_and(|expr| expr.expression_class() == ExpressionClass::Constant)
                {
                    alter_table(
                        &data,
                        AlterTableInfo::AddColumn(Box::new(AddColumnInfo {
                            new_column: column,
                            if_column_not_exists: command.missing_ok,
                        })),
                    )
                } else {
                    return Ok(transform_and_materialize_alter(&data, column, command.missing_ok));
                }
            }
            AlterTableType::DropColumn => {
                let cascade = command.behavior == DropBehavior::Cascade;
                if !is_table {
                    return Err(Error::parser("Dropping columns is only supported for tables"));
                }
                let names = string_list(&command.def_list)?;
                match names.as_slice() {
                    [] => return Err(Error::internal("Expected a name")),
                    [name] => alter_table(
                        &data,
                        AlterTableInfo::RemoveColumn(RemoveColumnInfo {
                            removed_column: name.clone(),
                            if_column_exists: command.missing_ok,
                            cascade,
                        }),
                    ),
                    _ => alter_table(
                        &data,
                        AlterTableInfo::RemoveField(RemoveFieldInfo {
                            column_path: names.into_iter().collect(),
                            if_column_exists: command.missing_ok,
                            cascade,
                        }),
                    ),
                }
            }
            AlterTableType::ColumnDefault => {
                let expression = self.transform_opt_boxed(command.def.as_ref())?;
                if !is_table {
                    return Err(Error::parser(
                        "Alter column's default is only supported for tables",
                    ));
                }
                alter_table(
                    &data,
                    AlterTableInfo::SetDefault(SetDefaultInfo {
                        column_name: command.name.clone().unwrap_or_default(),
                        expression,
                    }),
                )
            }
            AlterTableType::AlterColumnType => {
                let column_def: &'a ColumnDef = cast_node(required(command.def.as_ref(), "column")?)?;
                let column = self.transform_column_definition(column_def)?;
                if !is_table {
                    return Err(Error::parser("Alter column's type is only supported for tables"));
                }
                if column.logical_type == LogicalType::UNKNOWN && column_def.raw_default.is_none() {
                    return Err(Error::parser(
                        "Omitting the type is only possible in combination with USING",
                    ));
                }
                let column_name = command.name.clone().unwrap_or_default();
                let expression = match &column_def.raw_default {
                    Some(raw_default) => self.transform_expression(raw_default)?,
                    None => build::cast(
                        column.logical_type.clone(),
                        build::column_ref(vec![column_name.clone()]),
                        false,
                    ),
                };
                alter_table(
                    &data,
                    AlterTableInfo::AlterColumnType(ChangeColumnTypeInfo {
                        column_name,
                        target_type: column.logical_type,
                        expression: Some(Box::new(expression)),
                    }),
                )
            }
            AlterTableType::SetNotNull => alter_table(
                &data,
                AlterTableInfo::SetNotNull(SetNotNullInfo {
                    column_name: command.name.clone().unwrap_or_default(),
                }),
            ),
            AlterTableType::DropNotNull => alter_table(
                &data,
                AlterTableInfo::DropNotNull(DropNotNullInfo {
                    column_name: command.name.clone().unwrap_or_default(),
                }),
            ),
            AlterTableType::AddConstraint => {
                let constraint: &'a PgConstraint = cast_node(required(command.def.as_ref(), "constraint")?)?;
                if constraint.contype != ConstrType::Primary {
                    return Err(Error::not_implemented(
                        "No support for that ALTER TABLE option yet!",
                    ));
                }
                let constraint = self.transform_constraint(constraint)?;
                alter_table(
                    &data,
                    AlterTableInfo::AddConstraint(AddConstraintInfo {
                        constraint: Box::new(constraint),
                    }),
                )
            }
            AlterTableType::SetPartitionedBy => {
                let partition_keys = self.transform_expressions(&command.def_list)?;
                alter_table(
                    &data,
                    AlterTableInfo::SetPartitionedBy(SetPartitionedByInfo { partition_keys }),
                )
            }
            AlterTableType::SetSortedBy => {
                let mut orders = Vec::new();
                self.transform_order_by(&command.def_list, &mut orders)?;
                alter_table(&data, AlterTableInfo::SetSortedBy(SetSortedByInfo { orders }))
            }
            AlterTableType::SetRelOptions => {
                let mut options = CiHashMap::new_with_keys();
                self.transform_table_options(&mut options, &command.options, false)?;
                alter_table(
                    &data,
                    AlterTableInfo::SetTableOptions(SetTableOptionsInfo {
                        table_options: options.into_ordered().into_iter().collect(),
                    }),
                )
            }
            AlterTableType::ResetRelOptions => {
                let mut options = CiHashMap::new_with_keys();
                self.transform_table_options(&mut options, &command.options, true)?;
                let mut names: CiHashMap<(), ByteString> = CiHashMap::new_with_keys();
                for (name, _) in options.iter() {
                    names.insert(name.to_owned(), ());
                }
                alter_table(
                    &data,
                    AlterTableInfo::ResetTableOptions(ResetTableOptionsInfo {
                        table_options: names.into_ordered().into_iter().map(|(name, ())| name).collect(),
                    }),
                )
            }
            _ => {
                return Err(Error::not_implemented(
                    "No support for that ALTER TABLE option yet!",
                ));
            }
        };
        Ok(SqlStatementKind::Alter(Box::new(info)))
    }

    /// `TransformAlterDatabase`.
    pub(crate) fn transform_alter_database(stmt: &AlterDatabaseStmt) -> Result<SqlStatementKind> {
        let database_name = stmt.dbname.clone().unwrap_or_default();
        if database_name.is_empty() {
            return Err(Error::parser("ALTER DATABASE requires a database name"));
        }
        let if_not_found = transform_on_entry_not_found(stmt.missing_ok);
        match stmt.alter_type {
            AlterDatabaseType::Rename => {
                let Some(new_name) = stmt.new_name.clone() else {
                    return Err(Error::parser("ALTER DATABASE RENAME requires a new name"));
                };
                let data = AlterEntryData {
                    catalog: database_name,
                    schema: String::new().into(),
                    name: String::new().into(),
                    if_not_found,
                };
                Ok(SqlStatementKind::Alter(Box::new(alter_info(
                    &data,
                    AlterInfoKind::AlterDatabase(AlterDatabaseInfo::RenameDatabase(RenameDatabaseInfo {
                        new_name,
                    })),
                ))))
            }
        }
    }

    /// `TransformAlterSequence`: `ALTER SEQUENCE ... OWNED BY ...`.
    pub(crate) fn transform_alter_sequence(stmt: &AlterSeqStmt) -> Result<SqlStatementKind> {
        let qname = transform_qualified_name(required(stmt.sequence.as_deref(), "sequence")?)?;
        if stmt.options.is_empty() {
            return Err(Error::internal("Expected an argument for ALTER SEQUENCE."));
        }
        let if_not_found = transform_on_entry_not_found(stmt.missing_ok);
        let mut info = None;
        let mut owned_by_seen = false;
        for node in &stmt.options {
            let def_elem: &DefElem = cast_node(node)?;
            let option_name = def_elem.defname.as_deref().unwrap_or_default();
            if option_name != b"owned_by" {
                return Err(Error::not_implemented("ALTER SEQUENCE option not supported yet!"));
            }
            if owned_by_seen {
                return Err(Error::parser("Owned by value should be passed at most once"));
            }
            owned_by_seen = true;
            let Some(arg) = &def_elem.arg else {
                return Err(Error::internal(roastedduck_common::byte_format!(
                    "Expected an argument for option {option_name}"
                )));
            };
            let Node::List(values) = arg else {
                return Err(Error::internal(roastedduck_common::byte_format!(
                    "Expected a string argument for option {option_name}"
                )));
            };
            let mut names = Vec::with_capacity(values.len());
            for value in values {
                names.push(res_target_name(value)?.unwrap_or_default());
            }
            let (owner_schema, owner_name) = match names.as_slice() {
                [schema, name] => (schema.clone(), name.clone()),
                [name] => (DEFAULT_SCHEMA.into(), name.clone()),
                _ => {
                    return Err(Error::internal(roastedduck_common::byte_format!(
                        "Wrong argument for {option_name}. Expected either <schema>.<name> or <name>"
                    )));
                }
            };
            let data = AlterEntryData {
                catalog: qname.catalog.clone(),
                schema: qname.schema.clone(),
                name: qname.name.clone(),
                if_not_found,
            };
            info = Some(alter_info(
                &data,
                AlterInfoKind::ChangeOwnership(ChangeOwnershipInfo {
                    entry_catalog_type: CatalogType::SequenceEntry,
                    owner_schema,
                    owner_name,
                }),
            ));
        }
        let info = info.ok_or_else(|| Error::internal("ALTER SEQUENCE without an option"))?;
        Ok(SqlStatementKind::Alter(Box::new(info)))
    }

    /// `TransformRename`: `ALTER TABLE ... RENAME ...`, `ALTER VIEW ... RENAME TO ...`.
    pub(crate) fn transform_rename(stmt: &RenameStmt) -> Result<SqlStatementKind> {
        let Some(relation) = stmt.relation.as_deref() else {
            return Err(Error::not_implemented("Altering schemas is not yet supported"));
        };
        let relation: &RangeVar = relation;
        let data = AlterEntryData {
            catalog: relation.catalogname.clone().unwrap_or_default(),
            schema: relation.schemaname.clone().unwrap_or_default(),
            name: relation.relname.clone().unwrap_or_default(),
            if_not_found: transform_on_entry_not_found(stmt.missing_ok),
        };
        let new_name = stmt.newname.clone().unwrap_or_default();
        let info = match stmt.rename_type {
            ObjectType::Column => {
                let mut names = string_list(&stmt.name_list)?;
                if names.len() == 1 {
                    alter_table(
                        &data,
                        AlterTableInfo::RenameColumn(RenameColumnInfo {
                            old_name: names.remove(0),
                            new_name,
                        }),
                    )
                } else {
                    alter_table(
                        &data,
                        AlterTableInfo::RenameField(RenameFieldInfo {
                            column_path: names.into_iter().collect(),
                            new_name,
                        }),
                    )
                }
            }
            ObjectType::Table => alter_table(
                &data,
                AlterTableInfo::RenameTable(RenameTableInfo {
                    new_table_name: new_name,
                }),
            ),
            ObjectType::View => alter_info(
                &data,
                AlterInfoKind::AlterView(AlterViewInfo::RenameView(RenameViewInfo {
                    new_view_name: new_name,
                })),
            ),
            _ => return Err(Error::not_implemented("Schema element not supported yet!")),
        };
        Ok(SqlStatementKind::Alter(Box::new(info)))
    }

    /// `TransformCommentOn`.
    pub(crate) fn transform_comment_on(&mut self, stmt: &'a CommentOnStmt) -> Result<SqlStatementKind> {
        let mut qualified_name: QualifiedName<ByteString> = QualifiedName::default();
        let mut column_name = ByteString::new();
        if stmt.object_type == ObjectType::Column {
            let expr = self.transform_required(stmt.column_expr.as_ref(), "column")?;
            let ParsedExpressionKind::ColumnRef(column_ref) = &expr.kind else {
                return Err(Error::parser(roastedduck_common::byte_format!(
                    "Unexpected expression found, expected column reference to comment on (e.g. \
                     'schema.table.column'), found '{expr}'"
                )));
            };
            let names = &column_ref.column_names;
            if names.len() > 4 {
                return Err(Error::parser(roastedduck_common::byte_format!(
                    "Invalid column reference: '{expr}', too many dots"
                )));
            }
            if names.len() < 2 {
                return Err(Error::parser(roastedduck_common::byte_format!(
                    "Invalid column reference: '{expr}', please specify a table"
                )));
            }
            column_name.clone_from(&names[names.len() - 1]);
            qualified_name.name.clone_from(&names[names.len() - 2]);
            if names.len() == 4 {
                qualified_name.catalog.clone_from(&names[0]);
                qualified_name.schema.clone_from(&names[1]);
            } else if names.len() == 3 {
                qualified_name.schema.clone_from(&names[0]);
            }
        } else {
            let text_name = transform_qualified_name(required(stmt.name.as_deref(), "object name")?)?;
            qualified_name = QualifiedName {
                catalog: text_name.catalog,
                schema: text_name.schema,
                name: text_name.name,
            };
        }
        let value = self.transform_required(stmt.value.as_ref(), "comment")?;
        let ParsedExpressionKind::Constant(constant) = value.kind else {
            return Err(Error::not_implemented("Can only use constants as comments"));
        };
        let comment_value = constant.value;
        let catalog_type = match stmt.object_type {
            ObjectType::Table => Some(CatalogType::TableEntry),
            ObjectType::Index => Some(CatalogType::IndexEntry),
            ObjectType::View => Some(CatalogType::ViewEntry),
            ObjectType::Function => Some(CatalogType::MacroEntry),
            ObjectType::TableMacro => Some(CatalogType::TableMacroEntry),
            ObjectType::Sequence => Some(CatalogType::SequenceEntry),
            ObjectType::Type => Some(CatalogType::TypeEntry),
            _ => None,
        };
        let data = AlterEntryData {
            catalog: qualified_name.catalog,
            schema: qualified_name.schema,
            name: qualified_name.name,
            if_not_found: OnEntryNotFound::ThrowException,
        };
        let info = if let Some(entry_catalog_type) = catalog_type {
            alter_info(
                &data,
                AlterInfoKind::SetComment(SetCommentInfo {
                    entry_catalog_type,
                    comment_value,
                }),
            )
        } else if stmt.object_type == ObjectType::Column {
            alter_info(
                &data,
                AlterInfoKind::SetColumnComment(Box::new(SetColumnCommentInfo {
                    catalog_entry_type: CatalogType::Invalid,
                    comment_value,
                    column_name,
                })),
            )
        } else if stmt.object_type == ObjectType::Database {
            return Err(Error::not_implemented(
                "Adding comments to databases is not implemented",
            ));
        } else if stmt.object_type == ObjectType::Schema {
            return Err(Error::not_implemented(
                "Adding comments to schemas is not implemented",
            ));
        } else {
            return Err(Error::not_implemented("Can not comment on this type"));
        };
        Ok(SqlStatementKind::Alter(Box::new(info)))
    }
}

/// `TransformAndMaterializeAlter`: `ADD COLUMN c DEFAULT <non-constant>` becomes
/// `ADD COLUMN c DEFAULT NULL; UPDATE t SET c = <expr>; ALTER c SET DEFAULT <expr>`, so
/// that the values are materialized once (and replay identically from the WAL).
fn transform_and_materialize_alter(
    data: &AlterEntryData,
    column: ColumnDefinition,
    missing_ok: bool,
) -> SqlStatementKind {
    let column_name = column.name.as_bytes().to_owned();
    let default = column.expression.clone();
    let mut null_column = column;
    null_column.expression = Some(Box::new(build::constant(Value::default())));
    let add = alter_table(
        data,
        AlterTableInfo::AddColumn(Box::new(AddColumnInfo {
            new_column: null_column,
            if_column_not_exists: missing_ok,
        })),
    );
    let table = build::table_ref(TableRefKind::BaseTable(Box::new(BaseTableRef {
        schema_name: data.schema.clone(),
        table_name: data.name.clone(),
        column_name_alias: Vec::new(),
        catalog_name: data.catalog.clone(),
        at_clause: None,
    })));
    let update = UpdateStatement {
        table: Box::new(table),
        from_table: None,
        set_info: Box::new(UpdateSetInfo {
            condition: None,
            columns: vec![column_name.clone().into()],
            expressions: default.iter().map(|expr| (**expr).clone()).collect(),
        }),
        returning_list: Vec::new(),
        cte_map: CommonTableExpressionMap::default(),
        prioritize_table_when_binding: true,
    };
    let set_default = alter_table(
        data,
        AlterTableInfo::SetDefault(SetDefaultInfo {
            column_name: column_name.into(),
            expression: default,
        }),
    );
    SqlStatementKind::Multi(vec![
        alter_statement(add),
        SqlStatement::new(SqlStatementKind::Update(Box::new(update))),
        alter_statement(set_default),
    ])
}
