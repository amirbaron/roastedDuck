//! Dynamic PIVOTs expose their enum creation and SELECT through the connection preprocessor.
use roastedduck::Database;
use roastedduck_ast::SqlStatementKind;
use roastedduck_common::Result;

#[test]
fn dynamic_pivot_preprocessing_matches_text_and_byte_ingress() -> Result<()> {
    let database = Database::open_in_memory()?;
    let mut connection = database.connect()?;
    connection.query("CREATE TABLE flights(city VARCHAR,carrier VARCHAR);INSERT INTO flights VALUES ('x','A'),('x','B'),('y','A')")?;
    let sql = "PIVOT flights ON carrier USING count(*) GROUP BY city";
    for statements in [
        connection.parse_statements(sql)?,
        connection.parse_statements_bytes(sql.as_bytes())?,
    ] {
        assert_eq!(statements.len(), 2);
        assert!(matches!(statements[0].kind, SqlStatementKind::Create(_)));
        assert!(matches!(statements[1].kind, SqlStatementKind::Select(_)));
    }
    let result = connection.query(sql)?;
    assert_eq!((result.row_count(), result.column_count()), (2, 3));
    assert_eq!(result.names()[1].as_bytes(), b"A");
    assert_eq!(result.names()[2].as_bytes(), b"B");
    Ok(())
}

#[test]
fn dynamic_pivot_preserves_native_identifiers() -> Result<()> {
    let database = Database::open_in_memory()?;
    let mut connection = database.connect()?;
    connection.query(b"CREATE TABLE \"flights-\xff\"(\"city-\xe9\" VARCHAR,carrier VARCHAR);INSERT INTO \"flights-\xff\" VALUES ('x','A'),('x','B')")?;
    let sql = b"PIVOT \"flights-\xff\" ON carrier USING count(*) GROUP BY \"city-\xe9\"";
    let statements = connection.parse_statements_bytes(sql)?;
    assert_eq!(statements.len(), 2);
    let result = connection.query(sql)?;
    assert_eq!(result.row_count(), 1);
    assert_eq!(result.names()[0].as_bytes(), b"city-\xe9");
    assert_eq!(result.value(1, 0).as_i128(), Some(1));
    assert_eq!(result.value(2, 0).as_i128(), Some(1));
    Ok(())
}
