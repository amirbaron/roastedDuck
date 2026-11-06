//! Database-global and connection-local state have independent restart lifetimes.

use rustdb::{Config, Database, ErrorKind, Result, Value};

#[test]
fn null_temp_directory_exposes_the_disabled_path_not_sql_null() -> Result<()> {
    let mut connection = Database::open_in_memory()?.connect()?;
    connection.query("SET temp_directory=NULL")?;
    assert_eq!(connection.current_setting("temp_directory")?, Value::varchar(""));
    assert_eq!(
        connection
            .query("SELECT value FROM duckdb_settings() WHERE name='temp_directory'")?
            .value(0, 0),
        Value::varchar("")
    );
    Ok(())
}

#[test]
fn retained_extension_definitions_and_client_overrides_survive_independently() -> Result<()> {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "rustdb-restart-config-{}-{nonce}.duckdb",
        std::process::id()
    ));
    let config = Config {
        load_extensions: false,
        ..Config::default()
    };
    let (configuration, session) = {
        let database = Database::open(path.to_string_lossy().as_ref(), &config)?;
        let mut connection = database.connect()?;
        connection.query("LOAD parquet; SET GLOBAL binary_as_string=true; SET binary_as_string=false")?;
        connection.query(
            "CREATE SCHEMA saved; SET schema='saved'; CREATE TABLE t(i INTEGER); INSERT INTO t VALUES (7)",
        )?;
        (connection.configuration_snapshot(), connection.session_snapshot())
    };
    let database = Database::open_from_snapshot(path.to_string_lossy().as_ref(), &configuration)?;
    let mut connection = database.connect()?;
    assert_eq!(
        connection.current_setting("binary_as_string")?,
        Value::boolean(true)
    );
    assert_eq!(
        connection
            .query("SELECT count(*) FROM duckdb_extensions() WHERE extension_name='parquet' AND loaded")?
            .value(0, 0),
        Value::bigint(0)
    );
    connection.restore_session(session)?;
    assert_eq!(
        connection.current_setting("binary_as_string")?,
        Value::boolean(false)
    );
    assert_eq!(
        connection.query("SELECT i FROM t")?.value(0, 0),
        Value::integer(7)
    );
    connection.query("LOAD parquet")?;
    assert_eq!(
        connection.current_setting("binary_as_string")?,
        Value::boolean(false)
    );
    let other = database.connect()?;
    assert_eq!(other.current_setting("binary_as_string")?, Value::boolean(true));

    let mut changed = configuration.clone();
    changed
        .config_mut()
        .set_option_by_name("binary_as_string", Value::boolean(false))?;
    let changed_database = Database::open_from_snapshot("", &changed)?;
    assert_eq!(
        changed_database.connect()?.current_setting("binary_as_string")?,
        Value::boolean(false)
    );
    let mut fresh = Config::default();
    fresh.set_option_by_name("autoload_known_extensions", Value::boolean(false))?;
    fresh.set_option_by_name("binary_as_string", Value::boolean(true))?;
    assert_eq!(
        Database::open("", &fresh).unwrap_err().kind(),
        ErrorKind::InvalidInput
    );
    Ok(())
}

#[test]
fn global_reset_reaches_untouched_and_new_connections_and_configuration_snapshots() -> Result<()> {
    let mut config = Config::default();
    config.set_option_by_name("threads", Value::integer(2))?;
    let database = Database::open("", &config)?;
    let mut first = database.connect()?;
    let mut untouched = database.connect()?;
    first.query("RESET threads")?;
    let expected = first.query("SELECT current_setting('threads')")?.value(0, 0);
    assert_eq!(untouched.current_setting("threads")?, expected);
    assert_eq!(
        untouched.query("SELECT current_setting('threads')")?.value(0, 0),
        expected
    );
    assert_eq!(database.connect()?.current_setting("threads")?, expected);
    let snapshot = database.configuration_snapshot();
    let reopened = Database::open_from_snapshot("", &snapshot)?;
    assert_eq!(reopened.connect()?.current_setting("threads")?, expected);
    Ok(())
}

#[test]
fn registered_callback_lifetime_and_global_values_are_retained_without_revalidation() -> Result<()> {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let calls = Arc::new(AtomicUsize::new(0));
    let (configuration, session) = {
        let database = Database::open_in_memory()?;
        let observed = Arc::clone(&calls);
        assert!(database.add_extension_option(rustdb_function::ExtensionOption {
            name: "app_setting".to_owned(),
            description: "SDK restart callback".to_owned(),
            input_type: rustdb::LogicalType::INTEGER,
            default_value: Value::integer(3),
            set_function: Some(Arc::new(move |_| {
                observed.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })),
            global_by_default: false,
        }));
        let mut connection = database.connect()?;
        connection.query("SET GLOBAL app_setting=7; SET app_setting=8")?;
        (connection.configuration_snapshot(), connection.session_snapshot())
    };
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let database = Database::open_from_snapshot("", &configuration)?;
    let mut connection = database.connect()?;
    assert_eq!(connection.current_setting("app_setting")?, Value::integer(7));
    connection.restore_session(session)?;
    assert_eq!(connection.current_setting("app_setting")?, Value::integer(8));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    connection.query("SET app_setting=9")?;
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    Ok(())
}
