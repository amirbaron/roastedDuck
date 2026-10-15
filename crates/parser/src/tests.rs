use rustdb_ast::json::JsonSerializeOptions;
use rustdb_ast::{ExpressionType, ParsedExpressionKind, QueryNodeKind, SqlStatementKind};

use crate::json::serialize_sql;
use crate::{SYNTAX_ERROR, parse};

fn json(sql: &str) -> String {
    serialize_sql(sql, &JsonSerializeOptions::default())
}

#[test]
fn splits_statements_and_their_text() {
    let statements = parse("SELECT 1; SELECT 2").expect("parses");
    assert_eq!(statements.len(), 2);
    assert_eq!(statements[0].query, "SELECT 1");
    assert_eq!(statements[1].query, " SELECT 2");
    assert_eq!(statements[1].location, 0);
    assert_eq!(statements[1].length, 9);
}

#[test]
fn syntax_errors_are_tagged() {
    let error = parse("SELEC 1").expect_err("fails");
    assert_eq!(error.subtype(), Some(SYNTAX_ERROR));
    assert_eq!(error.position(), Some(0));
    assert_eq!(
        json("SELEC 1"),
        r#"{"error":true,"error_type":"parser","error_message":"syntax error at or near \"SELEC\"","error_subtype":"SYNTAX_ERROR","position":"0"}"#
    );
}

#[test]
fn non_select_statements_are_not_serialized() {
    assert_eq!(
        json("SET x = 1"),
        r#"{"error":true,"error_type":"not implemented","error_message":"Only SELECT statements can be serialized to json!"}"#
    );
}

#[test]
fn not_in_negates_the_comparison() {
    let statements = parse("SELECT NOT (a IN (1, 2))").expect("parses");
    let SqlStatementKind::Select(select) = &statements[0].kind else {
        panic!("not a select");
    };
    let QueryNodeKind::Select(node) = &select.node.kind else {
        panic!("not a select node");
    };
    assert_eq!(node.select_list[0].expression_type, ExpressionType::CompareNotIn);
    assert!(matches!(
        node.select_list[0].kind,
        ParsedExpressionKind::Operator(_)
    ));
}

#[test]
fn expression_depth_is_limited() {
    let sql = format!("SELECT {}1{}", "(".repeat(5), ")".repeat(5));
    assert!(parse(&sql).is_ok());
    let deep = format!("SELECT {}", vec!["1"; 2000].join("+"));
    let error = parse(&deep).expect_err("too deep");
    assert!(
        error
            .raw_message()
            .starts_with("Max expression depth limit of 1000 exceeded")
    );
}
