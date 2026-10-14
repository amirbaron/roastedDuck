//! Parser tests: representative trees, statement splitting, errors, and the
//! consistency of the action table with the parse tables.

use crate::nodes::{
    AConst, AExpr, AExprKind, BoolExpr, BoolExprType, FuncCall, Node, ResTarget, SelectStmt, SetOperation,
    SubLinkType, TypeCast, Value,
};
use crate::{ParserOptions, RawStmt, parse, parse_with_options};

fn statements(sql: &str) -> Vec<RawStmt> {
    match parse(sql) {
        Ok(stmts) => stmts,
        Err(err) => panic!("{sql}: {err}"),
    }
}

fn select(sql: &str) -> SelectStmt {
    let mut stmts = statements(sql);
    assert_eq!(stmts.len(), 1);
    match stmts.remove(0).stmt.take() {
        Some(Node::SelectStmt(select)) => *select,
        other => panic!("not a SELECT: {other:?}"),
    }
}

/// The expression of the first select-list entry.
fn first_target(sql: &str) -> Node {
    let mut select = select(sql);
    match select.target_list.remove(0) {
        Node::ResTarget(target) => target.val.expect("target value"),
        other => panic!("not a ResTarget: {other:?}"),
    }
}

fn error(sql: &str) -> (String, Option<usize>) {
    let err = parse(sql).expect_err(sql);
    assert_eq!(err.kind(), rustdb_common::ErrorKind::Parser, "{err}");
    (err.raw_message().to_owned(), err.query_location())
}

fn name(node: &Node) -> &str {
    node.str_val().expect("string value")
}

#[test]
fn select_constant() {
    let select = select("SELECT 1");
    assert_eq!(select.target_list.len(), 1);
    let Node::ResTarget(target) = &select.target_list[0] else {
        panic!("{select:?}")
    };
    assert_eq!(
        **target,
        ResTarget {
            name: None,
            indirection: vec![],
            val: Some(
                AConst {
                    val: Value::Integer(1),
                    location: 7
                }
                .into()
            ),
            location: 7,
        }
    );
    assert!(select.from_clause.is_empty() && select.where_clause.is_none());
}

#[test]
fn select_from_where_order_limit() {
    let select = select("select a, t.b from s.t where a > 1 order by 1 limit 3");
    let Node::RangeVar(table) = &select.from_clause[0] else {
        panic!()
    };
    assert_eq!(table.schemaname.as_deref(), Some("s"));
    assert_eq!(table.relname.as_deref(), Some("t"));
    let Node::ResTarget(second) = &select.target_list[1] else {
        panic!()
    };
    let Some(Node::ColumnRef(column)) = &second.val else {
        panic!()
    };
    assert_eq!(column.fields.iter().map(name).collect::<Vec<_>>(), ["t", "b"]);
    let Some(Node::AExpr(cond)) = &select.where_clause else {
        panic!()
    };
    assert_eq!((cond.kind, name(&cond.name[0])), (AExprKind::Op, ">"));
    assert_eq!(select.sort_clause.len(), 1);
    assert!(matches!(&select.limit_count, Some(Node::AConst(c)) if c.val == Value::Integer(3)));
    // with no OFFSET, insertSelectOptions marks the offset as "first"
    assert!(select.offset_first);
}

#[test]
fn identifier_case() {
    let Node::ColumnRef(column) = first_target("SELECT MyCol") else {
        panic!()
    };
    assert_eq!(name(&column.fields[0]), "MyCol");
    let folded = parse_with_options(
        "SELECT MyCol, \"Quoted\"",
        &ParserOptions {
            preserve_identifier_case: false,
        },
    )
    .expect("parse");
    let Some(Node::SelectStmt(select)) = &folded[0].stmt else {
        panic!()
    };
    let names: Vec<_> = select
        .target_list
        .iter()
        .map(|t| match t {
            Node::ResTarget(t) => match &t.val {
                Some(Node::ColumnRef(c)) => name(&c.fields[0]).to_owned(),
                other => panic!("{other:?}"),
            },
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(names, ["mycol", "Quoted"]);
}

#[test]
fn operator_precedence() {
    // 1 + 2 * 3 parses as 1 + (2 * 3)
    let Node::AExpr(plus) = first_target("SELECT 1 + 2 * 3") else {
        panic!()
    };
    assert_eq!(name(&plus.name[0]), "+");
    let Some(Node::AExpr(times)) = &plus.rexpr else {
        panic!("{plus:?}")
    };
    assert_eq!(name(&times.name[0]), "*");
    // AND binds tighter than OR, and AND chains are flattened
    let Node::BoolExpr(or) = first_target("SELECT a OR b AND c AND d") else {
        panic!()
    };
    assert_eq!(or.boolop, BoolExprType::Or);
    let Node::BoolExpr(and) = &or.args[1] else {
        panic!()
    };
    assert_eq!((and.boolop, and.args.len()), (BoolExprType::And, 3));
}

#[test]
fn negative_constants_fold() {
    // doNegate folds the sign into the constant and reports the sign's location
    assert_eq!(
        first_target("SELECT -5"),
        Node::from(AConst {
            val: Value::Integer(-5),
            location: 7
        })
    );
    assert_eq!(
        first_target("SELECT - 1.5"),
        Node::from(AConst {
            val: Value::Float("-1.5".into()),
            location: 7
        })
    );
    let Node::AExpr(minus) = first_target("SELECT -x") else {
        panic!()
    };
    assert!(minus.lexpr.is_none() && name(&minus.name[0]) == "-");
}

#[test]
fn lookahead_tokens_change_the_parse() {
    // NOT IN (list): scalar IN with "<>"
    let Node::AExpr(not_in) = first_target("SELECT x NOT IN (1, 2)") else {
        panic!()
    };
    assert_eq!((not_in.kind, name(&not_in.name[0])), (AExprKind::In, "<>"));
    assert!(matches!(&not_in.rexpr, Some(Node::List(items)) if items.len() == 2));
    // IN (subquery) becomes an ANY sublink
    let Node::SubLink(sublink) = first_target("SELECT x IN (SELECT 1)") else {
        panic!()
    };
    assert_eq!(sublink.sub_link_type, SubLinkType::Any);
    assert!(sublink.oper_name.is_empty() && sublink.testexpr.is_some());
}

#[test]
fn casts_and_functions() {
    let Node::TypeCast(cast) = first_target("SELECT 'a'::int") else {
        panic!()
    };
    let TypeCast {
        arg,
        type_name,
        try_cast,
        location,
    } = *cast;
    assert!(matches!(arg, Some(Node::AConst(_))));
    assert_eq!((try_cast, location), (0, 10));
    assert_eq!(
        type_name
            .expect("type")
            .names
            .iter()
            .map(name)
            .collect::<Vec<_>>(),
        ["int4"]
    );
    let Node::FuncCall(call) = first_target("SELECT count(DISTINCT x) FILTER (WHERE x > 0)") else {
        panic!()
    };
    let FuncCall {
        funcname,
        args,
        agg_distinct,
        agg_filter,
        ..
    } = *call;
    assert_eq!(funcname.iter().map(name).collect::<Vec<_>>(), ["count"]);
    assert!(agg_distinct && agg_filter.is_some() && args.len() == 1);
}

#[test]
fn distinct_forms() {
    assert_eq!(select("SELECT x FROM t").distinct_clause, None);
    assert_eq!(select("SELECT DISTINCT x FROM t").distinct_clause, Some(vec![]));
    let on = select("SELECT DISTINCT ON (a) x FROM t")
        .distinct_clause
        .expect("distinct on");
    assert!(matches!(&on[..], [Node::ColumnRef(_)]));
}

#[test]
fn set_operations() {
    let union = select("SELECT 1 UNION ALL SELECT 2 ORDER BY 1");
    assert_eq!((union.op, union.all), (SetOperation::Union, true));
    assert!(matches!(union.larg, Some(Node::SelectStmt(_))) && union.sort_clause.len() == 1);
}

#[test]
fn statement_splitting() {
    let stmts = statements("SELECT 1; SELECT 2;;");
    let spans: Vec<_> = stmts.iter().map(|s| (s.stmt_location, s.stmt_len)).collect();
    assert_eq!(spans, [(0, 8), (9, 9)]);
    assert!(statements("").is_empty());
    assert!(statements(" ; -- nothing\n").is_empty());
    // a statement without a trailing semicolon runs to the end (length 0)
    assert_eq!(
        statements("SELECT 1; SELECT 2")
            .iter()
            .map(|s| s.stmt_len)
            .collect::<Vec<_>>(),
        [8, 0]
    );
}

#[test]
fn other_statements() {
    let stmts = statements(
        "CREATE TABLE IF NOT EXISTS t (a INT PRIMARY KEY); INSERT INTO t VALUES (1); DROP TABLE t",
    );
    let kinds: Vec<_> = stmts.iter().map(|s| s.stmt.as_ref().map(Node::name)).collect();
    assert_eq!(kinds, [Some("CreateStmt"), Some("InsertStmt"), Some("DropStmt")]);
    let Some(Node::InsertStmt(insert)) = &stmts[1].stmt else {
        panic!()
    };
    let relation = insert.relation.as_deref().expect("relation");
    assert_eq!((relation.relname.as_deref(), relation.location), (Some("t"), 62));
    // a statement starts right after the previous semicolon
    assert_eq!(stmts[1].stmt_location, 49);
}

#[test]
fn syntax_errors() {
    assert_eq!(
        error("SELECT * FRM t"),
        ("syntax error at or near \"t\"".to_owned(), Some(13))
    );
    assert_eq!(
        error("SELECT 1 +"),
        ("syntax error at end of input".to_owned(), Some(10))
    );
    // positions count characters, not bytes
    assert_eq!(
        error("SELECT 'é' FRM t"),
        ("syntax error at or near \"t\"".to_owned(), Some(15))
    );
    assert_eq!(
        error("SELECT 'abc"),
        (
            "unterminated quoted string at or near \"'abc\"".to_owned(),
            Some(7)
        )
    );
}

#[test]
fn grammar_action_errors() {
    assert_eq!(
        error("SELECT 1 LIMIT 1, 2"),
        ("errhint NOT IMPLEMENTED".to_owned(), None)
    );
    assert_eq!(
        error("SELECT * FROM a.b.c.d"),
        ("NameListToString NOT IMPLEMENTED".to_owned(), None)
    );
    assert_eq!(
        error("(SELECT 1 ORDER BY 1) ORDER BY 2"),
        ("exprLocation NOT IMPLEMENTED".to_owned(), None)
    );
    assert_eq!(
        error("CREATE GLOBAL TEMP TABLE t(a INT)"),
        (
            "GLOBAL is deprecated in temporary table creation".to_owned(),
            Some(7)
        )
    );
    assert_eq!(
        error("SELECT CAST(1 AS FLOAT(60))"),
        (
            "precision for type float must be less than 54 bits".to_owned(),
            Some(23)
        )
    );
}

#[test]
fn parser_stack_is_limited_like_bison() {
    // bison gives up at YYMAXDEPTH (10000) stacked states
    let deep = format!("SELECT {}1{}", "(".repeat(10_000), ")".repeat(10_000));
    assert_eq!(
        error(&deep),
        ("memory exhausted at or near \"(\"".to_owned(), Some(10_003))
    );
    let fine = format!("SELECT {}1{}", "(".repeat(9_990), ")".repeat(9_990));
    assert!(parse(&fine).is_ok());
}

#[test]
fn deep_trees_are_dropped_without_recursion() {
    // left-recursive rules nest without growing the parser stack
    let sum = format!("SELECT 1{}", "+1".repeat(200_000));
    assert_eq!(statements(&sum).len(), 1);
    // ... and are torn down on errors too
    assert_eq!(error(&format!("{sum} FROM")).0, "syntax error at end of input");
}

#[test]
fn rule_actions_match_grammar() {
    use crate::actions::RULE_PRODUCTIONS;
    use crate::tables::{YYNRULES, YYPRHS, YYR1, YYRHS, YYTNAME};
    assert_eq!(RULE_PRODUCTIONS.len(), YYNRULES + 1);
    for (rule, expected) in RULE_PRODUCTIONS.iter().enumerate().skip(1) {
        let mut production = format!("{}:", YYTNAME[usize::from(YYR1[rule])]);
        let mut i = usize::from(YYPRHS[rule]);
        while YYRHS[i] >= 0 {
            production.push(' ');
            production.push_str(YYTNAME[YYRHS[i].unsigned_abs() as usize]);
            i += 1;
        }
        assert_eq!(&production, expected, "rule {rule}");
    }
}

#[test]
fn nodes_render_as_json() {
    let json = crate::nodes::to_json(&statements("SELECT 'a\"b'"));
    assert!(
        json.contains(r#""val":{"node":"AConst","val":{"node":"Value","str":"a\"b"},"location":7}"#),
        "{json}"
    );
}

#[test]
fn expression_helpers() {
    let a = AExpr {
        kind: AExprKind::Op,
        ..AExpr::default()
    };
    assert_eq!(Node::from(a.clone()).name(), "AExpr");
    assert_eq!(Node::from(BoolExpr::default()).downcast_ref::<AExpr>(), None);
    assert_eq!(Node::from(a.clone()).downcast_ref::<AExpr>(), Some(&a));
}
