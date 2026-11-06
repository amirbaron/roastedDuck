//! Imports expose their real statements and retain transaction atomicity across clients.
#![allow(
    clippy::unwrap_used,
    reason = "Filesystem fixture setup and explicit expected-failure assertions"
)]
use std::path::PathBuf;

use rustdb::{Config, Database, ErrorKind, Result, Value};
use rustdb_ast::{SqlStatementKind, TransactionInvalidationPolicy, TransactionType};

struct Fixture(PathBuf);
impl Fixture {
    fn new(label: &str, missing_target: bool) -> Self {
        let path = std::env::temp_dir().join(format!("rustdb-import-{}-{label}", std::process::id()));
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("schema.sql"), "CREATE TABLE imported(i INTEGER);\n").unwrap();
        let target = if missing_target {
            "missing_copy_target"
        } else {
            "imported"
        };
        std::fs::write(
            path.join("load.sql"),
            format!("COPY {target} FROM 'exported/imported.csv' (FORMAT CSV, HEADER false);\n"),
        )
        .unwrap();
        std::fs::write(path.join("imported.csv"), "1\n2\n").unwrap();
        Self(path)
    }
    fn sql(&self) -> String {
        format!(
            "IMPORT DATABASE '{}'",
            self.0.to_str().unwrap().replace('\'', "''")
        )
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn import_extraction_exposes_implicit_and_explicit_transaction_bodies() -> Result<()> {
    let fixture = Fixture::new("extraction", false);
    let mut connection = Database::open_in_memory()?.connect()?;
    let parsed = connection.parse_statements(&fixture.sql())?;
    assert_eq!(parsed.len(), 4);
    for (index, transaction_type) in [
        (0, TransactionType::BeginTransaction),
        (3, TransactionType::Commit),
    ] {
        let SqlStatementKind::Transaction(info) = &parsed[index].kind else {
            panic!("missing synthetic transaction statement");
        };
        assert_eq!(info.transaction_type, transaction_type);
        assert_eq!(
            info.invalidation_policy,
            TransactionInvalidationPolicy::AllErrorsInvalidateTransaction
        );
        assert!(info.auto_rollback);
    }
    assert!(matches!(parsed[1].kind, SqlStatementKind::Create(_)));
    assert!(matches!(parsed[2].kind, SqlStatementKind::Copy(_)));
    assert!(connection.is_auto_commit());
    let chained = connection.parse_statements(&format!("BEGIN; {}; COMMIT", fixture.sql()))?;
    assert_eq!(chained.len(), 6);
    assert!(matches!(chained[1].kind, SqlStatementKind::Set(_)));
    assert!(matches!(chained[4].kind, SqlStatementKind::Set(_)));
    connection.query("BEGIN")?;
    let active = connection.parse_statements(&fixture.sql())?;
    assert_eq!(active.len(), 4);
    assert!(matches!(active[0].kind, SqlStatementKind::Set(_)));
    assert!(matches!(active[3].kind, SqlStatementKind::Set(_)));
    assert!(!connection.is_auto_commit());
    connection.query("ROLLBACK")?;
    Ok(())
}

#[test]
fn expanded_import_commits_actual_copy_rows_and_returns_to_auto_commit() -> Result<()> {
    let fixture = Fixture::new("success", false);
    let mut connection = Database::open_in_memory()?.connect()?;
    connection.query(fixture.sql())?;
    assert!(connection.is_auto_commit());
    assert_eq!(
        connection.query("SELECT sum(i) FROM imported")?.value(0, 0),
        Value::hugeint(3)
    );
    Ok(())
}

#[test]
fn an_implicit_import_bind_failure_rolls_back_created_objects() -> Result<()> {
    let fixture = Fixture::new("rollback", true);
    let mut connection = Database::open_in_memory()?.connect()?;
    let error = connection.query(fixture.sql()).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Catalog);
    assert!(connection.is_auto_commit());
    assert_eq!(
        connection
            .query("SELECT count(*) FROM duckdb_tables() WHERE table_name='imported'")?
            .value(0, 0),
        Value::bigint(0)
    );
    Ok(())
}

#[test]
fn an_active_import_bind_failure_aborts_its_transaction_but_ordinary_errors_do_not() -> Result<()> {
    let fixture = Fixture::new("active", true);
    let mut connection = Database::open_in_memory()?.connect()?;
    connection.query("CREATE TABLE stable(i INTEGER); INSERT INTO stable VALUES (1); BEGIN")?;
    assert_eq!(
        connection.query("FROM missing").unwrap_err().kind(),
        ErrorKind::Catalog
    );
    assert_eq!(connection.query("FROM stable")?.value(0, 0), Value::integer(1));
    connection.query("INSERT INTO stable VALUES (2)")?;
    assert_eq!(
        connection.query(fixture.sql()).unwrap_err().kind(),
        ErrorKind::Catalog
    );
    assert!(!connection.is_auto_commit());
    assert_eq!(
        connection.query("FROM stable").unwrap_err().kind(),
        ErrorKind::Transaction
    );
    connection.query("ROLLBACK")?;
    assert_eq!(
        connection.query("SELECT sum(i) FROM stable")?.value(0, 0),
        Value::hugeint(1)
    );
    assert_eq!(
        connection
            .query("SELECT count(*) FROM duckdb_tables() WHERE table_name='imported'")?
            .value(0, 0),
        Value::bigint(0)
    );
    Ok(())
}

#[test]
fn policy_config_snapshots_retain_metadata_without_replaying_transaction_state() -> Result<()> {
    let name = "current_transaction_invalidation_policy";
    let message =
        "current_transaction_invalidaton_policy can only be set when there is an active client context";
    let mut config = Config::default();
    assert_eq!(
        config
            .set_option_by_name(name, Value::varchar("STANDARD_POLICY"))
            .unwrap_err()
            .raw_message(),
        message
    );
    assert_eq!(
        Database::open(
            "",
            &Config::default().with_option(name, Value::varchar("STANDARD_POLICY"))
        )
        .unwrap_err()
        .raw_message(),
        message
    );
    let snapshot = {
        let database = Database::open_in_memory()?;
        let mut connection = database.connect()?;
        connection
            .query("SET current_transaction_invalidation_policy='ALL_ERRORS_INVALIDATE_TRANSACTION'")?;
        connection.configuration_snapshot()
    };
    let database = Database::open_from_snapshot("", &snapshot)?;
    let mut connection = database.connect()?;
    assert_eq!(
        connection.current_setting(name)?,
        Value::varchar("ALL_ERRORS_INVALIDATE_TRANSACTION")
    );
    connection.query("CREATE TABLE stable(i INTEGER); INSERT INTO stable VALUES (1); BEGIN")?;
    assert_eq!(
        connection.query("FROM missing").unwrap_err().kind(),
        ErrorKind::Catalog
    );
    assert_eq!(connection.query("FROM stable")?.value(0, 0), Value::integer(1));
    connection.query("SET current_transaction_invalidation_policy='ALL_ERRORS_INVALIDATE_TRANSACTION'; RESET current_transaction_invalidation_policy")?;
    assert_eq!(
        connection.query("FROM missing").unwrap_err().kind(),
        ErrorKind::Catalog
    );
    assert_eq!(connection.query("FROM stable")?.value(0, 0), Value::integer(1));
    connection.query("ROLLBACK")?;
    Ok(())
}

#[test]
fn failed_extracted_prepare_leaves_the_synthetic_transaction_for_its_query_owner() -> Result<()> {
    let fixture = Fixture::new("prepared-owner", true);
    let mut connection = Database::open_in_memory()?.connect()?;
    connection.query("CREATE TABLE stable(i INTEGER); INSERT INTO stable VALUES (1)")?;
    let mut statements = connection.parse_statements(&fixture.sql())?.into_iter();
    let parameters = rustdb_common::CaseInsensitiveMap::<Value>::new();
    for _ in 0..2 {
        let prepared = connection.prepare_statement(statements.next().unwrap())?;
        connection.execute_prepared(&prepared, &parameters, false)?;
    }
    assert_eq!(
        connection
            .prepare_statement(statements.next().unwrap())
            .unwrap_err()
            .kind(),
        ErrorKind::Catalog
    );
    assert!(!connection.is_auto_commit());
    assert_eq!(
        connection.query("FROM stable").unwrap_err().kind(),
        ErrorKind::Transaction
    );
    assert!(connection.is_auto_commit());
    assert_eq!(
        connection
            .query("SELECT count(*) FROM duckdb_tables() WHERE table_name='imported'")?
            .value(0, 0),
        Value::bigint(0)
    );
    Ok(())
}
