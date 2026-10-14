//! Node constructors and checks used by the semantic actions: the Rust form of
//! libpg_query's `makefuncs.cpp` and of the helper functions at the end of
//! `gram.y` (`makeColumnRef`, `doNegate`, `insertSelectOptions`, ...).

use rustdb_common::{Error, Result};

use crate::error::{internal, unimplemented};
use crate::nodes::{
    AConst, AExpr, AExprKind, AIndirection, Alias, BoolExpr, BoolExprType, CollateClause, ColumnRef,
    CommonTableExpr, DefElem, DefElemAction, FuncCall, GroupingSet, GroupingSetKind, IntervalConstant,
    LimitPercent, List, Node, NodeStruct, ParamRef, RangeVar, RawStmt, RelPersistence, ResTarget,
    SampleOptions, SampleSize, SelectStmt, SetOperation, TypeCast, TypeName, Value, WithClause,
};
use crate::scanner::Scanner;
use crate::semantic::SelectLimit;

/// The schema `SystemFuncName`/`SystemTypeName` qualify names with.
const DEFAULT_SCHEMA: &str = "main";

/// Bits produced by `ConstraintAttributeSpec` (`CAS_*`).
pub(crate) mod constraint_attr {
    pub(crate) const NOT_DEFERRABLE: i32 = 0x01;
    pub(crate) const DEFERRABLE: i32 = 0x02;
    pub(crate) const INITIALLY_IMMEDIATE: i32 = 0x04;
    pub(crate) const INITIALLY_DEFERRED: i32 = 0x08;
    pub(crate) const NOT_VALID: i32 = 0x10;
    pub(crate) const NO_INHERIT: i32 = 0x20;
}

// ---------------------------------------------------------------------------------------
// List and pointer plumbing
// ---------------------------------------------------------------------------------------

/// `lappend`.
pub(crate) fn append(mut list: List, node: Node) -> List {
    list.push(node);
    list
}

/// `lcons`.
pub(crate) fn prepend(node: Node, mut list: List) -> List {
    list.insert(0, node);
    list
}

/// `list_concat`.
pub(crate) fn concat(mut list: List, tail: List) -> List {
    list.extend(tail);
    list
}

/// Dereferences a pointer the grammar guarantees to be set.
pub(crate) fn required<T>(value: Option<T>) -> Result<T> {
    value.ok_or_else(null_error)
}

/// Dereferences a pointer field for modification.
pub(crate) fn required_mut<T>(value: &mut Option<Box<T>>) -> Result<&mut T> {
    value.as_deref_mut().ok_or_else(null_error)
}

/// The error for dereferencing an unexpectedly absent pointer.
pub(crate) fn null_error() -> Error {
    internal("unexpected NULL pointer in parse tree")
}

/// `(PGFoo *) node` for a node that must be present.
pub(crate) fn downcast<T: NodeStruct>(node: Node) -> Result<Box<T>> {
    T::from_node(node).map_err(|node| internal(format!("expected {}, found {}", T::NAME, node.name())))
}

/// `(PGFoo *) node` for a nullable node.
pub(crate) fn downcast_opt<T: NodeStruct>(node: Option<Node>) -> Result<Option<Box<T>>> {
    node.map(downcast).transpose()
}

/// `(PGList *) node`.
pub(crate) fn into_list(node: Option<Node>) -> Result<List> {
    Node::into_list(node).map_err(|node| internal(format!("expected a list, found {}", node.name())))
}

/// `strVal(node)`.
pub(crate) fn str_val(node: &Node) -> Result<String> {
    node.str_val()
        .map(str::to_owned)
        .ok_or_else(|| internal(format!("expected a string, found {}", node.name())))
}

/// `printf("%s", s)` of a possibly-NULL C string.
pub(crate) fn c_str(s: &Option<String>) -> &str {
    s.as_deref().unwrap_or("(null)")
}

// ---------------------------------------------------------------------------------------
// makefuncs.cpp
// ---------------------------------------------------------------------------------------

/// `makeAExpr`.
pub(crate) fn make_a_expr(
    kind: AExprKind,
    name: List,
    lexpr: Option<Node>,
    rexpr: Option<Node>,
    location: i32,
) -> AExpr {
    AExpr {
        kind,
        name,
        lexpr,
        rexpr,
        location,
    }
}

/// `makeSimpleAExpr`: an operator expression with an unqualified operator name.
pub(crate) fn make_simple_a_expr(
    kind: AExprKind,
    name: impl Into<String>,
    lexpr: Option<Node>,
    rexpr: Option<Node>,
    location: i32,
) -> AExpr {
    make_a_expr(kind, vec![Node::string(name)], lexpr, rexpr, location)
}

/// `makeBoolExpr`.
pub(crate) fn make_bool_expr(boolop: BoolExprType, args: List, location: i32) -> BoolExpr {
    BoolExpr {
        boolop,
        args,
        location,
    }
}

/// `makeAlias`.
pub(crate) fn make_alias(aliasname: impl Into<String>, colnames: List) -> Alias {
    Alias {
        aliasname: Some(aliasname.into()),
        colnames,
    }
}

/// `makeRangeVar`.
pub(crate) fn make_range_var(schemaname: Option<String>, relname: Option<String>, location: i32) -> RangeVar {
    RangeVar {
        schemaname,
        relname,
        inh: true,
        relpersistence: RelPersistence::Permanent,
        location,
        ..RangeVar::default()
    }
}

/// `makeTypeName`.
pub(crate) fn make_type_name(name: impl Into<String>) -> TypeName {
    make_type_name_from_name_list(vec![Node::string(name)])
}

/// `makeTypeNameFromNameList`.
pub(crate) fn make_type_name_from_name_list(names: List) -> TypeName {
    TypeName {
        names,
        location: -1,
        ..TypeName::default()
    }
}

/// `makeDefElem`.
pub(crate) fn make_def_elem(name: impl Into<String>, arg: Option<Node>, location: i32) -> DefElem {
    DefElem {
        defnamespace: None,
        defname: Some(name.into()),
        arg,
        defaction: DefElemAction::Unspec,
        location,
    }
}

/// `makeDefElemExtended`.
pub(crate) fn make_def_elem_extended(
    namespace: Option<String>,
    name: impl Into<String>,
    arg: Option<Node>,
    defaction: DefElemAction,
    location: i32,
) -> DefElem {
    DefElem {
        defnamespace: namespace,
        defname: Some(name.into()),
        arg,
        defaction,
        location,
    }
}

/// `makeFuncCall`.
pub(crate) fn make_func_call(funcname: List, args: List, location: i32) -> FuncCall {
    FuncCall {
        funcname,
        args,
        location,
        ..FuncCall::default()
    }
}

/// `makeGroupingSet`.
pub(crate) fn make_grouping_set(kind: GroupingSetKind, content: List, location: i32) -> GroupingSet {
    GroupingSet {
        kind,
        content,
        location,
    }
}

// ---------------------------------------------------------------------------------------
// gram.y helpers
// ---------------------------------------------------------------------------------------

/// `makeRawStmt`.
pub(crate) fn make_raw_stmt(stmt: Option<Node>, stmt_location: i32) -> RawStmt {
    RawStmt {
        stmt,
        stmt_location,
        stmt_len: 0,
    }
}

/// `updateRawStmtEnd`: records where a statement ends unless that is already known.
pub(crate) fn update_raw_stmt_end(stmt: &mut RawStmt, end_location: i32) {
    if stmt.stmt_len > 0 {
        return;
    }
    stmt.stmt_len = end_location - stmt.stmt_location;
}

/// `makeColumnRef`: a column reference, wrapped in an `AIndirection` if the
/// indirection contains subscripts.
pub(crate) fn make_column_ref(
    colname: impl Into<String>,
    indirection: List,
    location: i32,
    scanner: &Scanner<'_>,
) -> Result<Node> {
    let colname = Node::string(colname);
    let mut nfields = 0;
    for (i, item) in indirection.iter().enumerate() {
        match item {
            Node::AIndices(_) => {
                let mut indirection = indirection;
                let (fields, rest) = if nfields == 0 {
                    (vec![colname], indirection)
                } else {
                    let rest = indirection.split_off(nfields);
                    (prepend(colname, indirection), rest)
                };
                let column = ColumnRef { fields, location };
                let rest = check_indirection(rest, scanner)?;
                return Ok(AIndirection {
                    arg: Some(column.into()),
                    indirection: rest,
                }
                .into());
            }
            Node::AStar(_) if i + 1 < indirection.len() => {
                return Err(scanner.syntax_error("improper use of \"*\""));
            }
            _ => {}
        }
        nfields += 1;
    }
    Ok(ColumnRef {
        fields: prepend(colname, indirection),
        location,
    }
    .into())
}

/// `makeTypeCast`.
pub(crate) fn make_type_cast(
    arg: Option<Node>,
    type_name: Option<Box<TypeName>>,
    try_cast: i32,
    location: i32,
) -> Node {
    TypeCast {
        arg,
        type_name,
        try_cast,
        location,
    }
    .into()
}

/// `makeStringConst`.
pub(crate) fn make_string_const(s: impl Into<String>, location: i32) -> Node {
    AConst {
        val: Value::String(s.into()),
        location,
    }
    .into()
}

/// `makeStringConstCast`.
pub(crate) fn make_string_const_cast(
    s: impl Into<String>,
    location: i32,
    type_name: Option<Box<TypeName>>,
) -> Node {
    make_type_cast(Some(make_string_const(s, location)), type_name, 0, -1)
}

/// The value of an `INTERVAL` literal (`makeIntervalNode` takes a string, an
/// integer or an expression).
pub(crate) enum IntervalValue {
    /// `INTERVAL '1 day'`
    String(String),
    /// `INTERVAL 1 DAY`
    Integer(i32),
    /// `INTERVAL (expr) DAY`
    Expr(Option<Node>),
}

/// `makeIntervalNode`.
pub(crate) fn make_interval_node(value: IntervalValue, location: i32, typmods: List) -> Node {
    let mut node = IntervalConstant {
        location,
        typmods,
        ..IntervalConstant::default()
    };
    match value {
        IntervalValue::String(s) => {
            node.val_type = NODE_TAG_STRING;
            node.sval = Some(s);
        }
        IntervalValue::Integer(i) => {
            node.val_type = NODE_TAG_INTEGER;
            node.ival = i;
        }
        IntervalValue::Expr(e) => {
            node.val_type = NODE_TAG_A_EXPR;
            node.eval = e;
        }
    }
    node.into()
}

/// `T_PGInteger`, `T_PGString` and `T_PGAExpr`: the node tags `IntervalConstant`
/// stores in `val_type`.
pub(crate) const NODE_TAG_INTEGER: i32 = 218;
/// `T_PGString`.
pub(crate) const NODE_TAG_STRING: i32 = 220;
/// `T_PGAExpr`.
pub(crate) const NODE_TAG_A_EXPR: i32 = 355;

/// `makeSampleSize`.
pub(crate) fn make_sample_size(sample_size: Option<Node>, is_percentage: bool) -> Node {
    SampleSize {
        is_percentage,
        sample_size,
    }
    .into()
}

/// `makeSampleOptions`.
pub(crate) fn make_sample_options(
    sample_size: Option<Node>,
    method: Option<String>,
    seed: Option<i32>,
    location: i32,
) -> Node {
    SampleOptions {
        sample_size,
        method,
        has_seed: seed.is_some(),
        seed: seed.unwrap_or(0),
        location,
    }
    .into()
}

/// `makeLimitPercent`.
pub(crate) fn make_limit_percent(limit_percent: Option<Node>) -> Node {
    LimitPercent { limit_percent }.into()
}

/// `makeIntConst`.
pub(crate) fn make_int_const(val: i32, location: i32) -> Node {
    AConst {
        val: Value::Integer(i64::from(val)),
        location,
    }
    .into()
}

/// `makeFloatConst`.
pub(crate) fn make_float_const(s: impl Into<String>, location: i32) -> Node {
    AConst {
        val: Value::Float(s.into()),
        location,
    }
    .into()
}

/// `makeBitStringConst`.
pub(crate) fn make_bit_string_const(s: impl Into<String>, location: i32) -> Node {
    AConst {
        val: Value::BitString(s.into()),
        location,
    }
    .into()
}

/// `makeNullAConst`.
pub(crate) fn make_null_a_const(location: i32) -> Node {
    AConst {
        val: Value::Null,
        location,
    }
    .into()
}

/// `makeAConst`.
pub(crate) fn make_a_const(value: Value, location: i32) -> Node {
    match value {
        Value::Float(s) => make_float_const(s, location),
        Value::Integer(i) => AConst {
            val: Value::Integer(i),
            location,
        }
        .into(),
        Value::String(s) => make_string_const(s, location),
        // C reads `val.str` regardless of the tag for the remaining kinds
        other => AConst {
            val: Value::String(other.as_str().unwrap_or_default().to_owned()),
            location,
        }
        .into(),
    }
}

/// `makeBoolAConst`: `'t'::bool` or `'f'::bool`.
pub(crate) fn make_bool_a_const(state: bool, location: i32) -> Node {
    let constant = make_string_const(if state { "t" } else { "f" }, location);
    make_type_cast(Some(constant), Some(Box::new(system_type_name("bool"))), 0, -1)
}

/// `check_qualified_name`: every name component must be a plain string.
pub(crate) fn check_qualified_name(names: &List, scanner: &Scanner<'_>) -> Result<()> {
    if names.iter().all(|n| n.str_val().is_some()) {
        Ok(())
    } else {
        Err(scanner.syntax_error("syntax error"))
    }
}

/// `check_func_name`.
pub(crate) fn check_func_name(names: List, scanner: &Scanner<'_>) -> Result<List> {
    check_qualified_name(&names, scanner)?;
    Ok(names)
}

/// `check_indirection`: `*` may only appear last.
pub(crate) fn check_indirection(indirection: List, scanner: &Scanner<'_>) -> Result<List> {
    let len = indirection.len();
    if indirection
        .iter()
        .take(len.saturating_sub(1))
        .any(|n| matches!(n, Node::AStar(_)))
    {
        return Err(scanner.syntax_error("improper use of \"*\""));
    }
    Ok(indirection)
}

/// `makeParamRef`.
pub(crate) fn make_param_ref(number: i32, location: i32) -> Node {
    ParamRef {
        number,
        location,
        name: None,
    }
    .into()
}

/// `makeNamedParamRef`.
pub(crate) fn make_named_param_ref(name: impl Into<String>, location: i32) -> Node {
    ParamRef {
        number: 0,
        location,
        name: Some(name.into()),
    }
    .into()
}

/// `insertSelectOptions`: attaches ORDER BY, locking, LIMIT/OFFSET and WITH clauses
/// to a statement built by `select_clause`.
pub(crate) fn insert_select_options(
    stmt: &mut Node,
    sort_clause: List,
    locking_clause: List,
    limit: SelectLimit,
    with_clause: Option<Box<WithClause>>,
) -> Result<()> {
    let Node::SelectStmt(stmt) = stmt else {
        // the C code computes the error position with the unimplemented exprLocation()
        return Err(unimplemented("exprLocation"));
    };
    if !sort_clause.is_empty() {
        if !stmt.sort_clause.is_empty() {
            return Err(unimplemented("exprLocation"));
        }
        stmt.sort_clause = sort_clause;
    }
    stmt.locking_clause.extend(locking_clause);
    if let Some(offset) = limit.offset {
        if stmt.limit_offset.is_some() {
            return Err(unimplemented("exprLocation"));
        }
        stmt.limit_offset = Some(offset);
    }
    if let Some(count) = limit.count {
        if stmt.limit_count.is_some() {
            return Err(unimplemented("exprLocation"));
        }
        stmt.limit_count = Some(count);
    }
    if limit.offset_first {
        stmt.offset_first = true;
    }
    if let Some(with) = with_clause {
        if stmt.with_clause.is_some() {
            return Err(unimplemented("exprLocation"));
        }
        stmt.with_clause = Some(with);
    }
    Ok(())
}

/// Splits `OptPartitionSortedOptions` (`PARTITIONED BY` / `SORTED BY` options) into
/// the partition and sort lists of `CREATE TABLE`; later options win.
pub(crate) fn partition_and_sort_lists(options: List) -> Result<(List, List)> {
    let mut partition_list = List::new();
    let mut sort_list = List::new();
    for option in options {
        let Node::DefElem(def) = option else {
            return Err(internal(format!("expected DefElem, found {}", option.name())));
        };
        match def.defname.as_deref() {
            Some("partitioned_by") => partition_list = into_list(def.arg)?,
            Some("sorted_by") => sort_list = into_list(def.arg)?,
            _ => {}
        }
    }
    Ok((partition_list, sort_list))
}

/// `makeSetOp`.
pub(crate) fn make_set_op(op: SetOperation, all: bool, larg: Option<Node>, rarg: Option<Node>) -> Node {
    SelectStmt {
        op,
        all,
        larg,
        rarg,
        ..SelectStmt::default()
    }
    .into()
}

/// `SystemFuncName`: `main.<name>`.
pub(crate) fn system_func_name(name: &str) -> List {
    vec![Node::string(DEFAULT_SCHEMA), Node::string(name)]
}

/// `SystemTypeName`.
pub(crate) fn system_type_name(name: &str) -> TypeName {
    make_type_name_from_name_list(vec![Node::string(name)])
}

/// `doNegate`: folds a minus sign into a numeric constant.
pub(crate) fn do_negate(node: Option<Node>, location: i32) -> Node {
    match node {
        Some(Node::AConst(mut constant)) => {
            // report the constant's location as that of the '-' sign
            constant.location = location;
            match &mut constant.val {
                Value::Integer(i) => {
                    *i = -*i;
                    Node::AConst(constant)
                }
                value @ Value::Float(_) => {
                    do_negate_float(value);
                    Node::AConst(constant)
                }
                _ => make_simple_a_expr(AExprKind::Op, "-", None, Some(Node::AConst(constant)), location)
                    .into(),
            }
        }
        other => make_simple_a_expr(AExprKind::Op, "-", None, other, location).into(),
    }
}

/// `doNegateFloat`: toggles the sign of a float literal's text.
pub(crate) fn do_negate_float(value: &mut Value) {
    if let Value::Float(text) = value {
        let unsigned = text.strip_prefix('+').unwrap_or(text);
        *text = match unsigned.strip_prefix('-') {
            Some(positive) => positive.to_owned(),
            None => format!("-{unsigned}"),
        };
    }
}

/// `makeAndExpr`: flattens `a AND b AND c` into one `BoolExpr`.
pub(crate) fn make_and_expr(lexpr: Option<Node>, rexpr: Option<Node>, location: i32) -> Result<Node> {
    make_bool_chain(BoolExprType::And, lexpr, rexpr, location)
}

/// `makeOrExpr`: flattens `a OR b OR c` into one `BoolExpr`.
pub(crate) fn make_or_expr(lexpr: Option<Node>, rexpr: Option<Node>, location: i32) -> Result<Node> {
    make_bool_chain(BoolExprType::Or, lexpr, rexpr, location)
}

fn make_bool_chain(
    op: BoolExprType,
    lexpr: Option<Node>,
    rexpr: Option<Node>,
    location: i32,
) -> Result<Node> {
    let lexpr = required(lexpr)?;
    let rexpr = required(rexpr)?;
    // The C code looks through AEXPR_PAREN nodes, which this grammar never builds.
    match lexpr {
        Node::BoolExpr(mut chain) if chain.boolop == op => {
            chain.args.push(rexpr);
            Ok(Node::BoolExpr(chain))
        }
        lexpr => Ok(make_bool_expr(op, vec![lexpr, rexpr], location).into()),
    }
}

/// `makeNotExpr`.
pub(crate) fn make_not_expr(expr: Option<Node>, location: i32) -> Result<Node> {
    Ok(make_bool_expr(BoolExprType::Not, vec![required(expr)?], location).into())
}

/// `SplitColQualList`: separates constraints from the (at most one) COLLATE clause.
pub(crate) fn split_col_qual_list(
    qual_list: List,
    scanner: &Scanner<'_>,
) -> Result<(List, Option<Box<CollateClause>>)> {
    let mut constraints = List::with_capacity(qual_list.len());
    let mut collate: Option<Box<CollateClause>> = None;
    for node in qual_list {
        match node {
            Node::Constraint(_) => constraints.push(node),
            Node::CollateClause(clause) => {
                if collate.is_some() {
                    return Err(
                        scanner.error_at("multiple COLLATE clauses not allowed".into(), clause.location)
                    );
                }
                collate = Some(clause);
            }
            // elog() is unimplemented in libpg_query
            _ => return Err(unimplemented("elog")),
        }
    }
    Ok((constraints, collate))
}

/// Flags of the node being built that `processCASbits` may set.
#[derive(Default)]
pub(crate) struct CasTargets<'n> {
    pub(crate) deferrable: Option<&'n mut bool>,
    pub(crate) initdeferred: Option<&'n mut bool>,
    pub(crate) not_valid: Option<&'n mut bool>,
    pub(crate) no_inherit: Option<&'n mut bool>,
}

/// `processCASbits`: applies `ConstraintAttributeSpec` bits to a constraint,
/// rejecting attributes the constraint type does not support.
pub(crate) fn process_cas_bits(
    cas_bits: i32,
    location: i32,
    constr_type: &str,
    targets: CasTargets<'_>,
    scanner: &Scanner<'_>,
) -> Result<()> {
    use constraint_attr::{DEFERRABLE, INITIALLY_DEFERRED, NO_INHERIT, NOT_VALID};
    let CasTargets {
        mut deferrable,
        mut initdeferred,
        mut not_valid,
        mut no_inherit,
    } = targets;
    for flag in [&mut deferrable, &mut initdeferred, &mut not_valid] {
        if let Some(flag) = flag.as_deref_mut() {
            *flag = false;
        }
    }
    let unsupported = |what: &str| {
        scanner.error_at(
            format!("{constr_type} constraints cannot be marked {what}"),
            location,
        )
    };
    if cas_bits & (DEFERRABLE | INITIALLY_DEFERRED) != 0 {
        *deferrable.ok_or_else(|| unsupported("DEFERRABLE"))? = true;
    }
    if cas_bits & INITIALLY_DEFERRED != 0 {
        *initdeferred.ok_or_else(|| unsupported("DEFERRABLE"))? = true;
    }
    if cas_bits & NOT_VALID != 0 {
        *not_valid.ok_or_else(|| unsupported("NOT VALID"))? = true;
    }
    if cas_bits & NO_INHERIT != 0 {
        *no_inherit.ok_or_else(|| unsupported("NO INHERIT"))? = true;
    }
    Ok(())
}

/// `makeRecursiveViewSelect`: rewrites `CREATE RECURSIVE VIEW v (cols) AS q` into
/// `WITH RECURSIVE v (cols) AS (q) SELECT cols FROM v`.
pub(crate) fn make_recursive_view_select(
    relname: impl Into<String>,
    aliases: List,
    query: Option<Node>,
) -> Result<Node> {
    let relname = relname.into();
    let target_list = aliases
        .iter()
        .map(|alias| {
            let column = ColumnRef {
                fields: vec![Node::string(str_val(alias)?)],
                location: -1,
            };
            Ok(ResTarget {
                name: None,
                indirection: List::new(),
                val: Some(column.into()),
                location: -1,
            }
            .into())
        })
        .collect::<Result<List>>()?;
    let cte = CommonTableExpr {
        ctename: Some(relname.clone()),
        aliascolnames: aliases,
        ctequery: query,
        location: -1,
        ..CommonTableExpr::default()
    };
    let with = WithClause {
        ctes: vec![cte.into()],
        recursive: true,
        location: -1,
    };
    let from = make_range_var(None, Some(relname), -1);
    Ok(SelectStmt {
        with_clause: Some(Box::new(with)),
        target_list,
        from_clause: vec![from.into()],
        ..SelectStmt::default()
    }
    .into())
}
