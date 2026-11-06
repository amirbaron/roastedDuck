//! Real PEG entry-point journeys for name-bearing grammar dispatch.
use rustdb_ast::{ParseInfo, ParsedExpressionKind, QueryNodeKind, SqlStatementKind, TableRefKind};
use rustdb_parser::ParserOptions;
fn parse(sql: &str) -> rustdb_common::Result<Vec<rustdb_ast::SqlStatement>> {
    rustdb_autocomplete::parse_query(sql, ParserOptions::default())
}
#[test]
fn install_version_and_repository_keep_unicode_names() -> rustdb_common::Result<()> {
    let statements = parse("INSTALL 'éxt' FROM 'répo' VERSION 'v-é';")?;
    let SqlStatementKind::Load(info) = &statements[0].kind else {
        panic!("not LOAD");
    };
    assert_eq!(info.filename.as_bytes(), "éxt".as_bytes());
    assert_eq!(info.repository.as_bytes(), "répo".as_bytes());
    assert_eq!(info.version.as_bytes(), "v-é".as_bytes());
    let _metadata = ParseInfo::Load(Box::new(info.clone()));
    Ok(())
}
#[test]
fn replacement_and_alias_names_reach_native_ast() -> rustdb_common::Result<()> {
    let statements = parse("SELECT * REPLACE (2 AS \"é\") FROM (SELECT 1 AS \"é\") AS \"源\";")?;
    let SqlStatementKind::Select(statement) = &statements[0].kind else {
        panic!("not SELECT");
    };
    let QueryNodeKind::Select(select) = &statement.node.kind else {
        panic!("not SELECT node");
    };
    let ParsedExpressionKind::Star(star) = &select.select_list[0].kind else {
        panic!("not STAR");
    };
    assert!(star.replace_list.contains_key("é"));
    assert_eq!(select.from_table.alias.as_bytes(), "源".as_bytes());
    assert!(matches!(select.from_table.kind, TableRefKind::Subquery(_)));
    Ok(())
}
#[test]
fn cte_and_named_window_names_reach_native_ast() -> rustdb_common::Result<()> {
    let statements = parse(
        "WITH \"源\"(\"é\") AS (SELECT 1) SELECT sum(\"é\") OVER \"窓\" FROM \"源\" WINDOW \"窓\" AS (ORDER BY \"é\");",
    )?;
    let SqlStatementKind::Select(statement) = &statements[0].kind else {
        panic!("not SELECT");
    };
    let cte = statement.node.cte_map.map.get("源").unwrap();
    assert_eq!(cte.aliases[0].as_bytes(), "é".as_bytes());
    let QueryNodeKind::Select(select) = &statement.node.kind else {
        panic!("not SELECT node");
    };
    let ParsedExpressionKind::Window(window) = &select.select_list[0].kind else {
        panic!("not WINDOW");
    };
    assert_eq!(window.function_name.as_bytes(), b"sum");
    assert_eq!(window.orders.len(), 1);
    Ok(())
}
