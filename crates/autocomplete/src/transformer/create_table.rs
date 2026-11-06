//! The transform functions of DuckDB's `transform_create_table.cpp`: `CREATE ...`,
//! `CREATE TABLE`, column definitions and constraints, and the identifier rules shared
//! by the DDL statements.

use rustdb_common::LogicalIndex;

use super::common::{simple_type_expression, type_expression_of, unbound_type};
use super::prelude::*;

/// `DConstants::INVALID_INDEX`: the column index of table-level `UNIQUE` constraints.
const INVALID_INDEX: u64 = u64::MAX;

/// A `CreateInfo` with the defaults of DuckDB's `CreateInfo(type, schema)`.
pub(super) fn create_info(schema: impl AsRef<[u8]>, kind: CreateInfoKind) -> CreateInfo {
    CreateInfo {
        catalog: String::new().into(),
        schema: schema.as_ref().into(),
        temporary: false,
        internal: false,
        on_conflict: OnCreateConflict::ErrorOnConflict,
        sql: String::new().into(),
        comment: Value::default(),
        tags: CaseInsensitiveMap::default(),
        dependencies: LogicalDependencyList::default(),
        kind,
    }
}

/// The conflict policy of `CREATE ... [IF NOT EXISTS]`.
pub(super) const fn on_create_conflict(if_not_exists: bool) -> OnCreateConflict {
    if if_not_exists {
        OnCreateConflict::IgnoreOnConflict
    } else {
        OnCreateConflict::ErrorOnConflict
    }
}

/// What happens when the entry of `DROP` / `ALTER ... [IF EXISTS]` is missing.
pub(super) const fn on_entry_not_found(if_exists: bool) -> OnEntryNotFound {
    if if_exists {
        OnEntryNotFound::ReturnNull
    } else {
        OnEntryNotFound::ThrowException
    }
}

/// `ColumnDefinition(name, type)`.
pub(super) fn column_definition(name: impl Into<ByteString>, logical_type: LogicalType) -> ColumnDefinition {
    ColumnDefinition {
        name: name.into(),
        logical_type,
        expression: None,
        category: TableColumnType::Standard,
        compression_type: CompressionType::CompressionAuto,
        comment: Value::default(),
        tags: OrderedMap::default(),
    }
}

/// The `BaseTableRef` of a `BaseTableName` result.
pub(super) fn base_table_ref(table: BaseTable) -> Result<BaseTableRef> {
    match table.0.kind {
        TableRefKind::BaseTable(table) => Ok(*table),
        _ => Err(Error::internal("Expected a base table reference")),
    }
}

/// `ColumnList::AddColumn`: column names are unique, case-insensitively.
fn add_column(columns: &mut ColumnList, column: ColumnDefinition) -> Result<()> {
    if columns
        .columns
        .iter()
        .any(|existing| existing.name.eq_ignore_ascii_case(&column.name))
    {
        return Err(Error::catalog(byte_format!(
            "Column with name {} already exists!",
            column.name
        )));
    }
    columns.columns.push(column);
    Ok(())
}

/// `PEGTransformerFactory::VerifyColumnRefs`: generated columns cannot use qualified
/// column references.
fn verify_column_refs(expr: &ParsedExpression) -> Result<()> {
    if let ParsedExpressionKind::ColumnRef(column_ref) = &expr.kind
        && column_ref.column_names.len() > 1
    {
        return Err(Error::parser(
            "Qualified (tbl.name) column references are not allowed inside of generated column expressions",
        ));
    }
    let mut result = Ok(());
    rustdb_parser::expression_children(expr, &mut |child| {
        if result.is_ok() {
            result = verify_column_refs(child);
        }
    });
    result
}

impl PegTransformer {
    /// `TransformCreateStatement`: `CREATE [OR REPLACE] [TEMPORARY | PERSISTENT] ...`.
    pub(super) fn transform_create_statement(&mut self, parse_result: &ParseResult) -> Result<SqlStatement> {
        let list = parse_result.list()?;
        let replace = list.has(1)?;
        let mut info: CreateInfo = self.transform_child(list, 3)?;
        if replace {
            if info.on_conflict == OnCreateConflict::IgnoreOnConflict {
                return Err(Error::parser(
                    "Cannot specify both OR REPLACE and IF NOT EXISTS within single create statement",
                ));
            }
            info.on_conflict = OnCreateConflict::ReplaceOnConflict;
        }
        let persistent_type = self
            .transform_optional(list, 2)?
            .unwrap_or(SecretPersistType::Default);
        if let CreateInfoKind::Secret(secret_info) = &mut info.kind {
            secret_info.persist_type = persistent_type;
        }
        info.temporary = persistent_type == SecretPersistType::Temporary;
        Ok(SqlStatement::new(SqlStatementKind::Create(Box::new(info))))
    }

    /// `TransformTemporary`: `PERSISTENT`, `TEMP` or `TEMPORARY`.
    pub(super) fn transform_temporary(&mut self, parse_result: &ParseResult) -> Result<SecretPersistType> {
        Self::transform_enum(parse_result.list()?.choice(0)?)
    }

    /// `TransformCreateStatementVariation`: what is created.
    pub(super) fn transform_create_statement_variation(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<CreateInfo> {
        self.transform(parse_result.list()?.choice(0)?)
    }

    /// `TransformCreateTableStmt`: `TABLE [IF NOT EXISTS] name (CreateTableAs |
    /// CreateColumnList) [ON COMMIT ...]`.
    pub(super) fn transform_create_table_stmt(&mut self, parse_result: &ParseResult) -> Result<CreateInfo> {
        let list = parse_result.list()?;
        let table_name: QualifiedName = self.transform_child(list, 2)?;
        if table_name.name.is_empty() {
            return Err(Error::parser("Empty table name not supported"));
        }
        let mut table = CreateTableInfo {
            table: table_name.name,
            columns: ColumnList::default(),
            constraints: Vec::new(),
            query: None,
            partition_keys: Vec::new(),
            sort_keys: Vec::new(),
            options: CaseInsensitiveMap::default(),
        };
        let if_not_exists = list.has(1)?;
        let table_as_or_column_list = list.list(3)?.child(0)?;
        if table_as_or_column_list.name == "CreateTableAs" {
            let create_table_as: CreateTableAs = self.transform(table_as_or_column_list.choice()?)?;
            table.query = create_table_as.select_statement.map(Box::new);
            table.columns = create_table_as.column_names;
            table.partition_keys = create_table_as.partition_keys;
            table.sort_keys = create_table_as.sort_keys;
            table.options = create_table_as.options;
        } else {
            let column_list: ColumnElements = self.transform(table_as_or_column_list.choice()?)?;
            table.columns = column_list.columns;
            table.constraints = column_list.constraints;
            table.partition_keys = column_list.partition_keys;
            table.sort_keys = column_list.sort_keys;
            table.options = column_list.options;
        }
        // ON COMMIT is only checked for ON COMMIT DELETE ROWS, which is unsupported.
        let _on_commit: Option<bool> = self.transform_optional(list, 4)?;
        let mut info = create_info(&table_name.schema, CreateInfoKind::Table(Box::new(table)));
        info.catalog = table_name.catalog;
        info.on_conflict = on_create_conflict(if_not_exists);
        Ok(info)
    }

    /// `TransformCreateTableAs`: `[(names)] [PARTITIONED BY ...] [WITH (...)] AS query
    /// [WITH [NO] DATA]`.
    pub(super) fn transform_create_table_as(&mut self, parse_result: &ParseResult) -> Result<CreateTableAs> {
        let list = parse_result.list()?;
        let mut result = CreateTableAs::default();
        if let Some(column_names) = self.transform_optional(list, 0)? {
            result.column_names = column_names;
        }
        let pso: PartitionSortedOptions = self.transform_optional(list, 1)?.unwrap_or_default();
        result.partition_keys = pso.partition_keys;
        result.sort_keys = pso.sort_keys;
        if let Some(options) = self.transform_optional(list, 2)? {
            result.options = options;
        }
        let statement: SqlStatement = self.transform_child(list, 4)?;
        let SqlStatementKind::Select(mut select_statement) = statement.kind else {
            return Err(Error::parser("CREATE TABLE AS requires a SELECT clause"));
        };
        if let Some(with_data) = self.transform_optional(list, 5)? {
            result.with_data = with_data;
        }
        // `WithData` is true for `WITH NO DATA`.
        if result.with_data {
            select_statement
                .node
                .modifiers
                .push(ResultModifier::Limit(LimitModifier {
                    limit: Some(Box::new(ParsedExpression::constant(Value::integer(0)))),
                    offset: None,
                }));
        }
        result.select_statement = Some(select_statement);
        Ok(result)
    }

    /// `TransformIdentifierList`: `(name, ...)`, as columns of unknown type.
    pub(super) fn transform_identifier_list(&mut self, parse_result: &ParseResult) -> Result<ColumnList> {
        let identifiers = parse_result.list()?.child(0)?.parens_content()?.list_elements()?;
        let mut result = ColumnList::default();
        for identifier in identifiers {
            add_column(
                &mut result,
                column_definition(identifier.identifier()?.to_owned(), LogicalType::UNKNOWN),
            )?;
        }
        Ok(result)
    }

    /// `TransformCreateColumnList`: `(columns and constraints) [PARTITIONED BY ...]
    /// [WITH (...)]`.
    pub(super) fn transform_create_column_list(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ColumnElements> {
        let list = parse_result.list()?;
        let Some(create_table_column_list) = list.child(0)?.parens_content()?.optional()? else {
            return Err(Error::parser("Table must have at least one column!"));
        };
        let mut result: ColumnElements = self.transform(create_table_column_list)?;
        let pso: PartitionSortedOptions = self.transform_optional(list, 1)?.unwrap_or_default();
        result.partition_keys = pso.partition_keys;
        result.sort_keys = pso.sort_keys;
        if let Some(options) = self.transform_optional(list, 2)? {
            result.options = options;
        }
        Ok(result)
    }

    /// `TransformCreateTableColumnList`: the column definitions and table constraints.
    /// Column constraints get the index of their element in the list (constraints
    /// included).
    pub(super) fn transform_create_table_column_list(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ColumnElements> {
        let column_elements = parse_result.list()?.child(0)?.list_elements()?;
        let mut result = ColumnElements::default();
        for (col_idx, column_element) in column_elements.into_iter().enumerate() {
            let column_element_child = column_element.list()?.choice(0)?;
            match column_element_child.name {
                "ColumnDefinition" => {
                    let column_result: ConstraintColumnDefinition = self.transform(column_element_child)?;
                    result.constraints.extend(column_result.constraints);
                    let index = LogicalIndex(col_idx as u64);
                    for (is_primary_key, constraint_type) in column_result.constraint_types {
                        match constraint_type {
                            ConstraintType::NotNull => {
                                result
                                    .constraints
                                    .push(Constraint::NotNull(NotNullConstraint { index }));
                            }
                            ConstraintType::Unique => {
                                result.constraints.push(Constraint::Unique(UniqueConstraint {
                                    is_primary_key,
                                    index,
                                    columns: vec![column_result.column_definition.name.clone()],
                                }));
                            }
                            _ => {}
                        }
                    }
                    add_column(&mut result.columns, column_result.column_definition)?;
                }
                "TopLevelConstraint" => {
                    result.constraints.push(self.transform(column_element_child)?);
                }
                other => {
                    return Err(Error::not_implemented(byte_format!(
                        "Unknown column type encountered: {other}"
                    )));
                }
            }
        }
        Ok(result)
    }

    /// `TransformIdentifierOrStringLiteral`: an unqualified name.
    pub(super) fn transform_identifier_or_string_literal(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<QualifiedName> {
        let choice = parse_result.list()?.choice(0)?;
        let mut result = QualifiedName::default();
        match &choice.kind {
            ParseResultKind::Identifier(identifier) => result.name = identifier.into(),
            ParseResultKind::String { text, .. } => result.name = text.into(),
            _ => {}
        }
        Ok(result)
    }

    /// `TransformColIdOrString`.
    pub(super) fn transform_col_id_or_string(&mut self, parse_result: &ParseResult) -> Result<String> {
        self.transform(parse_result.list()?.choice(0)?)
    }

    /// `TransformColLabelOrString`.
    pub(super) fn transform_col_label_or_string(&mut self, parse_result: &ParseResult) -> Result<String> {
        let choice = parse_result.list()?.choice(0)?;
        if choice.result_type() == ParseResultType::String {
            return Ok(choice.string_literal()?.0.to_owned());
        }
        self.transform(choice)
    }

    /// `TransformColId`.
    pub(super) fn transform_col_id(&mut self, parse_result: &ParseResult) -> Result<String> {
        let choice = parse_result.list()?.choice(0)?;
        if choice.result_type() == ParseResultType::Identifier {
            return Ok(choice.identifier()?.to_owned());
        }
        self.transform(choice)
    }

    /// `TransformIdentifier`.
    pub(super) fn transform_identifier(&mut self, parse_result: &ParseResult) -> Result<String> {
        Ok(parse_result.list()?.identifier(0)?.to_owned())
    }

    /// `TransformDottedIdentifier`: `name (. label)*`.
    pub(super) fn transform_dotted_identifier(&mut self, parse_result: &ParseResult) -> Result<Vec<String>> {
        let list = parse_result.list()?;
        let mut parts = vec![list.identifier(0)?.to_owned()];
        if let Some(optional_elements) = list.optional(1)? {
            for child in optional_elements.repeat()? {
                parts.push(self.transform(child.list()?.child(1)?)?);
            }
        }
        Ok(parts)
    }

    /// `TransformColumnDefinition`: `name [type] [GENERATED ...] [CONSTRAINT name]
    /// constraints*`.
    pub(super) fn transform_column_definition(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ConstraintColumnDefinition> {
        let list = parse_result.list()?;
        let dotted_identifier: Vec<String> = self.transform_child(list, 0)?;
        let qualified_name = QualifiedName::from_parts(dotted_identifier)?;
        let type_opt = list.optional(1)?;
        let generated_opt = list.optional(2)?;
        let mut ty = LogicalType::ANY;
        if type_opt.is_none() && generated_opt.is_none() {
            return Err(Error::parser(byte_format!(
                "Column {} must have a type or be defined as a GENERATED column.",
                qualifier_to_string(
                    &qualified_name.catalog,
                    &qualified_name.schema,
                    &qualified_name.name
                )
            )));
        }
        if let Some(column_type) = self.transform_optional(list, 1)? {
            ty = column_type;
        }
        let mut compression_type = CompressionType::CompressionAuto;
        let mut default_value: Option<ParsedExpression> = None;
        let mut constraint_types: Vec<(bool, ConstraintType)> = Vec::new();
        let mut constraints: Vec<Constraint> = Vec::new();
        if let Some(constraints_repeat) = list.optional(4)? {
            for constraint_entry in constraints_repeat.repeat()? {
                let constraint = constraint_entry.list()?.choice(0)?;
                match constraint.name {
                    "DefaultValue" => {
                        if default_value.is_some() {
                            return Err(Error::parser("Cannot define a default value twice"));
                        }
                        default_value = Some(self.transform(constraint)?);
                    }
                    "NotNullConstraint" | "UniqueConstraint" | "PrimaryKeyConstraint" => {
                        constraint_types.push(self.transform(constraint)?);
                    }
                    "ColumnCompression" => {
                        compression_type = self.transform(constraint)?;
                        if compression_type == CompressionType::CompressionAuto {
                            return Err(Error::parser(
                                "Unrecognized option for column compression, expected none, uncompressed, rle, \
                                 dictionary, pfor, bitpacking, fsst, chimp, patas, zstd, alp, alprd or roaring",
                            ));
                        }
                    }
                    "ForeignKeyConstraint" => {
                        let mut fk_constraint: ForeignKeyConstraint = self.transform(constraint)?;
                        fk_constraint.fk_columns.push(qualified_name.name.clone());
                        constraints.push(Constraint::ForeignKey(Box::new(fk_constraint)));
                    }
                    "ColumnCollation" => {
                        if ty.id() == LogicalTypeId::Any {
                            return Err(Error::parser(byte_format!(
                                "Specify the VARCHAR type for column \"{}\" with collation.",
                                qualifier_to_string(
                                    &qualified_name.catalog,
                                    &qualified_name.schema,
                                    &qualified_name.name
                                )
                            )));
                        } else if ty.id() == LogicalTypeId::Unbound {
                            let expr = type_expression_of(&ty)?;
                            let ParsedExpressionKind::Type(type_expr) = &expr.kind else {
                                return Err(Error::internal("Expected a type expression"));
                            };
                            if !type_expr.type_name.eq_ignore_ascii_case("VARCHAR") {
                                return Err(Error::parser("Only VARCHAR columns can have collations!"));
                            }
                        } else {
                            return Err(Error::internal("Expected only unbound types here"));
                        }
                        let collation: ParsedExpression = self.transform(constraint)?;
                        ty = unbound_type(simple_type_expression("VARCHAR", vec![collation]))?;
                    }
                    _ => constraints.push(self.transform(constraint)?),
                }
            }
        }
        if let Some(generated_pr) = generated_opt {
            let generated: GeneratedColumnDefinition = self.transform(generated_pr)?;
            let mut expr = generated
                .expr
                .ok_or_else(|| Error::internal("Generated column without an expression"))?;
            if rustdb_parser::has_subquery(&expr) {
                return Err(Error::parser(byte_format!(
                    "Expression of generated column \"{}\" contains a subquery, which isn't allowed",
                    qualified_name.name
                )));
            }
            if ty != LogicalType::ANY {
                expr = ParsedExpression::cast(expr, ty.clone());
            }
            if rustdb_parser::has_subquery(&expr) {
                return Err(Error::parser(byte_format!(
                    "Expression of generated column \"{}\" contains a subquery, which isn't allowed",
                    qualified_name.name
                )));
            }
            let mut col = column_definition(qualified_name.name, ty);
            col.expression = Some(Box::new(expr));
            col.category = TableColumnType::Generated;
            col.compression_type = compression_type;
            if default_value.is_some() {
                return Err(Error::parser("Not allowed to set default on a generated column"));
            }
            return Ok(ConstraintColumnDefinition {
                column_definition: col,
                constraint_types,
                constraints,
            });
        }
        let mut col = column_definition(qualified_name.name, ty);
        if let Some(default_value) = default_value {
            col.expression = Some(Box::new(default_value));
        }
        col.compression_type = compression_type;
        Ok(ConstraintColumnDefinition {
            column_definition: col,
            constraint_types,
            constraints,
        })
    }

    /// `TransformGeneratedColumn`: `[GENERATED ALWAYS] AS (expr) [VIRTUAL | STORED]`.
    pub(super) fn transform_generated_column(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<GeneratedColumnDefinition> {
        let list = parse_result.list()?;
        let expr: ParsedExpression = self.transform(list.child(2)?.parens_content()?)?;
        verify_column_refs(&expr)?;
        if let Some(generated_column_type) = list.optional(3)? {
            let _: bool = self.transform(generated_column_type)?;
        }
        Ok(GeneratedColumnDefinition {
            expr: Some(expr),
            ..GeneratedColumnDefinition::default()
        })
    }

    /// `TransformDefaultValue`: `DEFAULT expr`.
    pub(super) fn transform_default_value(&mut self, parse_result: &ParseResult) -> Result<ParsedExpression> {
        self.transform_child(parse_result.list()?, 1)
    }

    /// `TransformTopLevelConstraint`: `[CONSTRAINT name] constraint`; the name is
    /// ignored.
    pub(super) fn transform_top_level_constraint(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Constraint> {
        let list = parse_result.list()?;
        let _constraint_name = list.optional(0)?;
        self.transform_child(list, 1)
    }

    /// `TransformTopLevelConstraintList`.
    pub(super) fn transform_top_level_constraint_list(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Constraint> {
        self.transform(parse_result.list()?.choice(0)?)
    }

    /// `TransformTopPrimaryKeyConstraint`: `PRIMARY KEY (columns)`.
    pub(super) fn transform_top_primary_key_constraint(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Constraint> {
        let columns: Vec<ByteString> = self.transform_child(parse_result.list()?, 2)?;
        Ok(Constraint::Unique(UniqueConstraint {
            is_primary_key: true,
            index: LogicalIndex(INVALID_INDEX),
            columns,
        }))
    }

    /// `TransformTopUniqueConstraint`: `UNIQUE (columns)`.
    pub(super) fn transform_top_unique_constraint(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Constraint> {
        let columns: Vec<ByteString> = self.transform_child(parse_result.list()?, 1)?;
        Ok(Constraint::Unique(UniqueConstraint {
            is_primary_key: false,
            index: LogicalIndex(INVALID_INDEX),
            columns,
        }))
    }

    /// `TransformCheckConstraint`: `CHECK (expr)`, without subqueries.
    pub(super) fn transform_check_constraint(&mut self, parse_result: &ParseResult) -> Result<Constraint> {
        let check_expr: ParsedExpression =
            self.transform(parse_result.list()?.child(1)?.parens_content()?)?;
        if rustdb_parser::has_subquery(&check_expr) {
            return Err(Error::parser("subqueries prohibited in CHECK constraints"));
        }
        Ok(Constraint::Check(CheckConstraint {
            expression: Box::new(check_expr),
        }))
    }

    /// `TransformTopForeignKeyConstraint`: `FOREIGN KEY (columns) REFERENCES ...`.
    pub(super) fn transform_top_foreign_key_constraint(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Constraint> {
        let list = parse_result.list()?;
        let pk_list: Vec<ByteString> = self.transform_child(list, 2)?;
        let mut fk_constraint: ForeignKeyConstraint = self.transform_child(list, 3)?;
        fk_constraint.fk_columns = pk_list;
        if !fk_constraint.pk_columns.is_empty()
            && fk_constraint.fk_columns.len() != fk_constraint.pk_columns.len()
        {
            return Err(Error::parser(
                "The number of referencing and referenced columns for foreign keys must be the same",
            ));
        }
        Ok(Constraint::ForeignKey(Box::new(fk_constraint)))
    }

    /// `TransformColumnIdList`: `(column, ...)`.
    pub(super) fn transform_column_id_list(&mut self, parse_result: &ParseResult) -> Result<Vec<String>> {
        parse_result
            .list()?
            .child(0)?
            .parens_content()?
            .list_elements()?
            .into_iter()
            .map(|colid| self.transform(colid))
            .collect()
    }

    /// `TransformTypeFuncName`.
    pub(super) fn transform_type_func_name(&mut self, parse_result: &ParseResult) -> Result<String> {
        let choice = parse_result.list()?.choice(0)?;
        if choice.result_type() == ParseResultType::Identifier {
            return Ok(choice.identifier()?.to_owned());
        }
        self.transform(choice)
    }

    /// `TransformColumnCompression`: `USING COMPRESSION name`.
    pub(super) fn transform_column_compression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<CompressionType> {
        let compression_string: String = self.transform_child(parse_result.list()?, 2)?;
        let compression_string = compression_string.to_ascii_lowercase();
        CompressionType::from_name(&compression_string).ok_or_else(|| {
            rustdb_function::logging::enum_error(
                &compression_string,
                "CompressionType",
                &CompressionType::ALL.map(CompressionType::name),
            )
        })
    }

    /// `TransformForeignKeyConstraint`: `REFERENCES table [(columns)] [ON ...]`; the
    /// referencing columns are filled in by the caller.
    pub(super) fn transform_foreign_key_constraint(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ForeignKeyConstraint> {
        let list = parse_result.list()?;
        let base_table = base_table_ref(self.transform_child(list, 1)?)?;
        let mut pk_list = Vec::new();
        if let Some(col_list) = list.optional(2)? {
            pk_list = self.transform(col_list.parens_content()?)?;
        }
        let _key_actions: KeyActions = self.transform_child(list, 3)?;
        Ok(ForeignKeyConstraint {
            pk_columns: pk_list,
            fk_columns: Vec::new(),
            fk_type: ForeignKeyType::FkTypeForeignKeyTable,
            schema: base_table.schema_name,
            table: base_table.table_name,
            pk_keys: Vec::new(),
            fk_keys: Vec::new(),
        })
    }

    /// `TransformKeyActions`: `[ON UPDATE action] [ON DELETE action]`.
    pub(super) fn transform_key_actions(&mut self, parse_result: &ParseResult) -> Result<KeyActions> {
        let list = parse_result.list()?;
        let mut results = KeyActions::default();
        if let Some(update_action) = self.transform_optional(list, 0)? {
            results.update_action = update_action;
        }
        if let Some(delete_action) = self.transform_optional(list, 1)? {
            results.delete_action = delete_action;
        }
        Ok(results)
    }

    /// `TransformUpdateAction`: `ON UPDATE action`.
    pub(super) fn transform_update_action(&mut self, parse_result: &ParseResult) -> Result<String> {
        self.transform_child(parse_result.list()?, 2)
    }

    /// `TransformDeleteAction`: `ON DELETE action`.
    pub(super) fn transform_delete_action(&mut self, parse_result: &ParseResult) -> Result<String> {
        self.transform_child(parse_result.list()?, 2)
    }

    /// `TransformKeyAction`.
    pub(super) fn transform_key_action(&mut self, parse_result: &ParseResult) -> Result<String> {
        self.transform(parse_result.list()?.choice(0)?)
    }

    /// `TransformNoKeyAction`: `NO ACTION`.
    pub(super) fn transform_no_key_action(&mut self, _parse_result: &ParseResult) -> Result<String> {
        Ok("NoKeyAction".to_owned())
    }

    /// `TransformRestrictKeyAction`: `RESTRICT`.
    pub(super) fn transform_restrict_key_action(&mut self, _parse_result: &ParseResult) -> Result<String> {
        Ok("Restrict".to_owned())
    }

    /// `TransformCascadeKeyAction`: `CASCADE` is not supported.
    pub(super) fn transform_cascade_key_action(&mut self, _parse_result: &ParseResult) -> Result<String> {
        Err(Error::parser(
            "FOREIGN KEY constraints cannot use CASCADE, SET NULL or SET DEFAULT",
        ))
    }

    /// `TransformSetNullKeyAction`: `SET NULL` is not supported.
    pub(super) fn transform_set_null_key_action(&mut self, _parse_result: &ParseResult) -> Result<String> {
        Err(Error::parser(
            "FOREIGN KEY constraints cannot use CASCADE, SET NULL or SET DEFAULT",
        ))
    }

    /// `TransformSetDefaultKeyAction`: `SET DEFAULT` is not supported.
    pub(super) fn transform_set_default_key_action(&mut self, _parse_result: &ParseResult) -> Result<String> {
        Err(Error::parser(
            "FOREIGN KEY constraints cannot use CASCADE, SET NULL or SET DEFAULT",
        ))
    }

    /// `TransformPrimaryKeyConstraint`: `PRIMARY KEY` on a column.
    pub(super) fn transform_primary_key_constraint(
        &mut self,
        _parse_result: &ParseResult,
    ) -> Result<(bool, ConstraintType)> {
        Ok((true, ConstraintType::Unique))
    }

    /// `TransformUniqueConstraint`: `UNIQUE` on a column.
    pub(super) fn transform_unique_constraint(
        &mut self,
        _parse_result: &ParseResult,
    ) -> Result<(bool, ConstraintType)> {
        Ok((false, ConstraintType::Unique))
    }

    /// `TransformNotNullConstraint`: `NOT NULL`, or `NULL` (no constraint).
    pub(super) fn transform_not_null_constraint(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<(bool, ConstraintType)> {
        if parse_result.list()?.has(0)? {
            return Ok((false, ConstraintType::NotNull));
        }
        Ok((false, ConstraintType::Invalid))
    }

    /// `TransformColumnCollation`: `COLLATE name` as the `collation` type parameter.
    pub(super) fn transform_column_collation(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        let dotted_identifier: Vec<String> = self.transform_child(parse_result.list()?, 1)?;
        let collation = dotted_identifier.join(".");
        Ok(ParsedExpression::constant(Value::varchar(collation)).with_alias("collation"))
    }

    /// `TransformWithData`: `WITH [NO] DATA`; true for `WITH NO DATA`.
    pub(super) fn transform_with_data(&mut self, parse_result: &ParseResult) -> Result<bool> {
        parse_result.list()?.has(1)
    }

    /// `TransformCommitAction`: `ON COMMIT PRESERVE ROWS`.
    pub(super) fn transform_commit_action(&mut self, parse_result: &ParseResult) -> Result<bool> {
        self.transform_child(parse_result.list()?, 2)
    }

    /// `TransformPreserveOrDelete`: only `PRESERVE` is supported.
    pub(super) fn transform_preserve_or_delete(&mut self, parse_result: &ParseResult) -> Result<bool> {
        let preserve_or_delete = parse_result.list()?.choice(0)?.keyword()?;
        if preserve_or_delete.eq_ignore_ascii_case("delete") {
            return Err(Error::not_implemented(
                "Only ON COMMIT PRESERVE ROWS is supported",
            ));
        }
        Ok(true)
    }

    /// `TransformGeneratedColumnType`: only `VIRTUAL` is supported.
    pub(super) fn transform_generated_column_type(&mut self, parse_result: &ParseResult) -> Result<bool> {
        let keyword = parse_result.list()?.choice(0)?.keyword()?;
        if keyword.eq_ignore_ascii_case("stored") {
            return Err(Error::invalid_input("Can not create a STORED generated column!"));
        }
        Ok(true)
    }

    /// `TransformPartitionOptions`: `PARTITIONED BY (expr, ...)`.
    pub(super) fn transform_partition_options(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<ParsedExpression>> {
        self.transform_parenthesized_expressions(parse_result)
    }

    /// `TransformSortedOptions`: `SORTED BY (expr, ...)`.
    pub(super) fn transform_sorted_options(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<ParsedExpression>> {
        self.transform_parenthesized_expressions(parse_result)
    }

    /// The expressions of `KEYWORD BY (expr, ...)`.
    fn transform_parenthesized_expressions(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<ParsedExpression>> {
        parse_result
            .list()?
            .child(2)?
            .parens_content()?
            .list_elements()?
            .into_iter()
            .map(|expr| self.transform(expr))
            .collect()
    }

    /// `TransformPartitionOptSortedOptions`: `PARTITIONED BY (...) [SORTED BY (...)]`.
    pub(super) fn transform_partition_opt_sorted_options(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<PartitionSortedOptions> {
        let list = parse_result.list()?;
        Ok(PartitionSortedOptions {
            partition_keys: self.transform_child(list, 0)?,
            sort_keys: self.transform_optional(list, 1)?.unwrap_or_default(),
        })
    }

    /// `TransformSortedOptPartitionOptions`: `SORTED BY (...) [PARTITIONED BY (...)]`.
    pub(super) fn transform_sorted_opt_partition_options(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<PartitionSortedOptions> {
        let list = parse_result.list()?;
        let sort_keys = self.transform_child(list, 0)?;
        let partition_keys = self.transform_optional(list, 1)?.unwrap_or_default();
        Ok(PartitionSortedOptions {
            partition_keys,
            sort_keys,
        })
    }

    /// `TransformPartitionSortedOptions`.
    pub(super) fn transform_partition_sorted_options(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<PartitionSortedOptions> {
        self.transform(parse_result.list()?.choice(0)?)
    }
}
