//! Real typed extension entries must bind to their implementations, including lazy ICU.
use rustdb::{Config, Database, Result, Value};

#[test]
fn typed_placeholders_are_replaced_by_real_core_and_icu_implementations() -> Result<()> {
    let mut connection = Database::open_in_memory()?.connect()?;
    let result = connection.query(
        "SELECT year(DATE '2024-04-05'), year(TIMESTAMPTZ '2024-04-05 00:00:00+00'),
         date_part(['year','month'], DATE '2024-04-05').month,
         len(range(TIMESTAMP '2024-01-01', TIMESTAMP '2024-01-04', INTERVAL '1 day')),
         len(generate_series(TIMESTAMPTZ '2024-01-01 00:00:00+00',
                             TIMESTAMPTZ '2024-01-04 00:00:00+00', INTERVAL '1 day'))",
    )?;
    for (index, value) in [2024, 2024, 4, 3, 4].into_iter().enumerate() {
        assert_eq!(result.value(index, 0), Value::bigint(value));
    }
    assert_eq!(
        connection
            .query("SELECT count(*) FROM duckdb_functions()")?
            .value(0, 0),
        Value::bigint(2953)
    );
    Ok(())
}

#[test]
fn an_icu_placeholder_loads_and_delegates_before_executing_its_kernel() -> Result<()> {
    let config = Config {
        load_extensions: false,
        ..Config::default()
    };
    let mut connection = Database::open("", &config)?.connect()?;
    assert_eq!(
        connection
            .query("SELECT loaded FROM duckdb_extensions() WHERE extension_name='icu'")?
            .value(0, 0),
        Value::boolean(false)
    );
    assert_eq!(
        connection
            .query("SELECT year(TIMESTAMPTZ '2024-04-05 00:00:00+00')")?
            .value(0, 0),
        Value::bigint(2024)
    );
    assert_eq!(
        connection
            .query("SELECT loaded FROM duckdb_extensions() WHERE extension_name='icu'")?
            .value(0, 0),
        Value::boolean(true)
    );
    connection.query("SET TimeZone='Asia/Jerusalem'")?;
    assert_eq!(
        connection
            .query("SELECT hour(TIMESTAMPTZ '2024-04-05 00:00:00+00')")?
            .value(0, 0),
        Value::bigint(3)
    );
    Ok(())
}

#[test]
fn independently_initialized_main_identities_match_the_frozen_cpp_observation() -> Result<()> {
    for _ in 0..3 {
        let mut connection = Database::open_in_memory()?.connect()?;
        assert_eq!(
            connection
                .query("SELECT database_oid FROM duckdb_databases() WHERE database_name='memory'")?
                .value(0, 0),
            Value::bigint(20624)
        );
    }
    Ok(())
}
