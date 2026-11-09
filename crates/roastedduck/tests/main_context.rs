//! The startup client supplies real filesystem context; runtime setters are not startup options.
use roastedduck::{Config, Database, ErrorKind, Result, Value};
use roastedduck_types::LogicalType;

fn path(label: &str, extension: &str) -> std::path::PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "roastedduck-main-context-{label}-{}-{nonce}.{extension}",
        std::process::id()
    ))
}
fn sql_path(path: &std::path::Path) -> roastedduck_common::ByteString {
    roastedduck_ast::keyword::sql_string_bytes(path.as_os_str().as_encoded_bytes())
}
#[test]
fn main_database_retains_live_filesystem_policy() -> Result<()> {
    let path = path("policy", "duckdb");
    {
        let database = Database::open(path.as_os_str().as_encoded_bytes(), &Config::default())?;
        let mut connection = database.connect()?;
        connection.query("CREATE TABLE t(i INTEGER); INSERT INTO t VALUES (1); CHECKPOINT")?;
        connection.query("SET disabled_filesystems='LocalFileSystem'")?;
        assert_eq!(
            connection.current_setting("disabled_filesystems")?,
            Value::varchar("")
        );
        let error = connection.query("INSERT INTO t VALUES (2)").unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Transaction);
        assert_eq!(
            error.raw_message(),
            "Failed to commit: File system LocalFileSystem has been disabled by configuration"
        );
        assert_eq!(
            connection.query("SELECT sum(i) FROM t")?.value(0, 0),
            Value::hugeint(1)
        );
    }
    std::fs::remove_file(path).unwrap();
    Ok(())
}
#[test]
fn instance_dependent_setters_reject_fresh_configuration_assignments() {
    for name in [
        "disabled_filesystems",
        "enable_logging",
        "logging_mode",
        "logging_level",
        "logging_storage",
        "enabled_log_types",
        "disabled_log_types",
        "allowed_paths",
        "allowed_directories",
    ] {
        let value = match name {
            "enable_logging" => Value::boolean(false),
            "allowed_paths" | "allowed_directories" => Value::list(LogicalType::VARCHAR, Vec::new()),
            _ => Value::varchar(""),
        };
        let expected = format!("Cannot change/set {name} before the database is started");
        let mut config = Config::default();
        let error = config
            .set_option_by_name(&name.to_ascii_uppercase(), value.clone())
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidInput);
        assert_eq!(error.raw_message(), expected);
        let config = Config::default().with_option(name, value);
        let error = Database::open("", &config).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidInput);
        assert_eq!(error.raw_message(), expected);
    }
}

#[test]
fn configuration_snapshot_retains_allowlists_but_resets_instance_managers() -> Result<()> {
    let csv = path("allowlist", "csv");
    std::fs::write(&csv, "i\n1\n2\n").unwrap();
    let quoted_csv = sql_path(&csv);
    let (config, session) = {
        let database = Database::open_in_memory()?;
        let mut connection = database.connect()?;
        connection.query(roastedduck_common::byte_format!("SET allowed_paths=[{quoted_csv}]; SET logging_level='DEBUG'; SET enable_logging=true; SET disabled_filesystems='LocalFileSystem'"))?;
        assert!(
            connection
                .query(roastedduck_common::byte_format!("FROM read_csv({quoted_csv})"))
                .is_err()
        );
        (connection.configuration_snapshot(), connection.session_snapshot())
    };
    let database = Database::open_from_snapshot("", &config)?;
    let mut connection = database.connect()?;
    connection.restore_session(session)?;
    assert_eq!(connection.current_setting("enable_logging")?, Value::integer(0));
    assert_eq!(
        connection.current_setting("logging_level")?,
        Value::varchar("INFO")
    );
    assert_eq!(
        connection.current_setting("disabled_filesystems")?,
        Value::varchar("")
    );
    connection.query("SET enable_external_access=false")?;
    assert_eq!(
        connection
            .query(roastedduck_common::byte_format!(
                "SELECT sum(i) FROM read_csv({quoted_csv})"
            ))?
            .value(0, 0),
        Value::hugeint(3)
    );
    std::fs::remove_file(csv).unwrap();
    Ok(())
}
#[test]
fn disabled_external_access_still_allows_explicit_main_files() -> Result<()> {
    let existing = path("existing", "duckdb");
    {
        let db = Database::open(existing.as_os_str().as_encoded_bytes(), &Config::default())?;
        db.connect()?.query("CREATE TABLE t AS SELECT 1 i; CHECKPOINT")?;
    }
    let config = Config::default().with_option("enable_external_access", Value::boolean(false));
    for file in [&existing, &path("new", "duckdb")] {
        {
            let db = Database::open(file.as_os_str().as_encoded_bytes(), &config)?;
            let mut c = db.connect()?;
            c.query("CREATE TABLE IF NOT EXISTS t AS SELECT 1 i")?;
            c.query("INSERT INTO t VALUES (2)")?;
            assert_eq!(c.query("SELECT sum(i) FROM t")?.value(0, 0), Value::hugeint(3));
        }
        std::fs::remove_file(file).unwrap();
    }
    let csv = path("main", "csv");
    std::fs::write(&csv, "i\n1\n2\n").unwrap();
    {
        let db = Database::open(csv.as_os_str().as_encoded_bytes(), &config)?;
        assert_eq!(
            db.connect()?.query("SELECT sum(i) FROM file")?.value(0, 0),
            Value::hugeint(3)
        );
    }
    std::fs::remove_file(csv).unwrap();
    Ok(())
}
