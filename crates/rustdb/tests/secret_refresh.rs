//! Config-provider refresh metadata uses typed structs and keeps credentials private.

use rustdb::{Config, Database, Result, Value};

fn database(name: &str) -> Result<Database> {
    let directory = std::env::temp_dir().join(format!("secret-refresh-{}-{name}", std::process::id()));
    Database::open(
        "",
        &Config {
            options: vec![
                (
                    "secret_directory".into(),
                    Value::try_varchar(directory.as_os_str().as_encoded_bytes())?,
                ),
                ("allow_unredacted_secrets".into(), Value::boolean(true)),
            ],
            ..Config::default()
        },
    )
}

#[test]
fn replacement_metadata_is_accepted_and_redacted_by_default() -> Result<()> {
    let db = database("explicit")?;
    db.register_http_file_system()?;
    let mut con = db.connect()?;
    con.query("CREATE SECRET rotating(TYPE s3,KEY_ID 'stale',SECRET 'old-private',REFRESH_INFO MAP {'key_id':'fresh','secret':'replacement-private'})")?;
    let redacted = con
        .query("SELECT secret_string FROM duckdb_secrets()")?
        .value(0, 0)
        .to_varchar()?;
    assert!(redacted.contains("refresh_info=redacted"), "{redacted}");
    assert!(!redacted.contains("replacement-private") && !redacted.contains("old-private"));
    let visible = con
        .query("SELECT secret_string FROM duckdb_secrets(redact:=false)")?
        .value(0, 0)
        .to_varchar()?;
    assert!(visible.contains("replacement-private") && visible.contains("old-private"));
    Ok(())
}

#[test]
fn automatic_refresh_captures_normalized_creation_options() -> Result<()> {
    let db = database("automatic")?;
    db.register_http_file_system()?;
    let mut con = db.connect()?;
    con.query("CREATE SECRET rotating(TYPE s3,KEY_ID 'stale',SECRET 'old-private',REFRESH 'auto')")?;
    assert_eq!(
        con.query("SELECT current_setting('httpfs_enable_credential_refresh')")?
            .value(0, 0),
        Value::boolean(true)
    );
    let visible = con
        .query("SELECT secret_string FROM duckdb_secrets()")?
        .value(0, 0)
        .to_varchar()?;
    assert!(visible.contains("refresh=auto") && visible.contains("refresh_info=redacted"));
    Ok(())
}

#[test]
fn refresh_and_explicit_info_are_mutually_exclusive() -> Result<()> {
    let db = database("conflict")?;
    db.register_http_file_system()?;
    let mut con = db.connect()?;
    let error = con
        .query("CREATE SECRET rotating(TYPE s3,REFRESH 'auto',REFRESH_INFO MAP {'key_id':'fresh'})")
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("Can not set `refresh` and `refresh_info` at the same time"),
        "{error}"
    );
    assert_eq!(
        con.query("SELECT count(*) FROM duckdb_secrets()")?.value(0, 0),
        Value::bigint(0)
    );
    Ok(())
}
