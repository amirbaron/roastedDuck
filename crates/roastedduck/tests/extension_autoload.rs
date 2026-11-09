//! Missing catalog entries must load their provider and retry without explicit LOAD.

use roastedduck::{Config, Connection, Database, Result, Value};

fn database(autoload: bool) -> Result<Database> {
    Database::open(
        "",
        &Config {
            load_extensions: false,
            options: vec![("autoload_known_extensions".into(), Value::boolean(autoload))],
            ..Config::default()
        },
    )
}

fn loaded(connection: &mut Connection, extension: &str) -> Result<Value> {
    Ok(connection
        .query(format!(
            "SELECT loaded FROM duckdb_extensions() WHERE extension_name='{extension}'"
        ))?
        .value(0, 0))
}

#[test]
fn missing_table_function_loads_its_provider() -> Result<()> {
    let mut connection = database(true)?.connect()?;
    assert_eq!(loaded(&mut connection, "tpch")?, Value::boolean(false));
    connection.query("CALL DBGEN(sf=0)")?;
    assert_eq!(loaded(&mut connection, "tpch")?, Value::boolean(true));
    assert_eq!(
        connection.query("SELECT count(*) FROM lineitem")?.value(0, 0),
        Value::bigint(0)
    );
    assert_eq!(loaded(&mut connection, "parquet")?, Value::boolean(false));
    Ok(())
}

#[test]
fn missing_pragma_loads_parquet() -> Result<()> {
    let mut connection = database(true)?.connect()?;
    connection.query("PRAGMA add_parquet_key('key', '0123456789012345')")?;
    assert_eq!(loaded(&mut connection, "parquet")?, Value::boolean(true));
    Ok(())
}

#[test]
fn copy_format_lookup_autoloads_before_returning_not_found() -> Result<()> {
    let root = std::env::temp_dir().join(format!("copy-autoload-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let path = root.join("output.parquet");
    let path_sql = path.to_str().unwrap().replace('\'', "''");
    let mut connection = database(true)?.connect()?;
    connection.query(format!("COPY (SELECT 42 i) TO '{path_sql}' (FORMAT PARQUET)"))?;
    assert_eq!(loaded(&mut connection, "parquet")?, Value::boolean(true));
    assert_eq!(
        connection
            .query(format!("SELECT i FROM read_parquet('{path_sql}')"))?
            .value(0, 0),
        Value::integer(42)
    );
    drop(connection);
    std::fs::remove_dir_all(root).unwrap();
    Ok(())
}

#[test]
fn disabled_autoload_preserves_the_error_then_can_be_enabled() -> Result<()> {
    let mut connection = database(false)?.connect()?;
    let error = connection.query("CALL dbgen(sf=0)").unwrap_err().to_string();
    assert!(error.contains("exists in the tpch extension"), "{error}");
    assert_eq!(loaded(&mut connection, "tpch")?, Value::boolean(false));
    connection.query("SET autoload_known_extensions=true")?;
    connection.query("CALL dbgen(sf=0)")?;
    assert_eq!(loaded(&mut connection, "tpch")?, Value::boolean(true));
    Ok(())
}

#[test]
fn replacement_scan_sees_new_extension_defaults_and_preserves_local_settings() -> Result<()> {
    let root = std::env::temp_dir().join(format!("read-autoload-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let path = root.join("input.parquet");
    let path_sql = path.to_str().unwrap().replace('\'', "''");
    {
        let mut writer = Database::open_in_memory()?.connect()?;
        writer.query(format!("COPY (SELECT 42 i) TO '{path_sql}' (FORMAT PARQUET)"))?;
    }
    let mut connection = database(true)?.connect()?;
    let result = connection.query(format!(
        "SELECT i,current_setting('binary_as_string') FROM '{path_sql}'"
    ))?;
    assert_eq!(result.value(0, 0), Value::integer(42));
    assert_eq!(result.value(1, 0), Value::boolean(false));
    connection.query("SET binary_as_string=true")?;
    assert_eq!(
        connection
            .query("SELECT current_setting('binary_as_string') FROM tpch_queries() LIMIT 1")?
            .value(0, 0),
        Value::boolean(true)
    );
    assert_eq!(
        connection
            .query("SELECT scope FROM duckdb_settings() WHERE name='binary_as_string'")?
            .value(0, 0),
        Value::varchar("LOCAL")
    );
    drop(connection);
    std::fs::remove_dir_all(root).unwrap();
    Ok(())
}

#[test]
fn wrong_scalar_call_reports_the_table_function_after_autoload() -> Result<()> {
    let mut connection = database(true)?.connect()?;
    let error = connection.query("SELECT dbgen(0)").unwrap_err().to_string();
    assert!(
        error.contains("is a table function but it was used as a scalar function"),
        "{error}"
    );
    // C++ resolves the table function while constructing this diagnostic.
    assert_eq!(loaded(&mut connection, "tpch")?, Value::boolean(true));
    Ok(())
}

#[test]
fn disabled_pragma_autoload_leaves_parquet_unloaded() -> Result<()> {
    let mut connection = database(false)?.connect()?;
    let error = connection
        .query("PRAGMA add_parquet_key('key', '0123456789012345')")
        .unwrap_err()
        .to_string();
    assert!(error.contains("exists in the parquet extension"), "{error}");
    assert_eq!(loaded(&mut connection, "parquet")?, Value::boolean(false));
    Ok(())
}

#[test]
fn extension_registration_survives_user_transaction_rollback() -> Result<()> {
    let mut connection = database(true)?.connect()?;
    connection.query("BEGIN; CALL dbgen(sf=0); ROLLBACK")?;
    assert_eq!(loaded(&mut connection, "tpch")?, Value::boolean(true));
    assert!(connection.query("SELECT * FROM lineitem").is_err());
    assert_eq!(
        connection
            .query("SELECT count(*) FROM tpch_queries()")?
            .value(0, 0),
        Value::bigint(22)
    );
    Ok(())
}

#[test]
fn registered_functions_are_visible_to_an_already_open_transaction() -> Result<()> {
    let database = database(true)?;
    let mut first = database.connect()?;
    let mut second = database.connect()?;
    second.query("BEGIN")?;
    first.query("CALL dbgen(sf=0)")?;
    assert_eq!(
        second.query("SELECT count(*) FROM tpch_queries()")?.value(0, 0),
        Value::bigint(22)
    );
    second.query("COMMIT")?;
    Ok(())
}
