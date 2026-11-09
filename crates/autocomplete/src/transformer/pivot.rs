//! The transform functions of DuckDB's `transform_pivot.cpp` (`PIVOT` / `UNPIVOT`
//! statements), and the `CREATE TYPE ... AS ENUM` statements a `PIVOT` without `IN`
//! lists expands into (`PEGTransformer::CreatePivotStatement`).

use roastedduck_parser::{build, has_subquery, is_scalar};

use super::prelude::*;

/// `PivotColumn()`.
const fn pivot_column() -> PivotColumn {
    PivotColumn {
        pivot_expressions: Vec::new(),
        unpivot_names: Vec::new(),
        entries: Vec::new(),
        pivot_enum: ByteString::new(),
        subquery: None,
    }
}

/// `PivotRef()` of `source`.
fn pivot_ref(source: TableRef) -> PivotRef {
    PivotRef {
        source: Box::new(source),
        aggregates: Vec::new(),
        unpivot_names: Vec::new(),
        pivots: Vec::new(),
        groups: Vec::new(),
        column_name_alias: Vec::new(),
        include_nulls: false,
        bound: None,
    }
}

/// `SELECT * FROM pivot_ref`.
fn select_star_from_pivot(pivot_ref: PivotRef) -> SelectStatement {
    let mut select_node = build::select_node();
    select_node.select_list.push(build::star());
    *select_node.from_table = build::table_ref(TableRefKind::Pivot(Box::new(pivot_ref)));
    build::select_statement(build::query_node(QueryNodeKind::Select(Box::new(select_node))))
}

/// The unpivot entries of `target_list`: each expression under its alias.
fn unpivot_entries(target_list: Vec<ParsedExpression>) -> impl Iterator<Item = PivotColumnEntry> {
    target_list.into_iter().map(|expr| PivotColumnEntry {
        values: Vec::new(),
        alias: expr.alias.clone(),
        star_expr: Some(Box::new(expr)),
    })
}

/// `SQLStatement::ToString` of a statement a `PIVOT` expands into.
fn statement_to_string(statement: &SqlStatement) -> Result<ByteString> {
    Ok(match &statement.kind {
        SqlStatementKind::Select(select) => roastedduck_common::byte_format!("{select}"),
        SqlStatementKind::Insert(insert) => roastedduck_common::byte_format!("{insert}"),
        SqlStatementKind::Create(info) => info.to_sql().unwrap_or_default(),
        _ => ByteString::new(),
    })
}

/// `PEGTransformer::GenerateCreateEnumStmt`: `CREATE TEMPORARY TYPE name AS ENUM (SELECT
/// DISTINCT col::VARCHAR FROM source WHERE col IS NOT NULL ORDER BY 1)`, or of the `IN`
/// subquery.
fn generate_create_enum_stmt(entry: CreatePivotEntry) -> Result<SqlStatement> {
    let subselect = if let Some(subquery) = entry.subquery {
        subquery
    } else {
        let mut select_node = entry
            .base
            .ok_or_else(|| Error::internal("PIVOT entry without base query"))?;
        let QueryNodeKind::Select(select) = &mut select_node.kind else {
            return Err(Error::internal("PIVOT entry base is not a SELECT node"));
        };
        select
            .select_list
            .push(ParsedExpression::cast(entry.column.clone(), LogicalType::VARCHAR));
        select.where_clause = Some(Box::new(ParsedExpression::operator(
            ExpressionType::OperatorIsNotNull,
            vec![entry.column],
        )));
        // Order by the column.
        select_node
            .modifiers
            .push(ResultModifier::Distinct(DistinctModifier {
                distinct_on_targets: Vec::new(),
            }));
        select_node.modifiers.push(ResultModifier::Order(OrderModifier {
            orders: vec![OrderByNode {
                order_type: OrderType::Ascending,
                null_order: OrderByNullType::OrderDefault,
                expression: Box::new(ParsedExpression::constant(Value::integer(1))),
            }],
        }));
        select_node
    };
    let info = CreateInfo {
        catalog: String::new().into(),
        schema: String::new().into(),
        temporary: true,
        internal: false,
        on_conflict: OnCreateConflict::ReplaceOnConflict,
        sql: String::new().into(),
        comment: Value::default(),
        tags: CaseInsensitiveMap::default(),
        dependencies: LogicalDependencyList::default(),
        kind: CreateInfoKind::Type(CreateTypeInfo {
            name: entry.enum_name.into(),
            logical_type: LogicalType::INVALID,
            query: Some(Box::new(build::select_statement(subselect))),
        }),
    };
    Ok(SqlStatement::new(SqlStatementKind::Create(Box::new(info))))
}

impl PegTransformer {
    /// `PEGTransformer::ParamCount`.
    const fn param_count(&self) -> u64 {
        self.prepared_statement_parameter_index
    }

    /// `AddPivotEntry`: records an enum type the statement needs.
    fn add_pivot_entry(
        &mut self,
        enum_name: String,
        base: QueryNode,
        column: ParsedExpression,
        subquery: Option<QueryNode>,
        has_parameters: bool,
    ) {
        self.pivot_entries.push(CreatePivotEntry {
            enum_name,
            base: Some(base),
            column,
            subquery,
            has_parameters,
        });
    }

    /// Gives each pivot column without values (`PIVOT ... ON col`) an enum type,
    /// created from the distinct values of `col` in `source` (or of its `IN` subquery).
    fn add_pivot_enums(
        &mut self,
        columns: &mut [PivotColumn],
        source: &TableRef,
        has_parameters: bool,
    ) -> Result<()> {
        for col in columns {
            if !col.pivot_enum.is_empty() || !col.entries.is_empty() {
                continue;
            }
            let [pivot_expression] = col.pivot_expressions.as_slice() else {
                return Err(Error::internal(
                    "PIVOT statement with multiple names in pivot entry!?",
                ));
            };
            let enum_name = format!("__pivot_enum_{}", build::random_uuid());

            let mut new_select = build::select_node();
            *new_select.from_table = source.clone();
            let mut new_select = build::query_node(QueryNodeKind::Select(Box::new(new_select)));
            self.extract_ctes_recursive(&mut new_select.cte_map);
            let column = pivot_expression.clone();
            let subquery = col.subquery.take().map(|subquery| *subquery);
            self.add_pivot_entry(enum_name.clone(), new_select, column, subquery, has_parameters);
            col.pivot_enum = enum_name.into();
        }
        Ok(())
    }

    /// `TransformPivotOn`: `ON PivotColumnList`.
    pub(super) fn transform_pivot_on(&mut self, parse_result: &ParseResult) -> Result<Vec<PivotColumn>> {
        self.transform_child(parse_result.list()?, 1)
    }

    /// `TransformPivotColumnList`: `List(PivotColumnEntry)`; several expressions in one
    /// entry pivot on their `row(...)`, which may not be constant nor hold a subquery.
    pub(super) fn transform_pivot_column_list(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<PivotColumn>> {
        let pivot_column_list = parse_result.list()?.child(0)?.list_elements()?;
        let mut result = Vec::with_capacity(pivot_column_list.len());
        for pivot_column in pivot_column_list {
            let mut col: PivotColumn = self.transform(pivot_column)?;
            if col.pivot_expressions.len() > 1 {
                let row_function = build::function_full(
                    String::new(),
                    "main".to_owned(),
                    "row",
                    std::mem::take(&mut col.pivot_expressions),
                    None,
                    OrderModifier { orders: Vec::new() },
                    false,
                    false,
                    false,
                );
                col.pivot_expressions.push(row_function);
            }
            for expr in &col.pivot_expressions {
                if is_scalar(expr) {
                    return Err(
                        Error::parser(format!("Cannot pivot on constant value \"{expr}\""))
                            .with_position(expr.query_location),
                    );
                }
                if has_subquery(expr) {
                    return Err(Error::parser(format!("Cannot pivot on subquery \"{expr}\""))
                        .with_position(expr.query_location));
                }
            }
            result.push(col);
        }
        Ok(result)
    }

    /// `TransformPivotColumnEntry`: `PivotColumnSubquery / PivotValueList /
    /// PivotColumnEntryInternal`.
    pub(super) fn transform_pivot_column_entry(&mut self, parse_result: &ParseResult) -> Result<PivotColumn> {
        self.transform(parse_result.list()?.choice(0)?)
    }

    /// `TransformPivotColumnSubquery`: `BaseExpression IN Parens(SelectStatementInternal)`.
    pub(super) fn transform_pivot_column_subquery(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<PivotColumn> {
        let list = parse_result.list()?;
        let mut result = pivot_column();
        result.pivot_expressions.push(self.transform_child(list, 0)?);
        let select_subquery = list.child(2)?.parens_content()?;
        let select: SelectStatement = self.transform(select_subquery)?;
        result.subquery = Some(select.node);
        Ok(result)
    }

    /// `TransformPivotColumnEntryInternal`: an expression.
    pub(super) fn transform_pivot_column_entry_internal(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<PivotColumn> {
        let mut result = pivot_column();
        result
            .pivot_expressions
            .push(self.transform_child(parse_result.list()?, 0)?);
        Ok(result)
    }

    /// `TransformPivotStatement`: `PivotKeyword TableRef PivotOn? PivotUsing?
    /// PivotGroupByList?`; without `ON` it is a grouped `SELECT` of the `USING`
    /// aggregates.
    pub(super) fn transform_pivot_statement(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<SelectStatement> {
        let list = parse_result.list()?;

        let current_param_count = self.param_count();
        let source: TableRef = self.transform_child(list, 1)?;
        let next_param_count = self.param_count();
        let has_parameters = next_param_count > current_param_count;
        let pivot_columns = list.optional(2)?;
        let mut select_node = build::select_node();
        let pivot_group = list.optional(4)?;
        let Some(pivot_columns) = pivot_columns else {
            // DuckDB transforms the source a second time here.
            *select_node.from_table = self.transform_child(list, 1)?;
            if let Some(pivot_group) = pivot_group {
                let pivot_group_list: Vec<String> = self.transform(pivot_group)?;
                let mut set = std::collections::BTreeSet::new();
                for (gr, group) in pivot_group_list.into_iter().enumerate() {
                    let colref = ParsedExpression::column_ref([group]);
                    select_node.select_list.push(colref.clone());
                    select_node.group_expressions.push(colref);
                    set.insert(gr as u64);
                }
                select_node.group_sets.push(set);
            }
            let pivot_using: Vec<ParsedExpression> = self.transform_optional(list, 3)?.unwrap_or_default();
            select_node.select_list.extend(pivot_using);
            return Ok(build::select_statement(build::query_node(QueryNodeKind::Select(
                Box::new(select_node),
            ))));
        };
        let mut columns: Vec<PivotColumn> = self.transform(pivot_columns)?;
        self.add_pivot_enums(&mut columns, &source, has_parameters)?;

        // Generate the actual query, including the pivot.
        let mut pivot_ref = pivot_ref(source);
        if let Some(pivot_aggregates) = list.optional(3)? {
            pivot_ref.aggregates = self.transform(pivot_aggregates)?;
        } else {
            // A pivot without aggregates counts.
            pivot_ref
                .aggregates
                .push(ParsedExpression::function("count_star", Vec::new()));
        }
        if let Some(pivot_group) = pivot_group {
            pivot_ref.groups = self.transform(pivot_group)?;
        }
        pivot_ref.pivots = columns;
        Ok(select_star_from_pivot(pivot_ref))
    }

    /// `TransformPivotUsing`: `USING TargetList`.
    pub(super) fn transform_pivot_using(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<ParsedExpression>> {
        self.transform_child(parse_result.list()?, 1)
    }

    /// `TransformUnpivotHeader`: `UnpivotHeaderSingle / UnpivotHeaderList`.
    pub(super) fn transform_unpivot_header(&mut self, parse_result: &ParseResult) -> Result<Vec<String>> {
        self.transform(parse_result.list()?.choice(0)?)
    }

    /// `TransformUnpivotHeaderSingle`: `ColIdOrString`.
    pub(super) fn transform_unpivot_header_single(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<String>> {
        Ok(vec![self.transform_child(parse_result.list()?, 0)?])
    }

    /// `TransformUnpivotHeaderList`: `Parens(List(ColIdOrString))`.
    pub(super) fn transform_unpivot_header_list(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<String>> {
        parse_result
            .list()?
            .child(0)?
            .parens_content()?
            .list_elements()?
            .into_iter()
            .map(|col| self.transform(col))
            .collect()
    }

    /// `TransformIncludeOrExcludeNulls`: `IncludeNulls / ExcludeNulls`.
    pub(super) fn transform_include_or_exclude_nulls(&mut self, parse_result: &ParseResult) -> Result<bool> {
        Self::transform_enum(parse_result.list()?.choice(0)?)
    }

    /// `TransformUnpivotStatement`: `UnpivotKeyword TableRef ON TargetList
    /// IntoNameValues?`; the names and values columns default to `name` and `value`.
    pub(super) fn transform_unpivot_statement(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<SelectStatement> {
        let list = parse_result.list()?;
        let current_param_count = self.param_count();
        let source: TableRef = self.transform_child(list, 1)?;
        let next_param_count = self.param_count();
        let has_parameters = next_param_count > current_param_count;
        let target_list: Vec<ParsedExpression> = self.transform_child(list, 3)?;
        let unpivot_names_opt = list.optional(4)?;
        let mut columns = Vec::new();
        let mut unpivot_names = Vec::new();
        if let Some(unpivot_names_result) = unpivot_names_opt {
            let name_and_values: UnpivotNameValues = self.transform(unpivot_names_result)?;
            let unpivot_list_size = name_and_values.column.unpivot_names.len();
            if unpivot_list_size != 1 {
                return Err(Error::not_implemented(format!(
                    "Only expectd a single value after NAME, got {unpivot_list_size} instead."
                )));
            }
            let mut col = name_and_values.column;
            col.entries.extend(unpivot_entries(target_list));
            columns.push(col);
            unpivot_names = name_and_values.unpivot_names;
        } else {
            let mut col = pivot_column();
            col.unpivot_names.push("name".to_owned().into());
            col.entries.extend(unpivot_entries(target_list));
            columns.push(col);
        }
        self.add_pivot_enums(&mut columns, &source, has_parameters)?;

        let mut pivot_ref = pivot_ref(source);
        if unpivot_names_opt.is_none() {
            pivot_ref.unpivot_names.push("value".to_owned().into());
        } else {
            pivot_ref.unpivot_names = unpivot_names;
        }
        pivot_ref.pivots = columns;
        Ok(select_star_from_pivot(pivot_ref))
    }

    /// `TransformIntoNameValues`: `INTO NAME ColIdOrString ValueOrValues
    /// List(Identifier)`.
    pub(super) fn transform_into_name_values(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<UnpivotNameValues> {
        let list = parse_result.list()?;
        let mut column = pivot_column();
        column.unpivot_names.push(self.transform_child(list, 2)?);
        let mut result = UnpivotNameValues {
            unpivot_names: Vec::new(),
            column,
        };
        for identifier in list.child(4)?.list_elements()? {
            result.unpivot_names.push(identifier.identifier()?.into());
        }
        Ok(result)
    }

    /// `PEGTransformer::CreatePivotStatement`: the `CREATE TYPE ... AS ENUM` statements
    /// of the pivot entries, followed by the statement itself.
    pub(super) fn create_pivot_statement(&mut self, statement: SqlStatement) -> Result<SqlStatement> {
        let mut statements = Vec::with_capacity(self.pivot_entries.len() + 1);
        for pivot in std::mem::take(&mut self.pivot_entries) {
            if pivot.has_parameters {
                return Err(Error::parser(format!(
                    "PIVOT statements with pivot elements extracted from the data cannot have parameters in their \
                     source.\nIn order to use parameters the PIVOT values must be manually specified, e.g.:\nPIVOT \
                     ... ON {} IN (val1, val2, ...)",
                    pivot.column
                )));
            }
            let mut enum_stmt = generate_create_enum_stmt(pivot)?;
            enum_stmt.query = statement_to_string(&enum_stmt)?;
            statements.push(enum_stmt);
        }
        let location = statement.location;
        let length = statement.length;
        let mut statement = statement;
        statement.query = statement_to_string(&statement)?;
        statements.push(statement);
        let mut result = SqlStatement::new(SqlStatementKind::Multi(statements));
        result.location = location;
        result.length = length;
        Ok(result)
    }
}
