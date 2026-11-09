//! The other statements: transactions, PREPARE / EXECUTE / DEALLOCATE, PRAGMA, SET /
//! RESET, EXPLAIN, VACUUM, CALL, CHECKPOINT, LOAD / INSTALL, ATTACH / DETACH, USE,
//! EXPORT / IMPORT, COPY FROM DATABASE and UPDATE EXTENSIONS.

use roastedduck_ast::{
    AttachInfo, CallStatement, CatalogType, CopyDatabaseStatement, CopyDatabaseType, DetachInfo, DropInfo,
    ExecuteStatement, ExplainFormat, ExplainStatement, ExplainType, ExportStatement, ExpressionType,
    LoadInfo, LoadType, OnEntryNotFound, ParsedExpression, ParsedExpressionKind, PragmaInfo,
    PrepareStatement, SetScope, SetStatement, SqlStatementKind, TransactionInfo,
    TransactionInvalidationPolicy, TransactionModifierType, TransactionType, UpdateExtensionsInfo,
    VacuumInfo, VacuumOptions, keyword::write_optionally_quoted_bytes as write_optionally_quoted,
};
use roastedduck_common::{CaseInsensitiveMap, Error, OrderedMap, Result};
use roastedduck_pg_parser::nodes::{
    AttachStmt, CallStmt, CheckPointStmt, CopyDatabaseStmt, DeallocateStmt, DefElem, DetachStmt, ExecuteStmt,
    ExplainStmt, ExportStmt, FuncCall, ImportStmt, LoadInstallType, LoadStmt, Node, PragmaKind, PragmaStmt,
    PrepareStmt, TransactionStmt, TransactionStmtKind, TransactionStmtType, UpdateExtensionsStmt, UseStmt,
    VacuumStmt, VariableSetKind, VariableSetScope, VariableSetStmt, vacuum_option,
};
use roastedduck_types::{LogicalTypeId, Value};

use super::build;
use super::dml::copy_info;
use super::helpers::{transform_on_conflict, transform_on_entry_not_found, transform_qualified_name};
use super::{Transformer, cast_node, required, string_list};
use crate::equality::is_scalar;
use crate::hash_order::CiHashMap;

/// `ToSetScope`.
const fn to_set_scope(scope: VariableSetScope) -> SetScope {
    match scope {
        VariableSetScope::Local => SetScope::Local,
        VariableSetScope::Session => SetScope::Session,
        VariableSetScope::Global => SetScope::Global,
        VariableSetScope::Variable => SetScope::Variable,
        VariableSetScope::Default => SetScope::Automatic,
    }
}

/// `ParseFormat`: the argument of `EXPLAIN (FORMAT ...)`.
fn parse_explain_format(value: &Value) -> Result<ExplainFormat> {
    if value.logical_type().id() != LogicalTypeId::Varchar {
        return Err(Error::invalid_input("Expected a string as argument to FORMAT"));
    }
    let text = value.as_str().unwrap_or_default();
    let mut mapping: CiHashMap<ExplainFormat> = CiHashMap::new();
    for (name, format) in [
        ("default", ExplainFormat::Default),
        ("text", ExplainFormat::Text),
        ("json", ExplainFormat::Json),
        ("html", ExplainFormat::Html),
        ("graphviz", ExplainFormat::Graphviz),
        ("yaml", ExplainFormat::Yaml),
        ("mermaid", ExplainFormat::Mermaid),
    ] {
        mapping.insert(name.to_owned(), format);
    }
    if let Some(format) = mapping.get(text) {
        return Ok(*format);
    }
    let options: Vec<&str> = mapping.iter().map(|(name, _)| name).collect();
    Err(Error::invalid_input(roastedduck_common::byte_format!(
        "\"{text}\" is not a valid FORMAT argument, valid options are: {}",
        options.join(", ")
    )))
}

/// A `PragmaStatement` with the given name and constant string parameters.
fn pragma_statement(name: &str, parameters: Vec<ParsedExpression>) -> SqlStatementKind {
    SqlStatementKind::Pragma(Box::new(PragmaInfo {
        name: name.into(),
        parameters,
        named_parameters: CaseInsensitiveMap::default(),
    }))
}

impl<'a> Transformer<'a> {
    /// `TransformTransaction`: BEGIN / COMMIT / ROLLBACK.
    pub(crate) fn transform_transaction(stmt: &TransactionStmt) -> Result<SqlStatementKind> {
        let transaction_type = match stmt.kind {
            TransactionStmtKind::Begin | TransactionStmtKind::Start => TransactionType::BeginTransaction,
            TransactionStmtKind::Commit => TransactionType::Commit,
            TransactionStmtKind::Rollback => TransactionType::Rollback,
            other => {
                return Err(Error::not_implemented(roastedduck_common::byte_format!(
                    "Transaction type {} not implemented yet",
                    other as i32
                )));
            }
        };
        let modifier = match stmt.transaction_type {
            TransactionStmtType::Default => TransactionModifierType::TransactionDefaultModifier,
            TransactionStmtType::ReadOnly => TransactionModifierType::TransactionReadOnly,
            TransactionStmtType::ReadWrite => TransactionModifierType::TransactionReadWrite,
        };
        Ok(SqlStatementKind::Transaction(TransactionInfo {
            transaction_type,
            modifier,
            invalidation_policy: TransactionInvalidationPolicy::StandardPolicy,
            auto_rollback: false,
        }))
    }

    /// `TransformUpdateExtensions`.
    pub(crate) fn transform_update_extensions(stmt: &UpdateExtensionsStmt) -> Result<SqlStatementKind> {
        Ok(SqlStatementKind::UpdateExtensions(UpdateExtensionsInfo {
            extensions_to_update: string_list(&stmt.extensions)?,
        }))
    }

    /// `TransformPrepare`.
    pub(crate) fn transform_prepare(&mut self, stmt: &'a PrepareStmt) -> Result<SqlStatementKind> {
        if !stmt.argtypes.is_empty() {
            return Err(Error::not_implemented(
                "Prepared statement argument types are not supported, use CAST",
            ));
        }
        let name = stmt.name.clone().unwrap_or_default();
        let statement = self.transform_statement(required(stmt.query.as_ref(), "prepared statement")?)?;
        self.clear_parameters();
        Ok(SqlStatementKind::Prepare(PrepareStatement {
            name,
            statement: Box::new(statement),
        }))
    }

    /// `TransformExecute`.
    pub(crate) fn transform_execute(&mut self, stmt: &'a ExecuteStmt) -> Result<SqlStatementKind> {
        let name = stmt.name.clone().unwrap_or_default();
        let values = self.transform_expressions(&stmt.params)?;
        let mut named_values: CiHashMap<ParsedExpression, roastedduck_common::ByteString> =
            CiHashMap::new_with_keys();
        let mut param_idx = 0;
        for (i, mut expr) in values.into_iter().enumerate() {
            if !is_scalar(&expr) {
                return Err(Error::invalid_input(
                    "Only scalar parameters, named parameters or NULL supported for EXECUTE",
                ));
            }
            if !expr.alias.is_empty() && param_idx != 0 {
                return Err(Error::not_implemented(
                    "Mixing named parameters and positional parameters is not supported yet",
                ));
            }
            let mut param_name = expr.alias.clone();
            if expr.alias.is_empty() {
                param_name = (param_idx + 1).to_string().into();
                if param_idx != i {
                    return Err(Error::not_implemented(
                        "Mixing named parameters and positional parameters is not supported yet",
                    ));
                }
                param_idx += 1;
            }
            expr.alias.clear();
            named_values.set(param_name, expr);
        }
        Ok(SqlStatementKind::Execute(ExecuteStatement {
            name,
            named_values: named_values.into_ordered().into_iter().collect(),
        }))
    }

    /// `TransformDeallocate`: `DEALLOCATE name` drops a prepared statement.
    pub(crate) fn transform_deallocate(stmt: &DeallocateStmt) -> Result<SqlStatementKind> {
        let Some(name) = &stmt.name else {
            return Err(Error::parser("DEALLOCATE requires a name"));
        };
        Ok(SqlStatementKind::Drop(Box::new(DropInfo {
            catalog_type: CatalogType::PreparedStatement,
            catalog: String::new().into(),
            schema: String::new().into(),
            name: name.clone(),
            if_not_found: OnEntryNotFound::ThrowException,
            cascade: false,
            allow_drop_internal: false,
            extra_drop_info: None,
        })))
    }

    /// `TransformPragma`.
    pub(crate) fn transform_pragma(&mut self, stmt: &'a PragmaStmt) -> Result<SqlStatementKind> {
        let name = stmt.name.clone().unwrap_or_default();
        let mut parameters = Vec::new();
        let mut named_parameters: CiHashMap<ParsedExpression, roastedduck_common::ByteString> =
            CiHashMap::new_with_keys();
        for node in &stmt.args {
            let expr = self.transform_expression(node)?;
            if expr.expression_type == ExpressionType::CompareEqual {
                let ParsedExpressionKind::Comparison(comparison) = expr.kind else {
                    return Err(Error::internal("Comparison without operands"));
                };
                let ParsedExpressionKind::ColumnRef(column_ref) = &comparison.left.kind else {
                    return Err(Error::parser(
                        "Named parameter requires a column reference on the LHS",
                    ));
                };
                let key = if comparison.left.alias.is_empty() {
                    column_ref.column_names.last().cloned().unwrap_or_default()
                } else {
                    comparison.left.alias.clone()
                };
                named_parameters.insert(key, *comparison.right);
            } else if let ParsedExpressionKind::ColumnRef(column_ref) = &expr.kind {
                let text = if column_ref.column_names.len() > 1 {
                    expr.to_sql_bytes()
                } else {
                    column_ref.column_names.last().cloned().unwrap_or_default()
                };
                parameters.push(build::varchar_constant_bytes(text.as_bytes())?);
            } else {
                parameters.push(expr);
            }
        }
        match stmt.kind {
            PragmaKind::Nothing => {
                if !parameters.is_empty() || !named_parameters.is_empty() {
                    return Err(Error::internal(
                        "PRAGMA statement that is not a call or assignment cannot contain parameters",
                    ));
                }
            }
            PragmaKind::Assignment => {
                if parameters.len() != 1 {
                    return Err(Error::parser(
                        "PRAGMA statement with assignment should contain exactly one parameter",
                    ));
                }
                if !named_parameters.is_empty() {
                    return Err(Error::parser(
                        "PRAGMA statement with assignment cannot have named parameters",
                    ));
                }
                // SQLite compatibility: `PRAGMA table_info='t'` is a call.
                if !name.eq_ignore_ascii_case("table_info") {
                    let value = parameters.pop().map(Box::new);
                    return Ok(SqlStatementKind::Set(SetStatement {
                        name,
                        scope: SetScope::Automatic,
                        value,
                    }));
                }
            }
            PragmaKind::Call => {}
        }
        Ok(SqlStatementKind::Pragma(Box::new(PragmaInfo {
            name,
            parameters,
            named_parameters: named_parameters.into_ordered().into_iter().collect(),
        })))
    }

    /// `TransformExport`: `EXPORT DATABASE [name TO] 'path' (options)`.
    pub(crate) fn transform_export(&mut self, stmt: &'a ExportStmt) -> Result<SqlStatementKind> {
        let mut info = copy_info();
        info.file_path = stmt.filename.clone().unwrap_or_default();
        info.format = "csv".into();
        info.is_from = false;
        self.transform_copy_options(&mut info, &stmt.options)?;
        Ok(SqlStatementKind::Export(ExportStatement {
            info: Box::new(info),
            database: stmt.database.clone().unwrap_or_default(),
        }))
    }

    /// `TransformImport`: `IMPORT DATABASE 'path'` is `PRAGMA import_database('path')`.
    pub(crate) fn transform_import(stmt: &ImportStmt) -> Result<SqlStatementKind> {
        Ok(pragma_statement(
            "import_database",
            vec![build::try_varchar_constant(
                stmt.filename.as_deref().unwrap_or_default(),
            )?],
        ))
    }

    /// `TransformExplain`.
    pub(crate) fn transform_explain(&mut self, stmt: &'a ExplainStmt) -> Result<SqlStatementKind> {
        let mut explain_type = ExplainType::ExplainStandard;
        let mut explain_format = ExplainFormat::Default;
        let mut format_is_set = false;
        for node in &stmt.options {
            let def_elem: &'a DefElem = cast_node(node)?;
            let elem = def_elem
                .defname
                .as_deref()
                .unwrap_or_default()
                .to_ascii_lowercase();
            if elem == b"analyze" {
                explain_type = ExplainType::ExplainAnalyze;
            } else if elem == b"format" {
                if let Some(arg) = &def_elem.arg {
                    if format_is_set {
                        return Err(Error::invalid_input("FORMAT can not be provided more than once"));
                    }
                    let Node::Value(value) = arg else {
                        return Err(Error::internal("EXPLAIN FORMAT argument is not a value"));
                    };
                    let constant = Self::transform_value(value)?;
                    let ParsedExpressionKind::Constant(constant) = constant.kind else {
                        return Err(Error::internal("Value is not a constant"));
                    };
                    format_is_set = true;
                    explain_format = parse_explain_format(&constant.value)?;
                }
            } else {
                return Err(Error::not_implemented(roastedduck_common::byte_format!(
                    "Unimplemented explain type: {elem}"
                )));
            }
        }
        let statement = self.transform_statement(required(stmt.query.as_ref(), "explained statement")?)?;
        Ok(SqlStatementKind::Explain(ExplainStatement {
            statement: Box::new(statement),
            explain_type,
            explain_format,
        }))
    }

    /// `TransformVacuum`: `VACUUM` / `ANALYZE`.
    pub(crate) fn transform_vacuum(&mut self, stmt: &'a VacuumStmt) -> Result<SqlStatementKind> {
        let flags = stmt.options;
        let unsupported = [
            (vacuum_option::VERBOSE, "Verbose vacuum option"),
            (vacuum_option::FREEZE, "Freeze vacuum option"),
            (vacuum_option::FULL, "Full vacuum option"),
            (vacuum_option::NOWAIT, "No Wait vacuum option"),
            (vacuum_option::SKIPTOAST, "Skip Toast vacuum option"),
            (
                vacuum_option::DISABLE_PAGE_SKIPPING,
                "Disable Page Skipping vacuum option",
            ),
        ];
        let options = VacuumOptions {
            vacuum: flags & vacuum_option::VACUUM != 0,
            analyze: flags & vacuum_option::ANALYZE != 0,
        };
        for (flag, message) in unsupported {
            if flags & flag != 0 {
                return Err(Error::not_implemented(message));
            }
        }
        let mut info = VacuumInfo {
            options,
            has_table: false,
            table_ref: None,
            columns: Vec::new(),
        };
        if let Some(relation) = &stmt.relation {
            info.table_ref = Some(Box::new(self.transform_range_var(relation)?));
            info.has_table = true;
        }
        info.columns = string_list(&stmt.va_cols)?.into_iter().collect();
        Ok(SqlStatementKind::Vacuum(Box::new(info)))
    }

    /// `TransformCall`: `CALL function(...)`.
    pub(crate) fn transform_call(&mut self, stmt: &'a CallStmt) -> Result<SqlStatementKind> {
        let call: &'a FuncCall = cast_node(required(stmt.func.as_ref(), "function")?)?;
        let function = self.transform_func_call(call)?;
        Ok(SqlStatementKind::Call(CallStatement {
            function: Box::new(function),
        }))
    }

    /// `TransformSet`: `SET [scope] name = value` and `RESET [scope] name`.
    pub(crate) fn transform_set(&mut self, stmt: &'a VariableSetStmt) -> Result<SqlStatementKind> {
        let name = stmt.name.clone().unwrap_or_default();
        match stmt.kind {
            VariableSetKind::Value => {
                if stmt.scope == VariableSetScope::Local {
                    return Err(Error::not_implemented("SET LOCAL is not implemented."));
                }
                let [argument] = stmt.args.as_slice() else {
                    return Err(Error::parser("SET needs a single scalar value parameter"));
                };
                let scope = to_set_scope(stmt.scope);
                let mut expr = self.transform_expression(argument)?;
                if let ParsedExpressionKind::ColumnRef(column_ref) = &expr.kind {
                    let text = if column_ref.column_names.len() > 1 {
                        expr.to_sql_bytes()
                    } else {
                        column_ref.column_names.last().cloned().unwrap_or_default()
                    };
                    expr = build::varchar_constant_bytes(text.as_bytes())?;
                }
                let value = if expr.expression_type == ExpressionType::ValueDefault {
                    None
                } else {
                    Some(Box::new(expr))
                };
                Ok(SqlStatementKind::Set(SetStatement { name, scope, value }))
            }
            VariableSetKind::Reset => {
                if stmt.scope == VariableSetScope::Local {
                    return Err(Error::not_implemented("RESET LOCAL is not implemented."));
                }
                Ok(SqlStatementKind::Set(SetStatement {
                    name,
                    scope: to_set_scope(stmt.scope),
                    value: None,
                }))
            }
            _ => Err(Error::not_implemented("Can only SET or RESET a variable")),
        }
    }

    /// `TransformCheckpoint`: `CHECKPOINT [db]` is `CALL checkpoint([db])`.
    pub(crate) fn transform_checkpoint(stmt: &CheckPointStmt) -> Result<SqlStatementKind> {
        let name = if stmt.force {
            "force_checkpoint"
        } else {
            "checkpoint"
        };
        let children = stmt
            .name
            .iter()
            .map(build::try_varchar_constant)
            .collect::<Result<_>>()?;
        let mut function = build::function(name, children);
        if let ParsedExpressionKind::Function(function) = &mut function.kind {
            function.catalog = "system".into();
            function.schema = "main".into();
        }
        Ok(SqlStatementKind::Call(CallStatement {
            function: Box::new(function),
        }))
    }

    /// `TransformLoad`: `LOAD` / `INSTALL` / `FORCE INSTALL`.
    pub(crate) fn transform_load(stmt: &LoadStmt) -> SqlStatementKind {
        SqlStatementKind::Load(LoadInfo {
            filename: stmt.filename.clone().unwrap_or_default(),
            load_type: match stmt.load_type {
                LoadInstallType::Load => LoadType::Load,
                LoadInstallType::Install => LoadType::Install,
                LoadInstallType::ForceInstall => LoadType::ForceInstall,
            },
            repository: stmt.repository.clone().unwrap_or_default(),
            version: stmt.version.clone().unwrap_or_default(),
            repo_is_alias: stmt.repo_is_alias,
        })
    }

    /// `TransformAttach`.
    pub(crate) fn transform_attach(&mut self, stmt: &'a AttachStmt) -> Result<SqlStatementKind> {
        let mut parsed_options: CiHashMap<ParsedExpression, roastedduck_common::ByteString> =
            CiHashMap::new_with_keys();
        for node in &stmt.options {
            let def_elem: &'a DefElem = cast_node(node)?;
            let expr = match &def_elem.arg {
                Some(arg) => self.transform_expression(arg)?,
                None => build::constant(Value::boolean(true)),
            };
            parsed_options.set(
                def_elem
                    .defname
                    .as_deref()
                    .unwrap_or_default()
                    .to_ascii_lowercase(),
                expr,
            );
        }
        Ok(SqlStatementKind::Attach(Box::new(AttachInfo {
            name: stmt.name.clone().unwrap_or_default(),
            path: stmt.path.clone().unwrap_or_default(),
            options: OrderedMap::default(),
            on_conflict: transform_on_conflict(stmt.onconflict),
            parsed_options: parsed_options.into_ordered().into_iter().collect(),
        })))
    }

    /// `TransformDetach`.
    pub(crate) fn transform_detach(stmt: &DetachStmt) -> SqlStatementKind {
        SqlStatementKind::Detach(DetachInfo {
            name: stmt.db_name.clone().unwrap_or_default(),
            if_not_found: transform_on_entry_not_found(stmt.missing_ok),
        })
    }

    /// `TransformUse`: `USE db[.schema]` is `SET schema = 'db[.schema]'`.
    pub(crate) fn transform_use(stmt: &UseStmt) -> Result<SqlStatementKind> {
        let qname = transform_qualified_name(required(stmt.name.as_deref(), "database")?)?;
        if !qname.catalog.is_empty() {
            return Err(Error::parser(
                "Expected \"USE database\" or \"USE database.schema\"",
            ));
        }
        let name = if qname.schema.is_empty() {
            write_optionally_quoted(&qname.name)
        } else {
            roastedduck_common::byte_format!(
                "{}.{}",
                write_optionally_quoted(&qname.schema),
                write_optionally_quoted(&qname.name)
            )
        };
        Ok(SqlStatementKind::Set(SetStatement {
            name: "schema".into(),
            scope: SetScope::Automatic,
            value: Some(Box::new(build::try_varchar_constant(&name)?)),
        }))
    }

    /// `TransformCopyDatabase`.
    pub(crate) fn transform_copy_database(stmt: &CopyDatabaseStmt) -> Result<SqlStatementKind> {
        let from = stmt.from_database.clone().unwrap_or_default();
        let to = stmt.to_database.clone().unwrap_or_default();
        let Some(flag) = &stmt.copy_database_flag else {
            return Ok(pragma_statement(
                "copy_database",
                vec![
                    build::try_varchar_constant(&from)?,
                    build::try_varchar_constant(&to)?,
                ],
            ));
        };
        let copy_type = match flag.as_bytes() {
            b"schema" => CopyDatabaseType::CopySchema,
            b"data" => CopyDatabaseType::CopyData,
            _ => return Err(Error::not_implemented("Unsupported flag for COPY DATABASE")),
        };
        Ok(SqlStatementKind::CopyDatabase(CopyDatabaseStatement {
            from_database: from,
            to_database: to,
            copy_type,
        }))
    }
}
