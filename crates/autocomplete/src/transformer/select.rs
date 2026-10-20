//! The transform functions of DuckDB's `transform_select.cpp`.

use rustdb_function::cast::ValueCast;
use rustdb_parser::build;

use super::expression::expression_is_empty_star;
use super::prelude::*;

/// `SampleOptions::MAX_SAMPLE_ROWS`.
const MAX_SAMPLE_ROWS: u64 = 1_000_000_000;

/// `MAX_GROUPING_SETS`.
const MAX_GROUPING_SETS: usize = 65535;

/// The SELECT node of a query node DuckDB knows to be one.
fn select_node_mut(node: &mut QueryNode) -> Result<&mut SelectNode> {
    match &mut node.kind {
        QueryNodeKind::Select(select) => Ok(select),
        _ => Err(Error::internal("Expected a SELECT node")),
    }
}

/// A `SelectNode` without anything (`make_uniq<SelectNode>()`).
fn new_select_node() -> QueryNode {
    build::query_node(QueryNodeKind::Select(Box::new(build::select_node())))
}

/// `SELECT * FROM from_table`.
fn select_star_from(from_table: TableRef) -> SelectStatement {
    let mut select = build::select_node();
    select.select_list.push(build::star());
    select.from_table = Box::new(from_table);
    build::select_statement(build::query_node(QueryNodeKind::Select(Box::new(select))))
}

/// A `JoinRef` without sides' details (`make_uniq<JoinRef>()`).
fn new_join(ref_type: JoinRefType, join_type: JoinType, right: TableRef) -> JoinRef {
    JoinRef {
        left: Box::new(build::empty_table_ref()),
        right: Box::new(right),
        condition: None,
        join_type,
        ref_type,
        using_columns: Vec::new(),
        delim_flipped: false,
        duplicate_eliminated_columns: Vec::new(),
        is_implicit: false,
    }
}

/// `BaseTableRef(TableDescription(catalog, schema, table))`.
fn base_table(catalog: &str, schema: &str, table: &str) -> BaseTable {
    BaseTable(build::table_ref(TableRefKind::BaseTable(Box::new(
        BaseTableRef {
            schema_name: schema.to_owned(),
            table_name: table.to_owned(),
            column_name_alias: Vec::new(),
            catalog_name: catalog.to_owned(),
            at_clause: None,
        },
    ))))
}

/// The column name aliases of a table ref that has them.
fn column_name_alias_mut(table_ref: &mut TableRef) -> Option<&mut Vec<String>> {
    match &mut table_ref.kind {
        TableRefKind::BaseTable(base) => Some(&mut base.column_name_alias),
        TableRefKind::Subquery(subquery) => Some(&mut subquery.column_name_alias),
        TableRefKind::TableFunction(function) => Some(&mut function.column_name_alias),
        TableRefKind::Pivot(pivot) => Some(&mut pivot.column_name_alias),
        _ => None,
    }
}

/// `table_ref->alias = alias.name; table_ref->column_name_alias = alias.column_name_alias`.
fn apply_table_alias(table_ref: &mut TableRef, alias: TableAlias) {
    table_ref.alias = alias.name;
    if let Some(column_name_alias) = column_name_alias_mut(table_ref) {
        *column_name_alias = alias.column_name_alias;
    }
}

/// `IsConditionlessJoin`: a CROSS, POSITIONAL or NATURAL join without ON / USING.
fn is_conditionless_join(join: &JoinRef) -> bool {
    join.condition.is_none()
        && join.using_columns.is_empty()
        && matches!(
            join.ref_type,
            JoinRefType::Cross | JoinRefType::Positional | JoinRefType::Natural
        )
}

/// `ReassociateJoins`: turns the right-associative join trees of the grammar into
/// left-associative ones, rotating left while the root is a conditionless join whose
/// right side is a join.
fn reassociate_joins(mut root: TableRef) -> TableRef {
    loop {
        let TableRefKind::Join(current) = &mut root.kind else {
            return root;
        };
        if !is_conditionless_join(current) || !matches!(current.right.kind, TableRefKind::Join(_)) {
            return root;
        }
        // current(left=A, right=inner(left=B, right=C)) => inner(left=current(left=A, right=B), right=C)
        let mut inner = std::mem::replace(&mut current.right, Box::new(build::empty_table_ref()));
        let TableRefKind::Join(inner_join) = &mut inner.kind else {
            return root;
        };
        current.right = std::mem::replace(&mut inner_join.left, Box::new(build::empty_table_ref()));
        *inner_join.left = root;
        root = *inner;
    }
}

/// `RHSTableRefHasJoinOrPivot`: whether the right side of a `JoinOrPivot` has join or
/// pivot entries of its own (PEG right recursion rather than a parenthesized join).
fn rhs_table_ref_has_join_or_pivot(join_or_pivot: &ParseResult) -> Result<bool> {
    let join_clause = join_or_pivot.list()?.choice(0)?.list()?;
    let join = join_clause.choice(0)?.list()?;
    // The table ref of a JoinWithoutOnClause is its third child.
    join.list(2)?.has(1)
}

/// `ConstructConstantFromExpression`: the value of a constant expression, including
/// `struct_pack`, `list_value` and `map` calls of constants and casts of constants;
/// `None` for anything else.
pub(super) fn construct_constant_from_expression(expression: &ParsedExpression) -> Result<Option<Value>> {
    match &expression.kind {
        ParsedExpressionKind::Function(function)
            if expression.expression_type == ExpressionType::Function =>
        {
            match function.function_name.as_str() {
                "struct_pack" => {
                    let mut names: Vec<&str> = Vec::with_capacity(function.children.len());
                    let mut values = Vec::with_capacity(function.children.len());
                    for child in &function.children {
                        if names.iter().any(|name| *name == child.alias) {
                            return Err(Error::binder(format!(
                                "Duplicate struct entry name \"{}\"",
                                child.alias
                            )));
                        }
                        names.push(&child.alias);
                        let Some(value) = construct_constant_from_expression(child)? else {
                            return Ok(None);
                        };
                        values.push((child.alias.clone(), value));
                    }
                    Ok(Some(Value::struct_value(values)))
                }
                "list_value" => {
                    let mut values = Vec::with_capacity(function.children.len());
                    for child in &function.children {
                        let Some(value) = construct_constant_from_expression(child)? else {
                            return Ok(None);
                        };
                        values.push(value);
                    }
                    let mut child_type = LogicalType::SQLNULL;
                    for value in &values {
                        child_type = LogicalType::force_max_logical_type(&child_type, value.logical_type());
                    }
                    Ok(Some(Value::list(child_type, values)))
                }
                "map" => {
                    let (Some(keys), Some(values)) = (function.children.first(), function.children.get(1))
                    else {
                        return Ok(None);
                    };
                    let Some(keys) = construct_constant_from_expression(keys)? else {
                        return Ok(None);
                    };
                    let Some(values) = construct_constant_from_expression(values)? else {
                        return Ok(None);
                    };
                    let key_type = keys
                        .logical_type()
                        .child_type()
                        .cloned()
                        .unwrap_or(LogicalType::SQLNULL);
                    let value_type = values
                        .logical_type()
                        .child_type()
                        .cloned()
                        .unwrap_or(LogicalType::SQLNULL);
                    let entries = keys
                        .children()
                        .unwrap_or_default()
                        .iter()
                        .cloned()
                        .zip(values.children().unwrap_or_default().iter().cloned())
                        .collect();
                    Ok(Some(Value::map(key_type, value_type, entries)))
                }
                _ => Ok(None),
            }
        }
        ParsedExpressionKind::Constant(constant)
            if expression.expression_type == ExpressionType::ValueConstant =>
        {
            Ok(Some(constant.value.clone()))
        }
        ParsedExpressionKind::Cast(cast) if expression.expression_type == ExpressionType::OperatorCast => {
            let Some(value) = construct_constant_from_expression(&cast.child)? else {
                return Ok(None);
            };
            value.try_cast(&cast.cast_type).map(Some).map_err(|_| {
                Error::conversion(format!(
                    "Unable to cast {} to {}",
                    value.to_varchar().unwrap_or_default(),
                    cast.cast_type.id().name()
                ))
            })
        }
        _ => Ok(None),
    }
}

/// `TransformPivotInList`: the values a PIVOT IN entry stands for, if they are
/// constants (or unqualified names).
fn transform_pivot_in_list(expression: &ParsedExpression, entry: &mut PivotColumnEntry) -> Result<bool> {
    match &expression.kind {
        ParsedExpressionKind::ColumnRef(column_ref)
            if expression.expression_type == ExpressionType::ColumnRef =>
        {
            if column_ref.column_names.len() > 1 {
                return Err(
                    Error::parser("PIVOT IN list cannot contain qualified column references")
                        .with_position(expression.query_location),
                );
            }
            entry.values.push(Value::varchar(
                column_ref.column_names.last().cloned().unwrap_or_default(),
            ));
            Ok(true)
        }
        ParsedExpressionKind::Function(function)
            if expression.expression_type == ExpressionType::Function =>
        {
            if function.function_name != "row" {
                return Ok(false);
            }
            for child in &function.children {
                if !transform_pivot_in_list(child, entry)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        _ => match construct_constant_from_expression(expression)? {
            Some(value) => {
                entry.values.push(value);
                Ok(true)
            }
            None => Ok(false),
        },
    }
}

/// `VerifyLimitOffset`: the LIMIT / OFFSET modifier, if any.
fn verify_limit_offset(
    limit: LimitPercentResult,
    offset: LimitPercentResult,
) -> Result<Option<ResultModifier>> {
    if offset.is_percent {
        return Err(Error::parser("Percentage for offsets are not supported."));
    }
    if limit.is_percent {
        return Ok(Some(ResultModifier::LimitPercent(LimitPercentModifier {
            limit: limit.expression.map(Box::new),
            offset: offset.expression.map(Box::new),
        })));
    }
    if limit.expression.is_none() && offset.expression.is_none() {
        return Ok(None);
    }
    Ok(Some(ResultModifier::Limit(LimitModifier {
        limit: limit.expression.map(Box::new),
        offset: offset.expression.map(Box::new),
    })))
}

/// `CheckGroupingSetMax`.
fn check_grouping_set_max(count: usize) -> Result<()> {
    if count > MAX_GROUPING_SETS {
        return Err(Error::parser(format!(
            "Maximum grouping set count of {MAX_GROUPING_SETS} exceeded"
        )));
    }
    Ok(())
}

/// `CheckGroupingSetCubes`.
fn check_grouping_set_cubes(current_count: usize, cube_count: usize) -> Result<()> {
    let mut combinations = 1usize;
    for _ in 0..cube_count {
        combinations = combinations.saturating_mul(2);
        check_grouping_set_max(current_count.saturating_add(combinations))?;
    }
    Ok(())
}

/// `AddCubeSets`: every combination of the cube's sets, added to `current_set`.
fn add_cube_sets(
    current_set: &GroupingSet,
    cube_sets: &[GroupingSet],
    result_sets: &mut Vec<GroupingSet>,
    start_idx: usize,
) -> Result<()> {
    check_grouping_set_max(result_sets.len())?;
    result_sets.push(current_set.clone());
    for (k, cube_set) in cube_sets.iter().enumerate().skip(start_idx) {
        let mut child_set = current_set.clone();
        child_set.extend(cube_set.iter().copied());
        add_cube_sets(&child_set, cube_sets, result_sets, k + 1)?;
    }
    Ok(())
}

/// A set of GROUP BY expression indexes (DuckDB's `GroupingSet`).
type GroupingSet = std::collections::BTreeSet<u64>;

/// `GroupingExpressionMap`: the GROUP BY expressions seen so far (compared with
/// `ParsedExpression::Equals`), by index.
#[derive(Debug, Default)]
struct GroupingExpressionMap {
    expressions: Vec<(u64, ParsedExpression)>,
}

impl GroupingExpressionMap {
    /// The index of an expression equal to `expression`.
    fn find(&self, expression: &ParsedExpression) -> Option<usize> {
        let hash = rustdb_parser::equality::expression_hash(expression);
        self.expressions.iter().position(|(other_hash, other)| {
            *other_hash == hash && rustdb_parser::equality::expressions_equal(other, expression)
        })
    }
}

/// `AddGroupByExpression`: the indexes of a GROUP BY expression (the elements of a row
/// constructor), each expression kept once.
fn add_group_by_expression(
    expression: ParsedExpression,
    map: &mut GroupingExpressionMap,
    result: &mut GroupByNode,
    result_set: &mut Vec<u64>,
) {
    if expression.expression_type == ExpressionType::Function
        && let ParsedExpressionKind::Function(function) = &expression.kind
        && function.function_name == "row"
    {
        let ParsedExpressionKind::Function(function) = expression.kind else {
            return;
        };
        for child in function.children {
            add_group_by_expression(child, map, result, result_set);
        }
        return;
    }
    let index = if let Some(index) = map.find(&expression) {
        index
    } else {
        let index = result.group_expressions.len();
        map.expressions.push((
            rustdb_parser::equality::expression_hash(&expression),
            expression.clone(),
        ));
        result.group_expressions.push(expression);
        index
    };
    result_set.push(index as u64);
}

/// `SampleMethod` by name (`EnumUtil::FromString<SampleMethod>`).
fn sample_method_from_string(name: &str) -> Result<SampleMethod> {
    SampleMethod::from_name(name).ok_or_else(|| {
        let candidates: Vec<String> = SampleMethod::ALL
            .iter()
            .map(|method| method.name().to_owned())
            .collect();
        let closest = rustdb_common::string_util::top_n_jaro_winkler(&candidates, name);
        Error::not_implemented(format!(
            "Enum value: unrecognized value \"{name}\" for enum \"SampleMethod\"\n{}",
            rustdb_common::string_util::candidates_message(&closest, "Candidates")
        ))
    })
}

/// `ToRecursiveCTE`: a `UNION [ALL]` CTE that refers to itself becomes a recursive CTE
/// node; any other node is kept.
pub(super) fn to_recursive_cte(
    node: QueryNode,
    name: &str,
    aliases: &[String],
    key_targets: &[ParsedExpression],
) -> Result<QueryNode> {
    let QueryNodeKind::SetOperation(set_node) = &node.kind else {
        return Ok(node);
    };
    if set_node.setop_type != SetOperationType::Union {
        return Ok(node);
    }
    if set_node.children.len() < 2 {
        return Err(Error::parser(
            "Expected at least two children to set operation node in recursive CTE",
        ));
    }
    let QueryNode {
        modifiers,
        cte_map,
        kind: QueryNodeKind::SetOperation(mut set_node),
    } = node
    else {
        return Err(Error::internal("Expected a set operation node"));
    };
    let union_all = set_node.setop_all;
    let right = set_node
        .children
        .pop()
        .ok_or_else(|| Error::internal("A set operation without children"))?;
    // An n-ary node splits into everything but the last operand and the last one, like
    // the left-recursive tree of the grammar.
    let left = if set_node.children.len() == 1 {
        set_node
            .children
            .pop()
            .ok_or_else(|| Error::internal("A set operation without children"))?
    } else {
        QueryNode {
            modifiers,
            cte_map: CommonTableExpressionMap::default(),
            kind: QueryNodeKind::SetOperation(set_node),
        }
    };
    Ok(QueryNode {
        modifiers: Vec::new(),
        cte_map,
        kind: QueryNodeKind::RecursiveCte(Box::new(RecursiveCteNode {
            cte_name: name.to_owned(),
            union_all,
            left: Box::new(left),
            right: Box::new(right),
            aliases: aliases.to_vec(),
            key_targets: key_targets.to_vec(),
        })),
    })
}

impl PegTransformer {
    /// `TransformSelectStatement`.
    pub(super) fn transform_select_statement(&mut self, parse_result: &ParseResult) -> Result<SqlStatement> {
        let select: SelectStatement = self.transform_child(parse_result.list()?, 0)?;
        Ok(SqlStatement::new(SqlStatementKind::Select(select)))
    }

    /// `TransformSelectStatementInternal`: `[WITH ...] select [ORDER BY / LIMIT ...]`.
    pub(super) fn transform_select_statement_internal(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<SelectStatement> {
        let list = parse_result.list()?;
        let cte_map: CommonTableExpressionMap = self.transform_optional(list, 0)?.unwrap_or_default();
        if !cte_map.map.is_empty() {
            self.stored_cte_map.push(cte_map.clone());
        }
        let mut select: SelectStatement = self.transform_child(list, 1)?;
        if !cte_map.map.is_empty() {
            select.node.cte_map = cte_map;
        }
        let modifiers: Vec<ResultModifier> = self.transform_optional(list, 2)?.unwrap_or_default();
        select.node.modifiers.extend(modifiers);
        let QueryNodeKind::Select(select_node) = &select.node.kind else {
            return Ok(select);
        };
        let TableRefKind::Show(show_ref) = &select_node.from_table.kind else {
            return Ok(select);
        };
        if !select.node.cte_map.map.is_empty() {
            return Err(Error::parser(format!(
                "{} with CTE not allowed - wrap the statement in a subquery instead",
                show_ref.show_type.name()
            )));
        }
        if !select.node.modifiers.is_empty() {
            return Err(Error::parser(format!(
                "{} with ORDER BY not allowed - wrap the statement in a subquery instead",
                show_ref.show_type.name()
            )));
        }
        Ok(select)
    }

    /// `TransformSelectSetOpChain`: `a UNION b EXCEPT c ...`; consecutive unions of the
    /// same kind share one node.
    pub(super) fn transform_select_set_op_chain(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<SelectStatement> {
        let list = parse_result.list()?;
        let mut select: SelectStatement = self.transform_child(list, 0)?;
        let Some(operations) = list.optional(1)? else {
            return Ok(select);
        };
        for operation in operations.repeat()? {
            let operation = operation.list()?;
            let mut setop: QueryNode = self.transform_child(operation, 0)?;
            let right: SelectStatement = self.transform_child(operation, 1)?;
            let setop_node = set_operation_mut(&mut setop)?;
            if select.node.modifiers.is_empty()
                && select.node.cte_map.map.is_empty()
                && let QueryNodeKind::SetOperation(existing) = &mut select.node.kind
                && existing.setop_type == setop_node.setop_type
                && existing.setop_all == setop_node.setop_all
                && matches!(
                    existing.setop_type,
                    SetOperationType::Union | SetOperationType::UnionByName
                )
            {
                existing.children.push(*right.node);
                continue;
            }
            let left = std::mem::replace(&mut select.node, Box::new(new_select_node()));
            setop_node.children.push(*left);
            setop_node.children.push(*right.node);
            select.node = Box::new(setop);
        }
        Ok(select)
    }

    /// `TransformIntersectChain`: `a INTERSECT b ...`.
    pub(super) fn transform_intersect_chain(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<SelectStatement> {
        let list = parse_result.list()?;
        let mut select: SelectStatement = self.transform(list.child(0)?)?;
        let Some(intersections) = list.optional(1)? else {
            return Ok(select);
        };
        for intersection in intersections.repeat()? {
            let intersection = intersection.list()?;
            let mut node: QueryNode = self.transform(intersection.child(0)?)?;
            let right: SelectStatement = self.transform(intersection.child(1)?)?;
            let left = std::mem::replace(&mut select.node, Box::new(new_select_node()));
            let setop = set_operation_mut(&mut node)?;
            setop.children.push(*left);
            setop.children.push(*right.node);
            select.node = Box::new(node);
        }
        Ok(select)
    }

    /// `TransformSetIntersectClause`: `INTERSECT [ALL | DISTINCT]`.
    pub(super) fn transform_set_intersect_clause(&mut self, parse_result: &ParseResult) -> Result<QueryNode> {
        let distinct: Option<bool> = self.transform_optional(parse_result.list()?, 1)?;
        Ok(set_operation_node(
            SetOperationType::Intersect,
            distinct.is_some_and(|distinct| !distinct),
        ))
    }

    /// `TransformSelectAtom`.
    pub(super) fn transform_select_atom(&mut self, parse_result: &ParseResult) -> Result<SelectStatement> {
        self.transform(parse_result.list()?.choice(0)?)
    }

    /// `TransformSetopClause`: `UNION / EXCEPT [ALL | DISTINCT] [BY NAME]`.
    pub(super) fn transform_setop_clause(&mut self, parse_result: &ParseResult) -> Result<QueryNode> {
        let list = parse_result.list()?;
        let mut setop_type: SetOperationType = self.transform_child(list, 0)?;
        let distinct: Option<bool> = self.transform_optional(list, 1)?;
        if list.has(2)? {
            if setop_type == SetOperationType::Union {
                setop_type = SetOperationType::UnionByName;
            } else {
                return Err(Error::parser(format!(
                    "Invalid combination of {} and BY NAME",
                    setop_type.name()
                )));
            }
        }
        Ok(set_operation_node(
            setop_type,
            distinct.is_some_and(|distinct| !distinct),
        ))
    }

    /// `TransformDistinctOrAll`: whether `DISTINCT`.
    pub(super) fn transform_distinct_or_all(&mut self, parse_result: &ParseResult) -> Result<bool> {
        Ok(parse_result
            .list()?
            .choice(0)?
            .keyword()?
            .eq_ignore_ascii_case("distinct"))
    }

    /// `TransformSetopType`.
    pub(super) fn transform_setop_type(&mut self, parse_result: &ParseResult) -> Result<SetOperationType> {
        Self::transform_enum(parse_result.list()?.choice(0)?)
    }

    /// `TransformSelectParens`: `(select)`.
    pub(super) fn transform_select_parens(&mut self, parse_result: &ParseResult) -> Result<SelectStatement> {
        self.transform(parse_result.list()?.child(0)?.parens_content()?)
    }

    /// `TransformSelectStatementType`.
    pub(super) fn transform_select_statement_type(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<SelectStatement> {
        self.transform(parse_result.list()?.choice(0)?)
    }

    /// `TransformOptionalParensSimpleSelect`.
    pub(super) fn transform_optional_parens_simple_select(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<SelectStatement> {
        self.transform(parse_result.list()?.choice(0)?)
    }

    /// `TransformSimpleSelectParens`: `(simple select)`.
    pub(super) fn transform_simple_select_parens(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<SelectStatement> {
        self.transform(parse_result.list()?.child(0)?.parens_content()?)
    }

    /// `TransformSimpleSelect`: `SELECT ... FROM ... WHERE ... GROUP BY ... HAVING ...
    /// WINDOW ... QUALIFY ... USING SAMPLE ...`; the named windows are known while the
    /// rest is transformed.
    pub(super) fn transform_simple_select(&mut self, parse_result: &ParseResult) -> Result<SelectStatement> {
        let list = parse_result.list()?;
        if let Some(window_clause) = list.optional(4)? {
            let windows: Vec<ParsedExpression> = self.transform(window_clause)?;
            for mut window in windows {
                let window_name = std::mem::take(&mut window.alias);
                if self.window_clauses.contains_key(&window_name) {
                    return Err(Error::parser(format!(
                        "window \"{window_name}\" is already defined"
                    )));
                }
                self.window_clauses.insert(&window_name, WindowExpr(window));
            }
        }
        let mut node: QueryNode = self.transform_child(list, 0)?;
        let where_clause: Option<ParsedExpression> = self.transform_optional(list, 1)?;
        let groups: Option<GroupByNode> = self.transform_optional(list, 2)?;
        let having: Option<ParsedExpression> = self.transform_optional(list, 3)?;
        let qualify: Option<ParsedExpression> = self.transform_optional(list, 5)?;
        let sample: Option<SampleOptions> = self.transform_optional(list, 6)?;
        let select_node = select_node_mut(&mut node)?;
        if let Some(where_clause) = where_clause {
            select_node.where_clause = Some(Box::new(where_clause));
        }
        if let Some(mut groups) = groups {
            if groups.group_expressions.len() == 1 && expression_is_empty_star(&groups.group_expressions[0]) {
                // GROUP BY ALL.
                select_node.aggregate_handling = AggregateHandling::ForceAggregates;
                groups.group_expressions.clear();
                groups.grouping_sets.clear();
            }
            select_node.group_expressions = groups.group_expressions;
            select_node.group_sets = groups.grouping_sets;
        }
        if let Some(having) = having {
            select_node.having = Some(Box::new(having));
        }
        if let Some(qualify) = qualify {
            select_node.qualify = Some(Box::new(qualify));
        }
        if let Some(sample) = sample {
            select_node.sample = Some(Box::new(sample));
        }
        self.window_clauses = CaseInsensitiveMap::default();
        Ok(build::select_statement(node))
    }

    /// `TransformSelectFrom`.
    pub(super) fn transform_select_from(&mut self, parse_result: &ParseResult) -> Result<QueryNode> {
        self.transform(parse_result.list()?.choice(0)?)
    }

    /// `TransformSelectFromClause`: `SELECT ... [FROM ...]`.
    pub(super) fn transform_select_from_clause(&mut self, parse_result: &ParseResult) -> Result<QueryNode> {
        let list = parse_result.list()?;
        let mut node: QueryNode = self.transform_child(list, 0)?;
        let from_table: Option<TableRef> = self.transform_optional(list, 1)?;
        *select_node_mut(&mut node)?.from_table = from_table.unwrap_or_else(build::empty_table_ref);
        Ok(node)
    }

    /// `TransformFromSelectClause`: `FROM ... [SELECT ...]`; without a SELECT, `*`.
    pub(super) fn transform_from_select_clause(&mut self, parse_result: &ParseResult) -> Result<QueryNode> {
        let list = parse_result.list()?;
        let from_table: TableRef = self.transform_child(list, 0)?;
        let mut node = if let Some(select) = list.optional(1)? {
            self.transform(select)?
        } else {
            let mut node = new_select_node();
            select_node_mut(&mut node)?.select_list.push(build::star());
            node
        };
        *select_node_mut(&mut node)?.from_table = from_table;
        Ok(node)
    }

    /// `TransformFromClause`: `FROM a, b, ...` is an implicit cross product.
    pub(super) fn transform_from_clause(&mut self, parse_result: &ParseResult) -> Result<TableRef> {
        let table_refs = parse_result.list()?.child(1)?.list_elements()?;
        let mut table_refs = table_refs.into_iter();
        let first = table_refs
            .next()
            .ok_or_else(|| Error::internal("A FROM clause without tables"))?;
        let mut result: TableRef = self.transform(first)?;
        for table_ref in table_refs {
            let right: TableRef = self.transform(table_ref)?;
            let mut cross_product = new_join(JoinRefType::Cross, JoinType::Inner, right);
            cross_product.left = Box::new(result);
            cross_product.is_implicit = true;
            result = build::table_ref(TableRefKind::Join(Box::new(cross_product)));
        }
        Ok(result)
    }

    /// `TransformSelectClause`: `SELECT [DISTINCT [ON (...)] | ALL] targets`.
    pub(super) fn transform_select_clause(&mut self, parse_result: &ParseResult) -> Result<QueryNode> {
        let list = parse_result.list()?;
        let mut node = new_select_node();
        let distinct: Option<DistinctClause> = self.transform_optional(list, 1)?;
        if let Some(distinct) = distinct
            && distinct.is_distinct
        {
            node.modifiers.push(ResultModifier::Distinct(DistinctModifier {
                distinct_on_targets: distinct.distinct_targets,
            }));
        }
        let Some(targets) = list.optional(2)? else {
            return Err(Error::parser("SELECT clause without selection list"));
        };
        let targets: Vec<ParsedExpression> = self.transform(targets)?;
        select_node_mut(&mut node)?.select_list.extend(targets);
        Ok(node)
    }

    /// `TransformDistinctClause`.
    pub(super) fn transform_distinct_clause(&mut self, parse_result: &ParseResult) -> Result<DistinctClause> {
        self.transform(parse_result.list()?.choice(0)?)
    }

    /// `TransformDistinctOn`: `DISTINCT [ON (targets)]`.
    pub(super) fn transform_distinct_on(&mut self, parse_result: &ParseResult) -> Result<DistinctClause> {
        Ok(DistinctClause {
            is_distinct: true,
            distinct_targets: self
                .transform_optional(parse_result.list()?, 1)?
                .unwrap_or_default(),
        })
    }

    /// `TransformDistinctAll`: `ALL`.
    pub(super) fn transform_distinct_all(&mut self, _parse_result: &ParseResult) -> Result<DistinctClause> {
        Ok(DistinctClause::default())
    }

    /// `TransformDistinctOnTargets`: `ON (expr, ...)`.
    pub(super) fn transform_distinct_on_targets(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<ParsedExpression>> {
        self.transform_expressions(parse_result.list()?.child(1)?.parens_content()?)
    }

    /// The expressions of `List(Expression)`.
    fn transform_expressions(&mut self, list: &ParseResult) -> Result<Vec<ParsedExpression>> {
        list.list_elements()?
            .into_iter()
            .map(|element| self.transform(element))
            .collect()
    }

    /// `TransformFunctionArgument`: an expression, or `name := value` as an aliased one.
    pub(super) fn transform_function_argument(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        let choice = parse_result.list()?.choice(0)?;
        if choice.name == "NamedParameter" {
            let parameter: MacroParameter = self.transform(choice)?;
            let expression = parameter
                .expression
                .ok_or_else(|| Error::internal("A named parameter without a value"))?;
            return Ok(expression.with_alias(parameter.name));
        }
        self.transform(choice)
    }

    /// `TransformNamedParameter`: `name [type] := value`.
    pub(super) fn transform_named_parameter(&mut self, parse_result: &ParseResult) -> Result<MacroParameter> {
        let list = parse_result.list()?;
        let expression: ParsedExpression = self.transform_child(list, 3)?;
        let name: String = self.transform_child(list, 0)?;
        let mut parameter = MacroParameter {
            expression: Some(expression),
            name,
            is_default: true,
            ..MacroParameter::default()
        };
        if let Some(ty) = self.transform_optional(list, 1)? {
            parameter.ty = ty;
        }
        Ok(parameter)
    }

    /// `TransformTableFunctionArguments`: `(argument, ...)`.
    pub(super) fn transform_table_function_arguments(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<ParsedExpression>> {
        match parse_result.list()?.child(0)?.parens_content()?.optional()? {
            Some(arguments) => self.transform_expressions(arguments),
            None => Ok(Vec::new()),
        }
    }

    /// `TransformBaseTableName`: a (qualified) table name.
    pub(super) fn transform_base_table_name(&mut self, parse_result: &ParseResult) -> Result<BaseTable> {
        let choice = parse_result.list()?.choice(0)?;
        if choice.result_type() == ParseResultType::Identifier {
            return Ok(base_table("", "", choice.identifier()?));
        }
        self.transform(choice)
    }

    /// `TransformSchemaReservedTable`: `schema.table`.
    pub(super) fn transform_schema_reserved_table(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<BaseTable> {
        let list = parse_result.list()?;
        let schema: String = self.transform_child(list, 0)?;
        Ok(base_table("", &schema, list.identifier(1)?))
    }

    /// `TransformCatalogReservedSchemaTable`: `catalog.schema.table`.
    pub(super) fn transform_catalog_reserved_schema_table(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<BaseTable> {
        let list = parse_result.list()?;
        let catalog: String = self.transform_child(list, 0)?;
        let schema: String = self.transform_child(list, 1)?;
        Ok(base_table(&catalog, &schema, list.identifier(2)?))
    }

    /// `TransformSchemaQualification`: the schema of `schema.name`.
    pub(super) fn transform_schema_qualification(&mut self, parse_result: &ParseResult) -> Result<String> {
        Ok(parse_result.list()?.identifier(0)?.to_owned())
    }

    /// `TransformCatalogQualification`: the catalog of `catalog.schema.name`.
    pub(super) fn transform_catalog_qualification(&mut self, parse_result: &ParseResult) -> Result<String> {
        Ok(parse_result.list()?.identifier(0)?.to_owned())
    }

    /// `TransformQualifiedName`.
    pub(super) fn transform_qualified_name(&mut self, parse_result: &ParseResult) -> Result<QualifiedName> {
        self.transform(parse_result.list()?.choice(0)?)
    }

    /// `TransformCatalogReservedSchemaIdentifierOrStringLiteral`.
    pub(super) fn transform_catalog_reserved_schema_identifier_or_string_literal(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<QualifiedName> {
        self.transform_three_part_name(parse_result)
    }

    /// `TransformCatalogReservedSchemaIdentifier`.
    pub(super) fn transform_catalog_reserved_schema_identifier(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<QualifiedName> {
        self.transform_three_part_name(parse_result)
    }

    /// `catalog.schema.name` of three rules.
    fn transform_three_part_name(&mut self, parse_result: &ParseResult) -> Result<QualifiedName> {
        let list = parse_result.list()?;
        Ok(QualifiedName {
            catalog: self.transform_child(list, 0)?,
            schema: self.transform_child(list, 1)?,
            name: self.transform_child(list, 2)?,
        })
    }

    /// `TransformSchemaReservedIdentifierOrStringLiteral`: `schema.name`.
    pub(super) fn transform_schema_reserved_identifier_or_string_literal(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<QualifiedName> {
        let list = parse_result.list()?;
        Ok(QualifiedName {
            catalog: String::new(),
            schema: self.transform_child(list, 0)?,
            name: self.transform_child(list, 1)?,
        })
    }

    /// `TransformReservedIdentifierOrStringLiteral`.
    pub(super) fn transform_reserved_identifier_or_string_literal(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<String> {
        let choice = parse_result.list()?.choice(0)?;
        if choice.result_type() == ParseResultType::Identifier {
            return Ok(choice.identifier()?.to_owned());
        }
        self.transform(choice)
    }

    /// `TransformTableNameIdentifierOrStringLiteral`: an unqualified name.
    pub(super) fn transform_table_name_identifier_or_string_literal(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<QualifiedName> {
        Ok(QualifiedName {
            name: self.transform_child(parse_result.list()?, 0)?,
            ..QualifiedName::default()
        })
    }

    /// `TransformWhereClause`: `WHERE expr`.
    pub(super) fn transform_where_clause(&mut self, parse_result: &ParseResult) -> Result<ParsedExpression> {
        self.transform_child(parse_result.list()?, 1)
    }

    /// `TransformTargetList`: the select list.
    pub(super) fn transform_target_list(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<ParsedExpression>> {
        self.transform_expressions(parse_result.list()?.child(0)?)
    }

    /// `TransformAliasedExpression`.
    pub(super) fn transform_aliased_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        self.transform(parse_result.list()?.choice(0)?)
    }

    /// `TransformExpressionAsCollabel`: `expr AS alias`.
    pub(super) fn transform_expression_as_collabel(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        let list = parse_result.list()?;
        let expression: ParsedExpression = self.transform_child(list, 0)?;
        let alias: String = self.transform_child(list, 2)?;
        Ok(expression.with_alias(alias))
    }

    /// `TransformColIdExpression`: `alias: expr`.
    pub(super) fn transform_col_id_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        let list = parse_result.list()?;
        let expression: ParsedExpression = self.transform_child(list, 2)?;
        let alias: String = self.transform_child(list, 0)?;
        Ok(expression.with_alias(alias))
    }

    /// `TransformExpressionOptIdentifier`: `expr [alias]`.
    pub(super) fn transform_expression_opt_identifier(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        let list = parse_result.list()?;
        let mut expression: ParsedExpression = self.transform_child(list, 0)?;
        if let Some(alias) = list.optional(1)? {
            alias.identifier()?.clone_into(&mut expression.alias);
        }
        Ok(expression)
    }

    /// `TransformTableAlias`.
    pub(super) fn transform_table_alias(&mut self, parse_result: &ParseResult) -> Result<TableAlias> {
        self.transform(parse_result.list()?.choice(0)?)
    }

    /// `TransformTableAliasAs`: `AS name [(columns)]`.
    pub(super) fn transform_table_alias_as(&mut self, parse_result: &ParseResult) -> Result<TableAlias> {
        let list = parse_result.list()?;
        let name: QualifiedName = self.transform_child(list, 1)?;
        Ok(TableAlias {
            name: name.name,
            column_name_alias: self.transform_optional(list, 2)?.unwrap_or_default(),
        })
    }

    /// `TransformTableAliasWithoutAs`: `name [(columns)]`.
    pub(super) fn transform_table_alias_without_as(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<TableAlias> {
        let list = parse_result.list()?;
        Ok(TableAlias {
            name: list.identifier(0)?.to_owned(),
            column_name_alias: self.transform_optional(list, 1)?.unwrap_or_default(),
        })
    }

    /// `TransformColumnAliases`: `(name, ...)`.
    pub(super) fn transform_column_aliases(&mut self, parse_result: &ParseResult) -> Result<Vec<String>> {
        parse_result
            .list()?
            .child(0)?
            .parens_content()?
            .list_elements()?
            .into_iter()
            .map(|alias| self.transform(alias))
            .collect()
    }

    /// `TransformTableRef`: a table ref with its joins and pivots.
    pub(super) fn transform_table_ref(&mut self, parse_result: &ParseResult) -> Result<TableRef> {
        let list = parse_result.list()?;
        let mut inner: TableRef = self.transform_child(list, 0)?;
        let Some(joins_or_pivots) = list.optional(1)? else {
            return Ok(inner);
        };
        for join_or_pivot in joins_or_pivots.repeat()? {
            let mut transformed: TableRef = self.transform(join_or_pivot)?;
            match &mut transformed.kind {
                TableRefKind::Join(join) => {
                    *join.left = inner;
                    inner = if is_conditionless_join(join) && rhs_table_ref_has_join_or_pivot(join_or_pivot)?
                    {
                        reassociate_joins(transformed)
                    } else {
                        transformed
                    };
                }
                TableRefKind::Pivot(pivot) => {
                    *pivot.source = inner;
                    inner = transformed;
                }
                _ => {
                    return Err(Error::not_implemented(format!(
                        "Unsupported TableRef type encountered: {}",
                        transformed.table_reference_type().name()
                    )));
                }
            }
        }
        Ok(inner)
    }

    /// `TransformJoinOrPivot`.
    pub(super) fn transform_join_or_pivot(&mut self, parse_result: &ParseResult) -> Result<TableRef> {
        self.transform(parse_result.list()?.choice(0)?)
    }

    /// `TransformTableUnpivotClause`: `UNPIVOT [INCLUDE/EXCLUDE NULLS] (names FOR ...) [alias]`,
    /// without its source.
    pub(super) fn transform_table_unpivot_clause(&mut self, parse_result: &ParseResult) -> Result<TableRef> {
        let list = parse_result.list()?;
        let mut result = new_pivot_ref();
        if let Some(include_nulls) = self.transform_optional(list, 1)? {
            result.include_nulls = include_nulls;
        }
        let inner = list.child(2)?.parens_content()?.list()?;
        result.unpivot_names = self.transform(inner.child(0)?)?;
        for pivot_value in inner.repeat(2)? {
            result.pivots.push(self.transform(pivot_value)?);
        }
        if result.pivots.len() > 1 {
            return Err(Error::parser("UNPIVOT requires a single pivot element"));
        }
        let alias: TableAlias = self.transform_optional(list, 3)?.unwrap_or_default();
        result.column_name_alias = alias.column_name_alias;
        let mut table_ref = build::table_ref(TableRefKind::Pivot(Box::new(result)));
        table_ref.alias = alias.name;
        Ok(table_ref)
    }

    /// `TransformUnpivotValueList`: `name IN (targets)`.
    pub(super) fn transform_unpivot_value_list(&mut self, parse_result: &ParseResult) -> Result<PivotColumn> {
        let list = parse_result.list()?;
        let unpivot_names: Vec<String> = self.transform(list.child(0)?)?;
        if unpivot_names.len() != 1 {
            return Err(Error::parser(
                "UNPIVOT requires a single column name for the PIVOT IN clause",
            ));
        }
        let entries: Vec<PivotColumnEntry> = self.transform(list.child(2)?)?;
        Ok(PivotColumn {
            unpivot_names,
            entries,
            ..new_pivot_column()
        })
    }

    /// `TransformUnpivotTargetList`: `(expr [AS alias], ...)`.
    pub(super) fn transform_unpivot_target_list(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<PivotColumnEntry>> {
        let targets: Vec<ParsedExpression> =
            self.transform(parse_result.list()?.child(0)?.parens_content()?)?;
        Ok(targets
            .into_iter()
            .map(|target| PivotColumnEntry {
                values: Vec::new(),
                alias: target.alias.clone(),
                star_expr: Some(Box::new(target)),
            })
            .collect())
    }

    /// `TransformTablePivotClause`: `PIVOT (aggregates FOR ... [GROUP BY ...]) [alias]`,
    /// without its source.
    pub(super) fn transform_table_pivot_clause(&mut self, parse_result: &ParseResult) -> Result<TableRef> {
        let list = parse_result.list()?;
        let inner = list.child(1)?.parens_content()?.list()?;
        let mut result = new_pivot_ref();
        result.aggregates = self.transform_child(inner, 0)?;
        for pivot_value in inner.repeat(2)? {
            result.pivots.push(self.transform(pivot_value)?);
        }
        if let Some(groups) = self.transform_optional(inner, 3)? {
            result.groups = groups;
        }
        let alias: TableAlias = self.transform_optional(list, 2)?.unwrap_or_default();
        result.column_name_alias = alias.column_name_alias;
        let mut table_ref = build::table_ref(TableRefKind::Pivot(Box::new(result)));
        table_ref.alias = alias.name;
        Ok(table_ref)
    }

    /// `TransformPivotGroupByList`: `GROUP BY name, ...`.
    pub(super) fn transform_pivot_group_by_list(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<String>> {
        parse_result
            .list()?
            .child(2)?
            .list_elements()?
            .into_iter()
            .map(|name| self.transform(name))
            .collect()
    }

    /// `TransformPivotValueList`: `expr IN (targets)` or `expr IN enum`; a row
    /// constructor pivots on several expressions.
    pub(super) fn transform_pivot_value_list(&mut self, parse_result: &ParseResult) -> Result<PivotColumn> {
        let list = parse_result.list()?;
        let mut result = new_pivot_column();
        let pivot_expression: ParsedExpression = self.transform_child(list, 0)?;
        let target_list_or_enum = list.list(2)?.choice(0)?;
        if target_list_or_enum.result_type() == ParseResultType::Identifier {
            target_list_or_enum
                .identifier()?
                .clone_into(&mut result.pivot_enum);
        } else {
            result.entries = self.transform(target_list_or_enum)?;
        }
        match pivot_expression.kind {
            ParsedExpressionKind::Function(function) if function.function_name == "row" => {
                result.pivot_expressions = function.children;
            }
            kind => result.pivot_expressions.push(ParsedExpression {
                kind,
                ..pivot_expression
            }),
        }
        Ok(result)
    }

    /// `TransformPivotHeader`.
    pub(super) fn transform_pivot_header(&mut self, parse_result: &ParseResult) -> Result<ParsedExpression> {
        self.transform(parse_result.list()?.child(0)?)
    }

    /// `TransformPivotTargetList`: `(value [AS alias], ...)`; entries that are not
    /// constants keep their expression.
    pub(super) fn transform_pivot_target_list(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<PivotColumnEntry>> {
        let targets: Vec<ParsedExpression> =
            self.transform(parse_result.list()?.child(0)?.parens_content()?)?;
        let mut result = Vec::with_capacity(targets.len());
        for target in targets {
            let mut entry = PivotColumnEntry {
                values: Vec::new(),
                alias: target.alias.clone(),
                star_expr: None,
            };
            if !transform_pivot_in_list(&target, &mut entry)? {
                // PIVOT rejects these later.
                entry.star_expr = Some(Box::new(target));
            }
            result.push(entry);
        }
        Ok(result)
    }

    /// `TransformJoinClause`.
    pub(super) fn transform_join_clause(&mut self, parse_result: &ParseResult) -> Result<TableRef> {
        self.transform(parse_result.list()?.choice(0)?)
    }

    /// `TransformRegularJoinClause`: `[ASOF] [type] JOIN ref ON/USING ...`, without its
    /// left side.
    pub(super) fn transform_regular_join_clause(&mut self, parse_result: &ParseResult) -> Result<TableRef> {
        let list = parse_result.list()?;
        let ref_type = if list.has(0)? {
            JoinRefType::Asof
        } else {
            JoinRefType::Regular
        };
        let join_type: JoinType = self.transform_optional(list, 1)?.unwrap_or(JoinType::Inner);
        let right: TableRef = self.transform_child(list, 3)?;
        let mut result = new_join(ref_type, join_type, right);
        let qualifier: JoinQualifier = self.transform_child(list, 4)?;
        if let Some(on_clause) = qualifier.on_clause {
            result.condition = Some(Box::new(on_clause));
        } else if !qualifier.using_columns.is_empty() {
            result.using_columns = qualifier.using_columns;
        } else {
            return Err(Error::internal("Invalid join qualifier found."));
        }
        Ok(build::table_ref(TableRefKind::Join(Box::new(result))))
    }

    /// `TransformJoinType`.
    pub(super) fn transform_join_type(&mut self, parse_result: &ParseResult) -> Result<JoinType> {
        Self::transform_enum(parse_result.list()?.choice(0)?)
    }

    /// `TransformJoinQualifier`.
    pub(super) fn transform_join_qualifier(&mut self, parse_result: &ParseResult) -> Result<JoinQualifier> {
        self.transform(parse_result.list()?.choice(0)?)
    }

    /// `TransformOnClause`: `ON expr`.
    pub(super) fn transform_on_clause(&mut self, parse_result: &ParseResult) -> Result<JoinQualifier> {
        Ok(JoinQualifier {
            on_clause: Some(self.transform_child(parse_result.list()?, 1)?),
            using_columns: Vec::new(),
        })
    }

    /// `TransformUsingClause`: `USING (column, ...)`.
    pub(super) fn transform_using_clause(&mut self, parse_result: &ParseResult) -> Result<JoinQualifier> {
        let columns = parse_result.list()?.child(1)?.parens_content()?.list_elements()?;
        let mut result = JoinQualifier::default();
        for column in columns {
            let column = column.identifier()?;
            if column.is_empty() {
                return Err(Error::parser("Column identifier cannot be empty"));
            }
            result.using_columns.push(column.to_owned());
        }
        Ok(result)
    }

    /// `TransformJoinWithoutOnClause`: `CROSS / NATURAL / POSITIONAL JOIN ref`, without
    /// its left side.
    pub(super) fn transform_join_without_on_clause(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<TableRef> {
        let list = parse_result.list()?;
        let prefix: JoinPrefix = self.transform_child(list, 0)?;
        let right: TableRef = self.transform_child(list, 2)?;
        Ok(build::table_ref(TableRefKind::Join(Box::new(new_join(
            prefix.ref_type,
            prefix.join_type,
            right,
        )))))
    }

    /// `TransformJoinPrefix`.
    pub(super) fn transform_join_prefix(&mut self, parse_result: &ParseResult) -> Result<JoinPrefix> {
        self.transform(parse_result.list()?.choice(0)?)
    }

    /// `TransformCrossJoinPrefix`: `CROSS`.
    pub(super) fn transform_cross_join_prefix(&mut self, _parse_result: &ParseResult) -> Result<JoinPrefix> {
        Ok(JoinPrefix {
            ref_type: JoinRefType::Cross,
            join_type: JoinType::Inner,
        })
    }

    /// `TransformNaturalJoinPrefix`: `NATURAL [type]`.
    pub(super) fn transform_natural_join_prefix(&mut self, parse_result: &ParseResult) -> Result<JoinPrefix> {
        Ok(JoinPrefix {
            ref_type: JoinRefType::Natural,
            join_type: self
                .transform_optional(parse_result.list()?, 1)?
                .unwrap_or(JoinType::Inner),
        })
    }

    /// `TransformPositionalJoinPrefix`: `POSITIONAL`.
    pub(super) fn transform_positional_join_prefix(
        &mut self,
        _parse_result: &ParseResult,
    ) -> Result<JoinPrefix> {
        Ok(JoinPrefix {
            ref_type: JoinRefType::Positional,
            join_type: JoinType::Inner,
        })
    }

    /// `TransformInnerTableRef`.
    pub(super) fn transform_inner_table_ref(&mut self, parse_result: &ParseResult) -> Result<TableRef> {
        self.transform(parse_result.list()?.choice(0)?)
    }

    /// `TransformTableFunction`.
    pub(super) fn transform_table_function(&mut self, parse_result: &ParseResult) -> Result<TableRef> {
        self.transform(parse_result.list()?.choice(0)?)
    }

    /// `TransformTableFunctionLateralOpt`: `[LATERAL] f(args) [WITH ORDINALITY] [alias]`.
    pub(super) fn transform_table_function_lateral_opt(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<TableRef> {
        let list = parse_result.list()?;
        let function: QualifiedName = self.transform_child(list, 1)?;
        let arguments: Vec<ParsedExpression> = self.transform_child(list, 2)?;
        let mut table_ref = table_function_ref(&function, arguments, list.has(3)?);
        if let Some(alias) = self.transform_optional(list, 4)? {
            apply_table_alias(&mut table_ref, alias);
        }
        Ok(table_ref)
    }

    /// `TransformTableFunctionAliasColon`: `alias: f(args) [WITH ORDINALITY] [sample]`.
    pub(super) fn transform_table_function_alias_colon(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<TableRef> {
        let list = parse_result.list()?;
        let alias: String = self.transform_child(list, 0)?;
        let function: QualifiedName = self.transform_child(list, 1)?;
        let arguments: Vec<ParsedExpression> = self.transform_child(list, 2)?;
        let mut table_ref = table_function_ref(&function, arguments, list.has(3)?);
        table_ref.alias = alias;
        table_ref.sample = self.transform_optional(list, 4)?.map(Box::new);
        Ok(table_ref)
    }

    /// `TransformTableAliasColon`: `alias:`.
    pub(super) fn transform_table_alias_colon(&mut self, parse_result: &ParseResult) -> Result<String> {
        self.transform_child(parse_result.list()?, 0)
    }

    /// `TransformQualifiedTableFunction`: `[catalog.][schema.]function`; a single
    /// qualifier is the schema.
    pub(super) fn transform_qualified_table_function(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<QualifiedName> {
        let list = parse_result.list()?;
        let mut result = QualifiedName {
            catalog: self.transform_optional(list, 0)?.unwrap_or_default(),
            schema: self.transform_optional(list, 1)?.unwrap_or_default(),
            name: list.identifier(2)?.to_owned(),
        };
        if !result.catalog.is_empty() && result.schema.is_empty() {
            result.schema = std::mem::take(&mut result.catalog);
        }
        Ok(result)
    }

    /// `TransformTableSubquery`: `[LATERAL] (subquery) [alias]`.
    pub(super) fn transform_table_subquery(&mut self, parse_result: &ParseResult) -> Result<TableRef> {
        let list = parse_result.list()?;
        let mut subquery: TableRef = self.transform_child(list, 1)?;
        if let Some(alias) = self.transform_optional(list, 2)? {
            apply_table_alias(&mut subquery, alias);
        }
        Ok(subquery)
    }

    /// `TransformSubqueryReference`: `(select)`.
    pub(super) fn transform_subquery_reference(&mut self, parse_result: &ParseResult) -> Result<TableRef> {
        let select: SelectStatement = self.transform(parse_result.list()?.child(0)?.parens_content()?)?;
        Ok(build::subquery_ref(select))
    }

    /// `TransformBaseTableRef`: `[alias:] table [alias] [AT (...)] [sample]`.
    pub(super) fn transform_base_table_ref(&mut self, parse_result: &ParseResult) -> Result<TableRef> {
        let list = parse_result.list()?;
        let BaseTable(mut result) = self.transform_child(list, 1)?;
        let alias_colon: Option<String> = self.transform_optional(list, 0)?;
        let has_alias_colon = alias_colon.is_some();
        if let Some(alias) = alias_colon {
            result.alias = alias;
        }
        if list.has(2)? && has_alias_colon {
            return Err(Error::parser(format!(
                "Table reference {result} cannot have two aliases"
            )));
        }
        if let Some(alias) = self.transform_optional(list, 2)? {
            apply_table_alias(&mut result, alias);
        }
        if let Some(at_clause) = self.transform_optional::<AtClause>(list, 3)?
            && let TableRefKind::BaseTable(base) = &mut result.kind
        {
            base.at_clause = Some(Box::new(at_clause));
        }
        if let Some(sample) = self.transform_optional(list, 4)? {
            result.sample = Some(Box::new(sample));
        }
        Ok(result)
    }

    /// `TransformParensTableRef`: `[alias:] (table ref) [alias] [sample]`; with an alias
    /// it is `(SELECT * FROM ref) alias`.
    pub(super) fn transform_parens_table_ref(&mut self, parse_result: &ParseResult) -> Result<TableRef> {
        let list = parse_result.list()?;
        let table_ref: TableRef = self.transform(list.child(1)?.parens_content()?)?;
        let has_alias_colon = list.has(0)?;
        let has_alias = list.has(2)?;
        if has_alias_colon && has_alias {
            return Err(Error::parser(format!(
                "Table reference {table_ref} cannot have two aliases"
            )));
        }
        if !has_alias_colon && !has_alias {
            return Ok(table_ref);
        }
        let mut subquery = build::subquery_ref(select_star_from(table_ref));
        if let Some(alias) = self.transform_optional::<String>(list, 0)? {
            subquery.alias = alias;
        }
        if let Some(alias) = self.transform_optional(list, 2)? {
            apply_table_alias(&mut subquery, alias);
        }
        subquery.sample = self.transform_optional(list, 3)?.map(Box::new);
        Ok(subquery)
    }

    /// `TransformAtClause`: `AT (unit => expr)`.
    pub(super) fn transform_at_clause(&mut self, parse_result: &ParseResult) -> Result<AtClause> {
        self.transform(parse_result.list()?.child(1)?.parens_content()?)
    }

    /// `TransformAtSpecifier`: `unit => expr`.
    pub(super) fn transform_at_specifier(&mut self, parse_result: &ParseResult) -> Result<AtClause> {
        let list = parse_result.list()?;
        let unit: String = self.transform_child(list, 0)?;
        let expr: ParsedExpression = self.transform_child(list, 2)?;
        Ok(AtClause {
            unit,
            expr: Box::new(expr),
        })
    }

    /// `TransformAtUnit`: `TIMESTAMP` / `VERSION`.
    pub(super) fn transform_at_unit(&mut self, parse_result: &ParseResult) -> Result<String> {
        Ok(parse_result.list()?.choice(0)?.keyword()?.to_owned())
    }

    /// `TransformValuesRef`: `VALUES ... [alias]` as a table.
    pub(super) fn transform_values_ref(&mut self, parse_result: &ParseResult) -> Result<TableRef> {
        let list = parse_result.list()?;
        let values: SelectStatement = self.transform_child(list, 0)?;
        let mut subquery = build::subquery_ref(values);
        if let Some(alias) = self.transform_optional(list, 1)? {
            apply_table_alias(&mut subquery, alias);
        }
        Ok(subquery)
    }

    /// `TransformValuesClause`: `VALUES (...), ...` is `SELECT * FROM valueslist`.
    pub(super) fn transform_values_clause(&mut self, parse_result: &ParseResult) -> Result<SelectStatement> {
        let rows = parse_result.list()?.child(1)?.list_elements()?;
        let values = rows
            .into_iter()
            .map(|row| self.transform(row))
            .collect::<Result<Vec<Vec<ParsedExpression>>>>()?;
        if let Some(first) = values.first()
            && values.iter().any(|row| row.len() != first.len())
        {
            return Err(Error::parser("VALUES lists must all be the same length"));
        }
        let mut values_ref = build::table_ref(TableRefKind::ExpressionList(Box::new(ExpressionListRef {
            expected_names: Vec::new(),
            expected_types: Vec::new(),
            values,
        })));
        "valueslist".clone_into(&mut values_ref.alias);
        Ok(select_star_from(values_ref))
    }

    /// `TransformValuesExpressions`: `(expr, ...)`.
    pub(super) fn transform_values_expressions(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<ParsedExpression>> {
        self.transform_expressions(parse_result.list()?.child(0)?.parens_content()?)
    }

    /// `TransformTableStatement`: `TABLE name` is `SELECT * FROM name`.
    pub(super) fn transform_table_statement(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<SelectStatement> {
        let BaseTable(table) = self.transform_child(parse_result.list()?, 1)?;
        Ok(select_star_from(table))
    }

    /// `TransformOrderByClause`: `ORDER BY ...`.
    pub(super) fn transform_order_by_clause(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<OrderByNode>> {
        self.transform_child(parse_result.list()?, 2)
    }

    /// `TransformOrderByExpressions`.
    pub(super) fn transform_order_by_expressions(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<OrderByNode>> {
        self.transform(parse_result.list()?.choice(0)?)
    }

    /// `TransformOrderByExpressionList`: `expr [ASC|DESC] [NULLS ...], ...`.
    pub(super) fn transform_order_by_expression_list(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<OrderByNode>> {
        parse_result
            .list()?
            .child(0)?
            .list_elements()?
            .into_iter()
            .map(|order| self.transform(order))
            .collect()
    }

    /// `TransformOrderByAll`: `ORDER BY ALL [ASC|DESC] [NULLS ...]`.
    pub(super) fn transform_order_by_all(&mut self, parse_result: &ParseResult) -> Result<Vec<OrderByNode>> {
        let list = parse_result.list()?;
        let order_type: OrderType = self
            .transform_optional(list, 1)?
            .unwrap_or(OrderType::OrderDefault);
        let null_order: OrderByNullType = self
            .transform_optional(list, 2)?
            .unwrap_or(OrderByNullType::OrderDefault);
        let mut star = build::star_expression(String::new());
        star.columns = true;
        Ok(vec![OrderByNode {
            order_type,
            null_order,
            expression: Box::new(ParsedExpression::new(
                ExpressionType::Star,
                ParsedExpressionKind::Star(Box::new(star)),
            )),
        }])
    }

    /// `TransformOrderByExpression`: `expr [ASC|DESC] [NULLS FIRST|LAST]`.
    pub(super) fn transform_order_by_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<OrderByNode> {
        let list = parse_result.list()?;
        let expression: ParsedExpression = self.transform_child(list, 0)?;
        let order_type: OrderType = self
            .transform_optional(list, 1)?
            .unwrap_or(OrderType::OrderDefault);
        let null_order: OrderByNullType = self
            .transform_optional(list, 2)?
            .unwrap_or(OrderByNullType::OrderDefault);
        Ok(OrderByNode {
            order_type,
            null_order,
            expression: Box::new(expression),
        })
    }

    /// `TransformDescOrAsc`.
    pub(super) fn transform_desc_or_asc(&mut self, parse_result: &ParseResult) -> Result<OrderType> {
        Self::transform_enum(parse_result.list()?.choice(0)?)
    }

    /// `TransformNullsFirstOrLast`.
    pub(super) fn transform_nulls_first_or_last(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<OrderByNullType> {
        Self::transform_enum(parse_result.list()?.choice(0)?)
    }

    /// `TransformResultModifiers`: `[ORDER BY ...] [LIMIT / OFFSET ...]`.
    pub(super) fn transform_result_modifiers(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<ResultModifier>> {
        let list = parse_result.list()?;
        let mut result = Vec::new();
        let orders: Vec<OrderByNode> = self.transform_optional(list, 0)?.unwrap_or_default();
        if !orders.is_empty() {
            result.push(ResultModifier::Order(OrderModifier { orders }));
        }
        if let Some(Some(limit_offset)) = self.transform_optional::<Option<ResultModifier>>(list, 1)? {
            result.push(limit_offset);
        }
        Ok(result)
    }

    /// `TransformLimitOffset`.
    pub(super) fn transform_limit_offset(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Option<ResultModifier>> {
        self.transform(parse_result.list()?.choice(0)?)
    }

    /// `TransformOffsetLimitClause`: `OFFSET ... [LIMIT ...]`.
    pub(super) fn transform_offset_limit_clause(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Option<ResultModifier>> {
        let list = parse_result.list()?;
        let offset: LimitPercentResult = self.transform_child(list, 0)?;
        let limit: LimitPercentResult = self.transform_optional(list, 1)?.unwrap_or_default();
        verify_limit_offset(limit, offset)
    }

    /// `TransformLimitOffsetClause`: `LIMIT ... [OFFSET ...]`.
    pub(super) fn transform_limit_offset_clause(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Option<ResultModifier>> {
        let list = parse_result.list()?;
        let limit: LimitPercentResult = self.transform_child(list, 0)?;
        let offset: LimitPercentResult = self.transform_optional(list, 1)?.unwrap_or_default();
        verify_limit_offset(limit, offset)
    }

    /// `TransformLimitClause`: `LIMIT value`.
    pub(super) fn transform_limit_clause(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<LimitPercentResult> {
        self.transform_child(parse_result.list()?, 1)
    }

    /// `TransformLimitValue`.
    pub(super) fn transform_limit_value(&mut self, parse_result: &ParseResult) -> Result<LimitPercentResult> {
        self.transform(parse_result.list()?.choice(0)?)
    }

    /// `TransformLimitAll`: `LIMIT ALL` (a `*` limit).
    pub(super) fn transform_limit_all(&mut self, _parse_result: &ParseResult) -> Result<LimitPercentResult> {
        Ok(LimitPercentResult {
            is_percent: false,
            expression: Some(build::star()),
        })
    }

    /// `TransformLimitLiteralPercent`: `LIMIT n%` with a literal (as a string constant,
    /// like DuckDB).
    pub(super) fn transform_limit_literal_percent(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<LimitPercentResult> {
        let number = parse_result.list()?.child(0)?.number()?;
        Ok(LimitPercentResult {
            is_percent: true,
            expression: Some(build::varchar_constant(number)),
        })
    }

    /// `TransformLimitExpression`: `LIMIT expr [%]`.
    pub(super) fn transform_limit_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<LimitPercentResult> {
        let list = parse_result.list()?;
        Ok(LimitPercentResult {
            expression: Some(self.transform_child(list, 0)?),
            is_percent: list.has(1)?,
        })
    }

    /// `TransformOffsetClause`: `OFFSET value`.
    pub(super) fn transform_offset_clause(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<LimitPercentResult> {
        self.transform_child(parse_result.list()?, 1)
    }

    /// `TransformGroupByClause`: `GROUP BY ...`.
    pub(super) fn transform_group_by_clause(&mut self, parse_result: &ParseResult) -> Result<GroupByNode> {
        self.transform_child(parse_result.list()?, 2)
    }

    /// `TransformGroupByExpressions`.
    pub(super) fn transform_group_by_expressions(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<GroupByNode> {
        self.transform(parse_result.list()?.choice(0)?)
    }

    /// `TransformGroupByAll`: `GROUP BY ALL` (a `*` group).
    pub(super) fn transform_group_by_all(&mut self, _parse_result: &ParseResult) -> Result<GroupByNode> {
        Ok(GroupByNode {
            group_expressions: vec![build::star()],
            grouping_sets: Vec::new(),
        })
    }

    /// `GroupByExpressionUnfolding`: the grouping sets of one GROUP BY item.
    fn group_by_expression_unfolding(
        &mut self,
        group_by_expr: &ParseResult,
        map: &mut GroupingExpressionMap,
        result: &mut GroupByNode,
    ) -> Result<Vec<GroupingSet>> {
        let mut result_sets = Vec::new();
        let name = group_by_expr.name;
        if name.eq_ignore_ascii_case("EmptyGroupingItem") {
            result_sets.push(GroupingSet::new());
        } else if name.eq_ignore_ascii_case("Expression") {
            let mut indexes = Vec::new();
            let expression: ParsedExpression = self.transform(group_by_expr)?;
            add_group_by_expression(expression, map, result, &mut indexes);
            result_sets.push(indexes.into_iter().collect());
        } else if name.eq_ignore_ascii_case("GroupingSetsClause") {
            let inner = group_by_expr.list()?.child(2)?.parens_content()?.list()?;
            for child in inner.child(0)?.list_elements()? {
                let child_expr = child.list()?.choice(0)?;
                result_sets.extend(self.group_by_expression_unfolding(child_expr, map, result)?);
            }
        } else if name.eq_ignore_ascii_case("CubeOrRollupClause") {
            let list = group_by_expr.list()?;
            let type_str: String = self.transform_child(list, 0)?;
            let Some(expressions) = list.child(1)?.parens_content()?.optional()? else {
                return Err(Error::parser("CUBE or ROLLUP column list cannot be empty"));
            };
            let mut unfolding_sets = Vec::new();
            for expression in expressions.list_elements()? {
                let mut indexes = Vec::new();
                let expression: ParsedExpression = self.transform(expression)?;
                add_group_by_expression(expression, map, result, &mut indexes);
                unfolding_sets.push(indexes.into_iter().collect::<GroupingSet>());
            }
            if type_str.eq_ignore_ascii_case("CUBE") {
                check_grouping_set_cubes(result_sets.len(), unfolding_sets.len())?;
                add_cube_sets(&GroupingSet::new(), &unfolding_sets, &mut result_sets, 0)?;
            } else if type_str.eq_ignore_ascii_case("ROLLUP") {
                let mut current_set = GroupingSet::new();
                result_sets.push(current_set.clone());
                for set in &unfolding_sets {
                    current_set.extend(set.iter().copied());
                    result_sets.push(current_set.clone());
                }
            }
        }
        Ok(result_sets)
    }

    /// `TransformCubeOrRollup`: `CUBE` / `ROLLUP`.
    pub(super) fn transform_cube_or_rollup(&mut self, parse_result: &ParseResult) -> Result<String> {
        Ok(parse_result.list()?.choice(0)?.keyword()?.to_owned())
    }

    /// `TransformGroupByList`: the GROUP BY items; their grouping sets multiply.
    pub(super) fn transform_group_by_list(&mut self, parse_result: &ParseResult) -> Result<GroupByNode> {
        let items = parse_result.list()?.child(0)?.list_elements()?;
        let mut result = GroupByNode::default();
        let mut map = GroupingExpressionMap::default();
        for item in items {
            let group_by_expr = item.list()?.choice(0)?;
            let next_sets = self.group_by_expression_unfolding(group_by_expr, &mut map, &mut result)?;
            if result.grouping_sets.is_empty() {
                result.grouping_sets = next_sets;
                continue;
            }
            let grouping_set_count = result.grouping_sets.len().saturating_mul(next_sets.len());
            check_grouping_set_max(grouping_set_count)?;
            let mut new_sets = Vec::with_capacity(grouping_set_count);
            for current_set in &result.grouping_sets {
                for next_set in &next_sets {
                    new_sets.push(current_set.union(next_set).copied().collect());
                }
            }
            result.grouping_sets = new_sets;
        }
        Ok(result)
    }

    /// `TransformGroupByExpression`.
    pub(super) fn transform_group_by_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        self.transform(parse_result.list()?.choice(0)?)
    }

    /// `TransformEmptyGroupingItem`: only unfolded by `TransformGroupByList`.
    pub(super) fn transform_empty_grouping_item(
        &mut self,
        _parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(Error::not_implemented(
            "Rule 'EmptyGroupingItem' has not been implemented yet",
        ))
    }

    /// `TransformCubeOrRollupClause`: only unfolded by `TransformGroupByList`.
    pub(super) fn transform_cube_or_rollup_clause(
        &mut self,
        _parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(Error::not_implemented(
            "Rule 'CubeOrRollupClause' has not been implemented yet",
        ))
    }

    /// `TransformGroupingSetsClause`: only unfolded by `TransformGroupByList`.
    pub(super) fn transform_grouping_sets_clause(
        &mut self,
        _parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(Error::not_implemented(
            "Rule 'GroupingSetsClause' has not been implemented yet",
        ))
    }

    /// `TransformWithClause`: `WITH [RECURSIVE] name AS (...), ...`.
    pub(super) fn transform_with_clause(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<CommonTableExpressionMap> {
        let list = parse_result.list()?;
        let is_recursive = list.has(1)?;
        let mut result = CommonTableExpressionMap::default();
        for entry in list.child(2)?.list_elements()? {
            let (name, mut info): (String, CommonTableExpressionInfo) = self.transform(entry)?;
            if is_recursive {
                for modifier in &info.query.node.modifiers {
                    match modifier {
                        ResultModifier::Limit(_) | ResultModifier::LimitPercent(_) => {
                            return Err(Error::parser(
                                "LIMIT or OFFSET in a recursive query is not allowed",
                            ));
                        }
                        ResultModifier::Order(_) => {
                            return Err(Error::parser("ORDER BY in a recursive query is not allowed"));
                        }
                        ResultModifier::Distinct(_) => {}
                    }
                }
                let node = std::mem::replace(&mut info.query.node, Box::new(new_select_node()));
                info.query.node = Box::new(to_recursive_cte(*node, &name, &info.aliases, &info.key_targets)?);
            }
            if result.map.contains_key(&name) {
                return Err(Error::parser(format!("Duplicate CTE name \"{name}\"")));
            }
            result.map.insert(&name, info);
        }
        Ok(result)
    }

    /// `TransformWithStatement`: `name [(aliases)] [USING KEY (...)] AS [[NOT] MATERIALIZED] (query)`.
    pub(super) fn transform_with_statement(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<(String, CommonTableExpressionInfo)> {
        let list = parse_result.list()?;
        let name: String = self.transform_child(list, 0)?;
        let aliases: Vec<String> = self.transform_optional(list, 1)?.unwrap_or_default();
        let key_targets: Vec<ParsedExpression> = self.transform_optional(list, 2)?.unwrap_or_default();
        let materialized = match self.transform_optional::<bool>(list, 4)? {
            // With a result, it is NEVER or ALWAYS.
            Some(true) => CteMaterialize::CteMaterializeNever,
            Some(false) => CteMaterialize::CteMaterializeAlways,
            None => CteMaterialize::CteMaterializeDefault,
        };
        let table_ref: TableRef = self.transform_child(list, 5)?;
        let TableRefKind::Subquery(subquery) = table_ref.kind else {
            return Err(Error::internal("A CTE body is not a subquery"));
        };
        Ok((
            name,
            CommonTableExpressionInfo {
                aliases,
                query: subquery.subquery,
                materialized,
                key_targets,
            },
        ))
    }

    /// `TransformCTEBody`: `(SELECT ...)`; other statements are not allowed.
    pub(super) fn transform_cte_body(&mut self, parse_result: &ParseResult) -> Result<TableRef> {
        let body = parse_result
            .list()?
            .child(0)?
            .parens_content()?
            .list()?
            .choice(0)?;
        if body.name != "SelectStatementInternal" {
            return Err(Error::parser("A CTE needs a SELECT"));
        }
        let select: SelectStatement = self.transform(body)?;
        Ok(build::subquery_ref(select))
    }

    /// `TransformMaterialized`: whether `NOT MATERIALIZED`.
    pub(super) fn transform_materialized(&mut self, parse_result: &ParseResult) -> Result<bool> {
        parse_result.list()?.has(0)
    }

    /// `TransformUsingKey`: `USING KEY (columns)`.
    pub(super) fn transform_using_key(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<ParsedExpression>> {
        let columns: Vec<String> = self.transform_child(parse_result.list()?, 2)?;
        Ok(columns
            .into_iter()
            .map(|column| build::column_ref(vec![column]))
            .collect())
    }

    /// `TransformHavingClause`: `HAVING expr`.
    pub(super) fn transform_having_clause(&mut self, parse_result: &ParseResult) -> Result<ParsedExpression> {
        self.transform_child(parse_result.list()?, 1)
    }

    /// `TransformOffsetValue`.
    pub(super) fn transform_offset_value(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<LimitPercentResult> {
        Ok(LimitPercentResult {
            is_percent: false,
            expression: Some(self.transform_child(parse_result.list()?, 0)?),
        })
    }

    /// `TransformQualifyClause`: `QUALIFY expr`.
    pub(super) fn transform_qualify_clause(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        self.transform_child(parse_result.list()?, 1)
    }

    /// `TransformWindowClause`: `WINDOW name AS (...), ...`, the windows aliased by name.
    pub(super) fn transform_window_clause(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<ParsedExpression>> {
        self.transform_expressions(parse_result.list()?.child(1)?)
    }

    /// `TransformWindowDefinition`: `name AS (window)`.
    pub(super) fn transform_window_definition(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        let list = parse_result.list()?;
        self.in_window_definition = true;
        let WindowExpr(window) = self.transform_child(list, 2)?;
        self.in_window_definition = false;
        Ok(window.with_alias(list.identifier(0)?))
    }

    /// `TransformSampleClause`: `USING SAMPLE ...`.
    pub(super) fn transform_sample_clause(&mut self, parse_result: &ParseResult) -> Result<SampleOptions> {
        self.transform_child(parse_result.list()?, 1)
    }

    /// `TransformSampleEntry`.
    pub(super) fn transform_sample_entry(&mut self, parse_result: &ParseResult) -> Result<SampleOptions> {
        self.transform(parse_result.list()?.choice(0)?)
    }

    /// `TransformSampleEntryFunction`: `[method] (count) [REPEATABLE (seed)]`.
    pub(super) fn transform_sample_entry_function(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<SampleOptions> {
        let list = parse_result.list()?;
        let mut result: SampleOptions = self.transform(list.child(1)?.parens_content()?)?;
        if let Some(method) = self.transform_optional(list, 0)? {
            result.method = method;
        }
        if let Some(seed) = self.transform_optional::<Option<u64>>(list, 2)? {
            result.seed = seed;
            result.repeatable = true;
        }
        Ok(result)
    }

    /// `TransformSampleEntryCount`: `count [(method [, seed])]`.
    pub(super) fn transform_sample_entry_count(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<SampleOptions> {
        let list = parse_result.list()?;
        let mut result: SampleOptions = self.transform_child(list, 0)?;
        if let Some(properties) = list.optional(1)? {
            let (method, seed): (SampleMethod, Option<u64>) = self.transform(properties.parens_content()?)?;
            result.method = method;
            result.seed = seed;
        }
        Ok(result)
    }

    /// `TransformSampleFunction`: the sampling method.
    pub(super) fn transform_sample_function(&mut self, parse_result: &ParseResult) -> Result<SampleMethod> {
        let method: String = self.transform_child(parse_result.list()?, 0)?;
        sample_method_from_string(&method)
    }

    /// `TransformSampleCount`: `n [ROWS | %]`: a reservoir sample of rows or a system
    /// sample of a percentage.
    pub(super) fn transform_sample_count(&mut self, parse_result: &ParseResult) -> Result<SampleOptions> {
        let list = parse_result.list()?;
        let expression: ParsedExpression = self.transform_child(list, 0)?;
        let ParsedExpressionKind::Constant(constant) = &expression.kind else {
            return Err(
                Error::parser("Only constants are supported in sample clause currently")
                    .with_position(expression.query_location),
            );
        };
        let value = &constant.value;
        let is_percentage: bool = self.transform_optional(list, 1)?.unwrap_or(false);
        let mut result = SampleOptions {
            sample_size: Value::default(),
            is_percentage,
            method: SampleMethod::Invalid,
            seed: None,
            repeatable: false,
        };
        if is_percentage {
            let percentage = rustdb_parser::get_value_f64(value)?;
            if !(0.0..=100.0).contains(&percentage) {
                return Err(Error::parser(format!(
                    "Sample sample_size {percentage:.6} out of range, must be between 0 and 100"
                )));
            }
            result.sample_size = Value::double(percentage);
            result.method = SampleMethod::SystemSample;
        } else {
            let rows = rustdb_parser::get_value_i64(value)?;
            if rows < 0 || rows.unsigned_abs() > MAX_SAMPLE_ROWS {
                return Err(Error::parser(format!(
                    "Sample rows {rows} out of range, must be between 0 and {MAX_SAMPLE_ROWS}"
                )));
            }
            result.sample_size = Value::bigint(rows);
            result.method = SampleMethod::ReservoirSample;
        }
        Ok(result)
    }

    /// `TransformSampleValue`.
    pub(super) fn transform_sample_value(&mut self, parse_result: &ParseResult) -> Result<ParsedExpression> {
        self.transform(parse_result.list()?.choice(0)?)
    }

    /// `TransformSampleUnit`: whether a percentage (rather than rows).
    pub(super) fn transform_sample_unit(&mut self, parse_result: &ParseResult) -> Result<bool> {
        Self::transform_enum(parse_result.list()?.choice(0)?)
    }

    /// `TransformSampleProperties`: `method [, seed]`.
    pub(super) fn transform_sample_properties(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<(SampleMethod, Option<u64>)> {
        let list = parse_result.list()?;
        let method: String = self.transform_child(list, 0)?;
        let method = sample_method_from_string(&method)?;
        let seed = match list.optional(1)? {
            Some(seed) => self.transform_child(seed.list()?, 1)?,
            None => None,
        };
        Ok((method, seed))
    }

    /// `TransformRepeatableSample`: `REPEATABLE (seed)`.
    pub(super) fn transform_repeatable_sample(&mut self, parse_result: &ParseResult) -> Result<Option<u64>> {
        self.transform(parse_result.list()?.child(1)?.parens_content()?)
    }

    /// `TransformSampleSeed`: the seed constant.
    pub(super) fn transform_sample_seed(&mut self, parse_result: &ParseResult) -> Result<Option<u64>> {
        let expression: ParsedExpression = self.transform(parse_result.list()?.child(0)?)?;
        let ParsedExpressionKind::Constant(constant) = &expression.kind else {
            return Err(Error::internal("Expected a constant sample seed"));
        };
        let seed = constant.value.default_cast_as(&LogicalType::UBIGINT)?;
        Ok(seed.as_u64())
    }
}

/// The set operation of a query node DuckDB knows to be one.
fn set_operation_mut(node: &mut QueryNode) -> Result<&mut SetOperationNode> {
    match &mut node.kind {
        QueryNodeKind::SetOperation(setop) => Ok(setop),
        _ => Err(Error::internal("Expected a set operation node")),
    }
}

/// A `SetOperationNode` without operands.
fn set_operation_node(setop_type: SetOperationType, setop_all: bool) -> QueryNode {
    build::query_node(QueryNodeKind::SetOperation(SetOperationNode {
        setop_type,
        setop_all,
        children: Vec::new(),
    }))
}

/// `make_uniq<PivotRef>()`.
fn new_pivot_ref() -> PivotRef {
    PivotRef {
        source: Box::new(build::empty_table_ref()),
        aggregates: Vec::new(),
        unpivot_names: Vec::new(),
        pivots: Vec::new(),
        groups: Vec::new(),
        column_name_alias: Vec::new(),
        include_nulls: false,
        bound: None,
    }
}

/// `PivotColumn()`.
fn new_pivot_column() -> PivotColumn {
    PivotColumn {
        pivot_expressions: Vec::new(),
        unpivot_names: Vec::new(),
        entries: Vec::new(),
        pivot_enum: String::new(),
        subquery: None,
    }
}

/// A `TableFunctionRef` calling `function` with `arguments`.
fn table_function_ref(
    function: &QualifiedName,
    arguments: Vec<ParsedExpression>,
    with_ordinality: bool,
) -> TableRef {
    let call = build::function_full(
        function.catalog.clone(),
        function.schema.clone(),
        &function.name,
        arguments,
        None,
        OrderModifier { orders: Vec::new() },
        false,
        false,
        false,
    );
    build::table_ref(TableRefKind::TableFunction(TableFunctionRef {
        function: Box::new(call),
        column_name_alias: Vec::new(),
        with_ordinality: if with_ordinality {
            OrdinalityType::WithOrdinality
        } else {
            OrdinalityType::WithoutOrdinality
        },
    }))
}
