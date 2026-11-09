use roastedduck_ast::json::JsonSerializeOptions;
use roastedduck_ast::{
    AlterInfoKind, AlterTableInfo, CatalogType, Constraint, CreateInfo, CreateInfoKind, ExpressionType,
    InsertColumnOrder, OnConflictAction, OnCreateConflict, ParsedExpressionKind, QueryNodeKind, SetScope,
    SqlStatement, SqlStatementKind, TableColumnType, TableRefKind,
};
use roastedduck_common::ErrorKind;
use roastedduck_types::{LogicalTypeId, TypeInfo};

use crate::json::serialize_sql;
use crate::{SYNTAX_ERROR, parse};

fn json(sql: &str) -> String {
    serialize_sql(sql, &JsonSerializeOptions::default())
}

fn one(sql: &str) -> SqlStatement {
    let mut statements = parse(sql).unwrap_or_else(|error| panic!("{sql}: {error}"));
    assert_eq!(statements.len(), 1, "{sql}");
    statements.remove(0)
}

fn create(sql: &str) -> Box<CreateInfo> {
    match one(sql).kind {
        SqlStatementKind::Create(info) => info,
        other => panic!("{sql}: not a CREATE: {other:?}"),
    }
}

fn error(sql: &str) -> roastedduck_common::Error {
    parse(sql).expect_err(sql)
}

#[test]
fn splits_statements_and_their_text() {
    let statements = parse("SELECT 1; SELECT 2").expect("parses");
    assert_eq!(statements.len(), 2);
    assert_eq!(statements[0].query, "SELECT 1");
    assert_eq!(statements[1].query, " SELECT 2");
    assert_eq!(statements[1].location, 0);
    assert_eq!(statements[1].length, 9);
    assert_eq!(parse("  ;; ").expect("parses"), []);
}

#[test]
fn syntax_errors_are_tagged() {
    let error = error("SELEC 1");
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
    let SqlStatementKind::Select(select) = one("SELECT NOT (a IN (1, 2))").kind else {
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
    assert!(
        error(&deep)
            .raw_message()
            .starts_with("Max expression depth limit of 1000 exceeded")
    );
}

#[test]
fn parameters_are_numbered_per_statement() {
    let statements = parse("SELECT $a, $b; SELECT $c").expect("parses");
    let names: Vec<Vec<(roastedduck_common::ByteString, u64)>> = statements
        .iter()
        .map(|statement| {
            statement
                .named_param_map
                .iter()
                .map(|(k, v)| (k.to_owned(), *v))
                .collect()
        })
        .collect();
    assert_eq!(names[0].len(), 2);
    assert!(names[0].contains(&("a".into(), 1)));
    assert_eq!(names[1], [("c".into(), 1)]);
    assert_eq!(
        error("SELECT $a, ?").raw_message(),
        "Mixing named and positional parameters is not supported yet"
    );
    // Like DuckDB, the kind of the last parameter carries over to the next statement.
    assert_eq!(
        error("SELECT $a; SELECT ?").raw_message(),
        "Mixing named and positional parameters is not supported yet"
    );
}

#[test]
fn create_table_with_constraints() {
    let info = create(
        "CREATE TABLE s.t (a INT PRIMARY KEY, b VARCHAR DEFAULT 'x', c INT GENERATED ALWAYS AS (a + 1), \
         CHECK (a > 0), FOREIGN KEY (b) REFERENCES u (x))",
    );
    assert_eq!(info.schema, "s");
    assert!(!info.sql.as_bytes().first().is_some_and(u8::is_ascii_whitespace));
    let CreateInfoKind::Table(table) = &info.kind else {
        panic!("not a table");
    };
    assert_eq!(table.table, "t");
    let columns = &table.columns.columns;
    assert_eq!(columns.len(), 3);
    assert!(columns[1].expression.is_some());
    assert_eq!(columns[2].category, TableColumnType::Generated);
    let generated = columns[2].expression.as_ref().expect("generated expression");
    assert_eq!(generated.expression_type, ExpressionType::OperatorCast);
    assert!(matches!(table.constraints[0], Constraint::Unique(ref unique) if unique.is_primary_key));
    assert!(matches!(table.constraints[1], Constraint::Check(_)));
    assert!(matches!(table.constraints[2], Constraint::ForeignKey(_)));
    assert_eq!(
        error("CREATE TABLE t (HELLO INT, hello INT)").to_string(),
        "Catalog Error: Column with name hello already exists!"
    );
}

#[test]
fn non_constant_column_defaults_are_materialized() {
    let SqlStatementKind::Multi(statements) = one("ALTER TABLE t ADD COLUMN x INT DEFAULT random()").kind
    else {
        panic!("not a multi statement");
    };
    assert_eq!(statements.len(), 3);
    assert!(matches!(statements[1].kind, SqlStatementKind::Update(_)));
    let SqlStatementKind::Alter(alter) = &statements[2].kind else {
        panic!("not an ALTER");
    };
    assert!(matches!(
        alter.kind,
        AlterInfoKind::AlterTable(AlterTableInfo::SetDefault(_))
    ));
    let SqlStatementKind::Alter(alter) = one("ALTER TABLE t ADD COLUMN x INT DEFAULT 1").kind else {
        panic!("not an ALTER");
    };
    assert!(matches!(
        alter.kind,
        AlterInfoKind::AlterTable(AlterTableInfo::AddColumn(_))
    ));
}

#[test]
fn pivot_without_values_creates_an_enum() {
    let SqlStatementKind::Multi(statements) = one("WITH c AS (SELECT 1) PIVOT t ON x USING sum(y)").kind
    else {
        panic!("not a multi statement");
    };
    assert_eq!(statements.len(), 2);
    let SqlStatementKind::Create(info) = &statements[0].kind else {
        panic!("not a CREATE");
    };
    assert!(info.temporary);
    assert_eq!(info.on_conflict, OnCreateConflict::ReplaceOnConflict);
    let CreateInfoKind::Type(type_info) = &info.kind else {
        panic!("not a CREATE TYPE");
    };
    assert!(type_info.name.starts_with(b"__pivot_enum_"));
    let query = type_info.query.as_ref().expect("enum query");
    assert!(query.node.cte_map.map.contains_key("c"));
    assert!(matches!(statements[1].kind, SqlStatementKind::Select(_)));
    assert!(
        error("PIVOT (SELECT $1 AS x) ON x").raw_message().starts_with(
            "PIVOT statements with pivot elements extracted from the data cannot have parameters"
        )
    );
}

#[test]
fn insert_with_on_conflict() {
    let SqlStatementKind::Insert(insert) =
        one("INSERT INTO t BY NAME (a, b) VALUES (1, 2) ON CONFLICT (a) DO UPDATE SET b = excluded.b").kind
    else {
        panic!("not an INSERT");
    };
    assert_eq!(insert.columns, ["a", "b"]);
    assert_eq!(insert.column_order, InsertColumnOrder::InsertByName);
    let conflict = insert.on_conflict_info.as_ref().expect("ON CONFLICT");
    assert_eq!(conflict.action_type, OnConflictAction::Update);
    assert_eq!(conflict.indexed_columns, ["a"]);
    assert!(insert.table_ref.is_some());
}

#[test]
fn copy_options_stay_expressions() {
    let SqlStatementKind::Copy(copy) = one("COPY t (a) FROM 'f.csv' (HEADER, DELIMITER ',')").kind else {
        panic!("not a COPY");
    };
    assert_eq!(copy.file_path, "f.csv");
    assert!(copy.is_from);
    assert_eq!(copy.select_list, ["a"]);
    assert!(copy.parsed_options.get("header").is_some_and(Option::is_none));
    assert!(copy.parsed_options.get("delimiter").is_some_and(Option::is_some));
    assert_eq!(
        error("COPY t TO 'f' (a 1, A 2)").raw_message(),
        "Unexpected duplicate option \"A\""
    );
}

#[test]
fn settings_and_pragmas() {
    let SqlStatementKind::Set(set) = one("SET GLOBAL threads = 4").kind else {
        panic!("not a SET");
    };
    assert_eq!(set.scope, SetScope::Global);
    assert!(set.value.is_some());
    let SqlStatementKind::Set(reset) = one("RESET threads").kind else {
        panic!("not a RESET");
    };
    assert!(reset.value.is_none());
    let SqlStatementKind::Set(use_schema) = one("USE db.\"My Schema\"").kind else {
        panic!("not a USE");
    };
    assert_eq!(use_schema.name, "schema");
    assert!(matches!(
        one("PRAGMA table_info='t'").kind,
        SqlStatementKind::Pragma(_)
    ));
    assert!(matches!(one("PRAGMA threads=4").kind, SqlStatementKind::Set(_)));
}

#[test]
fn secrets_types_and_sequences() {
    let info = create("CREATE SECRET (TYPE S3, KEY_ID 'k')");
    let CreateInfoKind::Secret(secret) = &info.kind else {
        panic!("not a secret");
    };
    assert_eq!(secret.name, "__default_s3");
    assert!(secret.options.contains_key("key_id"));
    let info = create("CREATE TYPE mood AS ENUM ('happy', 'sad')");
    let CreateInfoKind::Type(mood) = &info.kind else {
        panic!("not a type");
    };
    assert_eq!(mood.logical_type.id(), LogicalTypeId::Enum);
    let error = error("CREATE TYPE mood AS ENUM ('sad', 'sad')");
    assert_eq!(error.kind(), ErrorKind::InvalidInput);
    let info = create("CREATE SEQUENCE seq INCREMENT BY -1");
    let CreateInfoKind::Sequence(sequence) = &info.kind else {
        panic!("not a sequence");
    };
    assert_eq!(
        (sequence.min_value, sequence.max_value, sequence.start_value),
        (i64::MIN, -1, -1)
    );
}

#[test]
fn compression_names_ignore_ascii_case_and_preserve_native_column_names() {
    for name in ["zstd", "ZSTD", "ZsTd"] {
        let mut sql = b"CREATE TABLE t (\"col\xff\" JSON USING COMPRESSION '".to_vec();
        sql.extend_from_slice(name.as_bytes());
        sql.extend_from_slice(b"')");
        let mut statements = crate::parse_bytes(&sql).expect("case-insensitive compression name");
        let SqlStatementKind::Create(info) = statements.remove(0).kind else {
            panic!("not a CREATE");
        };
        let CreateInfoKind::Table(table) = &info.kind else {
            panic!("not a table");
        };
        let column = &table.columns.columns[0];
        assert_eq!(column.name.as_bytes(), b"col\xff");
        assert_eq!(
            column.compression_type,
            roastedduck_ast::CompressionType::CompressionZstd
        );
    }
}

#[test]
fn drop_and_casts() {
    let SqlStatementKind::Drop(drop) = one("DROP VIEW IF EXISTS c.s.v CASCADE").kind else {
        panic!("not a DROP");
    };
    assert_eq!(drop.catalog_type, CatalogType::ViewEntry);
    assert_eq!(
        (
            drop.catalog.as_bytes(),
            drop.schema.as_bytes(),
            drop.name.as_bytes()
        ),
        (b"c".as_slice(), b"s".as_slice(), b"v".as_slice())
    );
    assert!(drop.cascade);
    let SqlStatementKind::Select(select) = one("SELECT x::int4[]").kind else {
        panic!("not a select");
    };
    let QueryNodeKind::Select(node) = &select.node.kind else {
        panic!("not a select node");
    };
    let ParsedExpressionKind::Cast(cast) = &node.select_list[0].kind else {
        panic!("not a cast");
    };
    let Some(TypeInfo::Unbound(list)) = cast.cast_type.info() else {
        panic!("not an unbound type");
    };
    assert_eq!(list.name, "list");
    assert!(matches!(node.from_table.kind, TableRefKind::Empty));
}
