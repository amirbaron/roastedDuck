//! Actual EXPORT bound plans and execution preserve metadata and all table copies.
#![allow(clippy::unwrap_used, reason = "Synthetic fixtures and JSON-plan assertions")]
use rustdb::{Database, Result, Value};
use std::path::PathBuf;
use std::sync::Arc;

#[path = "registered_storage.rs"]
mod registered_backend;
struct Fixture(PathBuf);
impl Fixture {
    fn new(label: &str) -> Self {
        let p = std::env::temp_dir().join(format!("rustdb-export-plan-{}-{label}", std::process::id()));
        std::fs::create_dir(&p).unwrap();
        Self(p)
    }
    fn path(&self, name: &str) -> String {
        self.0.join(name).to_str().unwrap().replace('\'', "''")
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn literal(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

#[test]
fn registered_export_writes_all_files_and_imports_csv_and_parquet() -> Result<()> {
    for format in ["csv", "parquet"] {
        let fs = Arc::new(registered_backend::MemoryFs::default());
        let mut c = Database::open_in_memory()?.connect()?;
        registered_backend::register(&c, &fs)?;
        c.query("CREATE TABLE sample(i INTEGER PRIMARY KEY,s VARCHAR DEFAULT '雪');INSERT INTO sample VALUES(1,'雪'),(2,NULL)")?;
        let path = format!("s3://export-{format}");
        c.query(format!("EXPORT DATABASE '{path}' (FORMAT {format})"))?;
        assert_ne!(
            fs.bytes(format!("{path}/sample.{format}").as_bytes()),
            Vec::<u8>::new()
        );
        assert!(
            fs.bytes(format!("{path}/schema.sql").as_bytes())
                .starts_with(b"CREATE TABLE \"sample\"")
        );
        assert!(
            fs.bytes(format!("{path}/load.sql").as_bytes())
                .starts_with(b"COPY \"sample\" FROM ")
        );
        let mut other = Database::open_in_memory()?.connect()?;
        registered_backend::register(&other, &fs)?;
        other.query(format!("IMPORT DATABASE '{path}'"))?;
        let result = other.query("FROM sample ORDER BY i")?;
        assert_eq!(result.row_count(), 2);
        assert_eq!(result.value(0, 0), Value::integer(1));
        assert_eq!(result.value(1, 0), Value::varchar("雪"));
        assert_eq!(result.value(0, 1), Value::integer(2));
        assert!(result.value(1, 1).is_null());
        assert!(other.query("INSERT INTO sample(i)VALUES(1)").is_err());
        other.query("INSERT INTO sample(i)VALUES(3)")?;
        assert_eq!(
            other.query("SELECT s FROM sample WHERE i=3")?.value(0, 0),
            Value::varchar("雪")
        );
    }
    Ok(())
}

#[test]
fn registered_export_metadata_failure_preserves_error_and_connection_recovers() -> Result<()> {
    let fs = Arc::new(registered_backend::MemoryFs::default());
    let mut c = Database::open_in_memory()?.connect()?;
    registered_backend::register(&c, &fs)?;
    let expected = rustdb::Error::new(rustdb::ErrorKind::Permission, "metadata callback failed")
        .with_subtype("python_callback")
        .with_extra_info("backend", b"export\xff".to_vec());
    fs.fail_open(Some(expected.clone()));
    let actual = c.query("EXPORT DATABASE 's3://export-error'").unwrap_err();
    assert_eq!(actual.kind(), expected.kind());
    assert_eq!(actual.subtype(), expected.subtype());
    assert_eq!(actual.extra_info(), expected.extra_info());
    assert!(
        actual
            .raw_message_bytes()
            .windows(expected.raw_message_bytes().len())
            .any(|s| s == expected.raw_message_bytes())
    );
    fs.fail_open(None);
    c.query("EXPORT DATABASE 's3://export-recovered'")?;
    assert_ne!(fs.bytes(b"s3://export-recovered/schema.sql"), Vec::<u8>::new());
    assert_eq!(c.query("SELECT 42")?.value(0, 0), Value::integer(42));
    Ok(())
}
fn metadata(c: &mut rustdb::Connection, sql: &str, path: &str) -> Result<Value> {
    Ok(c.query(format!(
        "SELECT json_extract_string(json_serialize_plan({}),{})",
        literal(sql),
        literal(path)
    ))?
    .value(0, 0))
}
#[test]
fn empty_export_serializes_real_copy_info_and_empty_bound_data() -> Result<()> {
    let f = Fixture::new("empty");
    let mut c = Database::open_in_memory()?.connect()?;
    for format in ["csv", "parquet"] {
        let sql = format!("EXPORT DATABASE '{}' (FORMAT {format})", f.path(format));
        assert_eq!(metadata(&mut c, &sql, "$.error")?, Value::varchar("false"));
        assert_eq!(
            metadata(&mut c, &sql, "$.plans[0].type")?,
            Value::varchar("LOGICAL_EXPORT")
        );
        assert_eq!(
            metadata(&mut c, &sql, "$.plans[0].copy_info.catalog")?,
            Value::varchar("")
        );
        assert_eq!(
            metadata(&mut c, &sql, "$.plans[0].copy_info.schema")?,
            Value::varchar("main")
        );
        assert_eq!(
            metadata(&mut c, &sql, "$.plans[0].exported_tables.info_type")?,
            Value::varchar("BOUND_EXPORT_DATA")
        );
        assert_eq!(
            metadata(&mut c, &sql, "$.plans[0].exported_tables.data")?,
            Value::varchar("[]")
        );
        if format == "csv" {
            for (index, key) in ["quote", "delimiter", "header"].into_iter().enumerate() {
                assert_eq!(
                    metadata(
                        &mut c,
                        &sql,
                        &format!("$.plans[0].copy_info.options[{index}].key")
                    )?,
                    Value::varchar(key)
                );
            }
        }
    }
    // Literal orders from frozen C++ full plans, including bucket growth and
    // force_quote erasure. Runtime metadata and serialization share this map.
    for (options, keys) in [
        (
            "FORMAT parquet,COMPRESSION zstd,ROW_GROUP_SIZE 2048",
            &["ROW_GROUP_SIZE", "COMPRESSION"][..],
        ),
        (
            "FORMAT csv,DELIMITER '|',HEADER false,FORCE_QUOTE *",
            &["quote", "DELIMITER", "HEADER"][..],
        ),
        (
            "FORMAT csv,DELIMITER '|',HEADER false,QUOTE '\"',ESCAPE '\"',NULL 'NULL',FORCE_QUOTE *,DATEFORMAT '%Y-%m-%d',TIMESTAMPFORMAT '%Y-%m-%d %H:%M:%S',COMPRESSION 'gzip'",
            &[
                "DELIMITER",
                "ESCAPE",
                "HEADER",
                "QUOTE",
                "NULL",
                "DATEFORMAT",
                "TIMESTAMPFORMAT",
                "COMPRESSION",
            ][..],
        ),
    ] {
        let sql = format!("EXPORT DATABASE '{}' ({options})", f.path("option-order"));
        assert_eq!(metadata(&mut c, &sql, "$.error")?, Value::varchar("false"));
        for (index, key) in keys.iter().enumerate() {
            assert_eq!(
                metadata(
                    &mut c,
                    &sql,
                    &format!("$.plans[0].copy_info.options[{index}].key")
                )?,
                Value::varchar(*key)
            );
        }
    }
    Ok(())
}
#[test]
fn multi_table_export_has_one_logical_union_and_actual_bound_records() -> Result<()> {
    let f = Fixture::new("multi");
    let mut c = Database::open_in_memory()?.connect()?;
    c.query("CREATE SCHEMA sample;CREATE TABLE sample.parent(i INTEGER PRIMARY KEY,s VARCHAR DEFAULT '雪');CREATE TABLE sample.child(i INTEGER REFERENCES sample.parent(i),v DECIMAL(18,2));CREATE TABLE extra_a(i INTEGER);CREATE TABLE extra_b(i INTEGER)")?;
    for format in ["csv", "parquet"] {
        let sql = format!("EXPORT DATABASE '{}' (FORMAT {format})", f.path(format));
        assert_eq!(metadata(&mut c, &sql, "$.error")?, Value::varchar("false"));
        assert_eq!(
            metadata(&mut c, &sql, "$.plans[0].children[0].type")?,
            Value::varchar("LOGICAL_UNION")
        );
        let r=c.query(format!("SELECT json_array_length(j,'$.plans[0].children[0].children'),json_array_length(j,'$.plans[0].exported_tables.data')FROM(SELECT json_serialize_plan({})j)",literal(&sql)))?;
        assert_eq!(r.value(0, 0), Value::ubigint(4));
        assert_eq!(r.value(1, 0), Value::ubigint(4));
        let r=c.query(format!("SELECT json_extract_string(value,'$.table_data.database_name'),json_extract_string(value,'$.table_data.not_null_columns')FROM json_each(json_serialize_plan({}),'$.plans[0].exported_tables.data')WHERE json_extract_string(value,'$.table_data.table_name')='parent'",literal(&sql)))?;
        assert_eq!(r.value(0, 0), Value::varchar(""));
        assert_eq!(r.value(1, 0), Value::varchar("[\"i\"]"));
    }
    Ok(())
}
#[test]
fn nary_export_execution_and_import_keep_defaults_foreign_keys_and_generated_columns() -> Result<()> {
    let f = Fixture::new("execution");
    for format in ["csv", "parquet"] {
        let mut c = Database::open_in_memory()?.connect()?;
        c.query("CREATE TABLE parent(i INTEGER PRIMARY KEY,s VARCHAR DEFAULT '雪',g INTEGER GENERATED ALWAYS AS(i+1));CREATE TABLE child(i INTEGER REFERENCES parent(i));CREATE TABLE extra(i INTEGER);INSERT INTO parent(i)VALUES(1),(2);INSERT INTO child VALUES(1);INSERT INTO extra VALUES(7)")?;
        let path = f.path(format);
        c.query(format!("EXPORT DATABASE '{path}'(FORMAT {format})"))?;
        let mut other = Database::open_in_memory()?.connect()?;
        other.query(format!("IMPORT DATABASE '{path}'"))?;
        assert_eq!(
            other.query("SELECT sum(i),sum(g),min(s)FROM parent")?.value(0, 0),
            Value::hugeint(3)
        );
        assert_eq!(
            other.query("SELECT sum(g)FROM parent")?.value(0, 0),
            Value::hugeint(5)
        );
        assert_eq!(other.query("FROM extra")?.value(0, 0), Value::integer(7));
        assert!(other.query("INSERT INTO child VALUES(9)").is_err());
        other.query("INSERT INTO parent(i)VALUES(3)")?;
        assert_eq!(
            other.query("SELECT s FROM parent WHERE i=3")?.value(0, 0),
            Value::varchar("雪")
        );
    }
    Ok(())
}
