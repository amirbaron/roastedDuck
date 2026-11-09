//! Startup secret isolation and the progress-time setting's client-visible side effects.
use roastedduck::{Config, Database, ErrorKind, Result, Value};

#[test]
fn parquet_extension_options_are_session_scoped_typed_and_registered_on_load() -> Result<()> {
    let database = Database::open_in_memory()?;
    let mut first = database.connect()?;
    let second = database.connect()?;
    first.query("SET binary_as_string=true")?;
    assert_eq!(first.current_setting("binary_as_string")?, Value::boolean(true));
    assert_eq!(second.current_setting("binary_as_string")?, Value::boolean(false));
    assert_eq!(
        first
            .query("SELECT scope FROM duckdb_settings() WHERE name='binary_as_string'")?
            .value(0, 0),
        Value::varchar("LOCAL")
    );
    let error = first
        .query("SET binary_as_string='not-bool'")
        .expect_err("a failed cast must preserve the current session value");
    assert_eq!(error.kind(), ErrorKind::InvalidInput);
    assert_eq!(first.current_setting("binary_as_string")?, Value::boolean(true));
    first.query("SET binary_as_string=NULL")?;
    assert!(first.current_setting("binary_as_string")?.is_null());
    assert!(
        first
            .query("SELECT value FROM duckdb_settings() WHERE name='binary_as_string'")?
            .value(0, 0)
            .is_null()
    );
    let mut config = Config::default();
    config.set_option_by_name("binary_as_string", Value::boolean(true))?;
    let connection = Database::open("", &config)?.connect()?;
    assert_eq!(
        connection.current_setting("binary_as_string")?,
        Value::boolean(true)
    );
    let config = Config {
        load_extensions: false,
        ..Config::default()
    };
    let database = Database::open("", &config)?;
    let mut connection = database.connect()?;
    assert!(connection.current_setting("binary_as_string").is_err());
    connection.query("LOAD parquet")?;
    assert_eq!(
        connection.current_setting("binary_as_string")?,
        Value::boolean(false)
    );
    Ok(())
}

#[test]
fn startup_extension_options_respect_autoload_validate_and_keep_the_last_value() -> Result<()> {
    for (name, value) in [("TimeZone", "Asia/Jerusalem"), ("Calendar", "buddhist")] {
        let mut disabled = Config::default();
        disabled.set_option_by_name(name, Value::varchar(value))?;
        disabled.set_option_by_name("autoload_known_extensions", Value::boolean(false))?;
        let error = Database::open("", &disabled).expect_err("disabled autoload must reject the option");
        assert_eq!(error.kind(), ErrorKind::InvalidInput);
        assert_eq!(
            error.raw_message(),
            format!("The following options were not recognized: {name}")
        );
    }
    for (name, invalid, valid) in [
        ("TimeZone", "No/Such_Zone", "Asia/Jerusalem"),
        ("Calendar", "no_such_calendar", "buddhist"),
    ] {
        let mut config = Config::default();
        config.set_option_by_name(name, Value::varchar(invalid))?;
        let error = Database::open("", &config).expect_err("the extension callback must validate the value");
        assert_eq!(error.kind(), ErrorKind::NotImplemented);
        config.set_option_by_name(&name.to_ascii_lowercase(), Value::varchar(valid))?;
        let connection = Database::open("", &config)?.connect()?;
        assert_eq!(connection.current_setting(name)?, Value::varchar(valid));
    }
    let path = std::env::temp_dir().join(format!(
        "roastedduck-invalid-startup-setting-{}.duckdb",
        std::process::id()
    ));
    assert!(!path.exists());
    let mut config = Config::default();
    config.set_option_by_name("Calendar", Value::varchar("no_such_calendar"))?;
    let error = Database::open(path.to_string_lossy().as_ref(), &config)
        .expect_err("validation precedes opening or creating persistent storage");
    assert_eq!(error.kind(), ErrorKind::NotImplemented);
    assert!(!path.exists());
    Ok(())
}

#[test]
fn configured_extension_options_load_early_and_disabled_defaults_allow_explicit_load() -> Result<()> {
    let mut config = Config::default();
    config.set_option_by_name("TimeZone", Value::varchar("Asia/Jerusalem"))?;
    let mut con = Database::open("", &config)?.connect()?;
    assert_eq!(
        con.query("SELECT current_setting('TimeZone')")?.value(0, 0),
        Value::varchar("Asia/Jerusalem")
    );
    assert_eq!(
        con.query("SELECT json_object('x',7)->>'x'")?.value(0, 0),
        Value::varchar("7")
    );

    let config = Config {
        load_extensions: false,
        ..Config::default()
    };
    let database = Database::open("", &config)?;
    let mut con = database.connect()?;
    assert_eq!(
        con.query(
            "SELECT count(*) FROM duckdb_extensions() WHERE loaded AND extension_name IN ('json','icu')"
        )?
        .value(0, 0),
        Value::bigint(0)
    );
    con.query("LOAD json; LOAD icu")?;
    assert_eq!(
        con.query("SELECT json_object('x',7)->>'x'")?.value(0, 0),
        Value::varchar("7")
    );
    // Test the newly registered setting independently of the host's default zone.
    con.query("SET TimeZone='UTC'")?;
    assert_eq!(
        con.query("SELECT current_setting('TimeZone')")?.value(0, 0),
        Value::varchar("UTC")
    );
    Ok(())
}

#[test]
fn startup_config_rejects_custom_local_settings_but_accepts_generic_defaults() -> Result<()> {
    for (name, value) in [
        ("custom_profiling_settings", "{}"),
        ("debug_force_external", "true"),
        ("enable_caching_operators", "true"),
        ("enable_http_logging", "false"),
        ("enable_profiling", "json"),
        ("enable_progress_bar", "true"),
        ("enable_progress_bar_print", "false"),
        ("http_logging_output", ""),
        ("profile_output", ""),
        ("profiling_coverage", "ALL"),
        ("profiling_mode", "standard"),
        ("progress_bar_time", "1"),
        ("schema", "main"),
        ("search_path", "main"),
        ("streaming_buffer_size", "1MB"),
        ("profiling_output", ""),
        ("SCHEMA", "main"),
    ] {
        let error = Config::default()
            .set_option_by_name(name, Value::varchar(value))
            .expect_err("a custom local setting has no DBConfig global setter");
        assert_eq!(error.kind(), ErrorKind::InvalidInput, "{name}");
        assert!(
            error.to_string().contains("as a global option"),
            "{name}: {error}"
        );
    }
    let mut config = Config::default();
    config.set_option_by_name("default_order", Value::varchar("desc"))?;
    config.set_option_by_name("arrow_large_buffer_size", Value::boolean(true))?;
    let mut connection = Database::open("", &config)?.connect()?;
    assert_eq!(
        connection
            .query("SELECT current_setting('default_order')")?
            .value(0, 0),
        Value::varchar("DESC")
    );
    assert_eq!(
        connection
            .query("SELECT current_setting('arrow_large_buffer_size')")?
            .value(0, 0),
        Value::boolean(true)
    );
    Ok(())
}

#[test]
fn configured_access_mode_is_visible_in_current_setting_and_settings_table() -> Result<()> {
    for mode in ["automatic", "read_write"] {
        let mut config = Config::default();
        config.set_option_by_name("access_mode", Value::varchar(mode))?;
        let mut connection = Database::open("", &config)?.connect()?;
        let result = connection.query(
            "SELECT current_setting('access_mode'), value FROM duckdb_settings() WHERE name='access_mode'",
        )?;
        assert_eq!(result.value(0, 0), Value::varchar(mode));
        assert_eq!(result.value(1, 0), Value::varchar(mode));
    }
    Ok(())
}

#[test]
fn invalid_external_thread_count_fails_before_changing_the_setting() -> Result<()> {
    let mut connection = Database::open_in_memory()?.connect()?;
    for input in ["-1", "'NaN'", "18446744073709551616"] {
        let error = connection
            .query(format!("SET external_threads={input}"))
            .expect_err("SET must reject values that cannot be cast to UBIGINT");
        assert_eq!(error.kind(), ErrorKind::InvalidInput);
        assert!(error.to_string().contains("Failed to cast value:"));
        assert_eq!(
            connection
                .query("SELECT current_setting('external_threads')")?
                .value(0, 0),
            Value::ubigint(1)
        );
    }
    connection.query("SET external_threads=0")?;
    assert_eq!(
        connection
            .query("SELECT current_setting('external_threads')")?
            .value(0, 0),
        Value::ubigint(0)
    );
    Ok(())
}

#[test]
fn disabling_persistent_secrets_does_not_read_configured_files() -> Result<()> {
    let directory = std::env::temp_dir().join(format!("roastedduck-secret-settings-{}", std::process::id()));
    std::fs::create_dir_all(&directory).expect("create isolated secret fixture");
    std::fs::write(
        directory.join("bad.duckdb_secret"),
        b"malformed synthetic fixture",
    )
    .expect("write isolated secret fixture");
    let mut config = Config::default();
    config.set_option_by_name("secret_directory", Value::varchar(directory.to_string_lossy()))?;
    config.set_option_by_name("allow_persistent_secrets", Value::boolean(false))?;
    let mut connection = Database::open("", &config)?.connect()?;
    assert_eq!(
        connection
            .query("SELECT count(*) FROM duckdb_secrets()")?
            .value(0, 0),
        Value::bigint(0)
    );

    let mut enabled = Config::default();
    enabled.set_option_by_name("secret_directory", Value::varchar(directory.to_string_lossy()))?;
    let mut connection = Database::open("", &enabled)?.connect()?;
    let error = connection
        .query("SELECT count(*) FROM duckdb_secrets()")
        .expect_err("the enabled control must actually read the malformed fixture");
    assert_eq!(error.kind(), ErrorKind::Serialization);
    Ok(())
}

#[test]
fn progress_time_enables_tracking_and_reset_restores_default() -> Result<()> {
    let mut connection = Database::open_in_memory()?.connect()?;
    connection.query("SET enable_progress_bar=false")?;
    connection.query("SET progress_bar_time=0")?;
    assert_eq!(
        connection
            .query("SELECT current_setting('enable_progress_bar')")?
            .value(0, 0),
        Value::boolean(true)
    );
    connection.query("RESET progress_bar_time")?;
    assert_eq!(
        connection
            .query("SELECT current_setting('enable_progress_bar')")?
            .value(0, 0),
        Value::boolean(false)
    );
    Ok(())
}
