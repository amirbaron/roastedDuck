//! Debug verification must execute distinct paths, including pending-query entry.
use roastedduck::{Database, ErrorKind, Result, Value};

#[test]
fn pending_verification_checks_results_and_runs_all_regular_variants() -> Result<()> {
    let database = Database::open_in_memory()?;
    let mut connection = database.connect()?;
    connection.query("CREATE SEQUENCE s")?;
    connection.query("PRAGMA enable_verification")?;
    let sql = "SELECT nextval('s') AS x";
    let statement = roastedduck_parser::parse(sql)?.remove(0);
    let error = connection
        .pending_query(sql, statement, false)
        .expect_err("volatile results must differ");
    assert_eq!(error.kind(), ErrorKind::Invalid);
    assert!(
        error
            .raw_message()
            .starts_with("Copied statement differs from original result!")
    );
    connection.query("PRAGMA disable_verification")?;
    // Independently observed on C++ 1.5.6: original, four base variants, parsed,
    // and prepared execute; EXPLAIN does not evaluate nextval.
    assert_eq!(
        connection.query("SELECT currval('s')")?.value(0, 0),
        Value::bigint(7)
    );
    Ok(())
}

#[test]
fn verification_is_session_local_and_survives_session_restore() -> Result<()> {
    let database = Database::open_in_memory()?;
    let mut first = database.connect()?;
    first.query("PRAGMA enable_verification")?;
    let snapshot = first.session_snapshot();
    let mut second = database.connect()?;
    second.query("CREATE SEQUENCE s")?;
    assert_eq!(second.query("SELECT nextval('s')")?.value(0, 0), Value::bigint(1));
    second.restore_session(snapshot)?;
    assert_eq!(
        second
            .query("SELECT nextval('s')")
            .expect_err("restored verification must run")
            .kind(),
        ErrorKind::Invalid
    );
    second.query("PRAGMA disable_verification")?;
    assert_eq!(second.query("SELECT currval('s')")?.value(0, 0), Value::bigint(8));
    Ok(())
}
