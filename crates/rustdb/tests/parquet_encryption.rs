//! Supported Parquet encrypted files retain all values and reject unauthenticated data.
#![allow(
    clippy::unwrap_used,
    reason = "Owned synthetic-key fixture setup and expected failures"
)]
use std::path::PathBuf;

use rustdb::{Database, Result, Value};

struct Fixture(PathBuf);
impl Fixture {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!("rustdb-parquet-crypto-{}-{label}", std::process::id()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
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

#[test]
fn encrypted_write_read_and_copy_from_preserve_nested_rows_for_all_key_sizes() -> Result<()> {
    let fixture = Fixture::new("nested");
    let mut connection = Database::open_in_memory()?.connect()?;
    connection.query("CREATE TABLE sample AS SELECT i, CASE WHEN i%3=0 THEN NULL ELSE '雪' || i::VARCHAR END s, [i,NULL,i+1] l, {'n':i,'label':'שלום'} st, (i/100)::DECIMAL(18,2) d FROM range(5000) t(i)")?;
    for size in [16, 24, 32] {
        let key = &"0123456789abcdef0123456789abcdef"[..size];
        connection.query(format!("PRAGMA add_parquet_key('synthetic', '{key}')"))?;
        for codec in ["snappy", "zstd", "gzip", "uncompressed"] {
            let path = fixture.path(&format!("{size}-{codec}.parquet"));
            connection.query(format!("COPY sample TO '{path}' (FORMAT PARQUET, COMPRESSION '{codec}', ROW_GROUP_SIZE 2048, ENCRYPTION_CONFIG {{footer_key:'synthetic'}})"))?;
            let bytes = std::fs::read(&path).unwrap();
            assert_eq!(&bytes[..4], b"PARE");
            assert_eq!(&bytes[bytes.len() - 4..], b"PARE");
            let scan = format!("read_parquet('{path}', encryption_config={{footer_key:'synthetic'}})");
            assert_eq!(connection.query(format!("SELECT count(*) FROM ((FROM sample EXCEPT ALL FROM {scan}) UNION ALL (FROM {scan} EXCEPT ALL FROM sample))"))?.value(0,0), Value::bigint(0));
            connection.query("CREATE OR REPLACE TABLE copied AS FROM sample LIMIT 0")?;
            connection.query(format!(
                "COPY copied FROM '{path}' (FORMAT PARQUET, ENCRYPTION_CONFIG {{footer_key:'synthetic'}})"
            ))?;
            assert_eq!(connection.query("SELECT count(*) FROM ((FROM sample EXCEPT ALL FROM copied) UNION ALL (FROM copied EXCEPT ALL FROM sample))")?.value(0,0), Value::bigint(0));
        }
    }
    Ok(())
}

#[test]
fn encrypted_pages_authenticate_keys_and_reject_truncated_files() -> Result<()> {
    let fixture = Fixture::new("authentication");
    let path = fixture.path("encrypted.parquet");
    let mut connection = Database::open_in_memory()?.connect()?;
    connection.query("PRAGMA add_parquet_key('right','0123456789abcdef'); PRAGMA add_parquet_key('wrong','fedcba9876543210')")?;
    connection.query(format!("COPY (SELECT i FROM range(5000) t(i)) TO '{path}' (FORMAT PARQUET, ENCRYPTION_CONFIG {{footer_key:'right'}})"))?;
    let error = connection
        .query(format!(
            "FROM read_parquet('{path}', encryption_config={{footer_key:'wrong'}})"
        ))
        .unwrap_err();
    assert!(
        error
            .raw_message()
            .contains("Computed AES tag differs from read AES tag")
    );
    assert!(
        connection
            .query(format!("FROM read_parquet('{path}')"))
            .unwrap_err()
            .raw_message()
            .contains("encryption_config")
    );
    let mut bytes = std::fs::read(&path).unwrap();
    bytes.truncate(bytes.len() - 12);
    let truncated = fixture.path("truncated.parquet");
    std::fs::write(&truncated, bytes).unwrap();
    assert!(
        connection
            .query(format!(
                "FROM read_parquet('{truncated}', encryption_config={{footer_key:'right'}})"
            ))
            .is_err()
    );
    Ok(())
}

#[test]
fn prepared_encrypted_copy_and_scan_keep_resolved_keys() -> Result<()> {
    let fixture = Fixture::new("prepared");
    let path = fixture.path("prepared.parquet");
    let mut connection = Database::open_in_memory()?.connect()?;
    connection.query("PRAGMA add_parquet_key('synthetic','0123456789abcdef')")?;
    let prepared = connection.prepare(format!(
        "COPY (SELECT 42 i) TO '{path}' (FORMAT PARQUET, ENCRYPTION_CONFIG {{footer_key:'synthetic'}})"
    ))?;
    let parameters = rustdb_common::CaseInsensitiveMap::<Value>::new();
    connection.execute_prepared(&prepared, &parameters, false)?;
    assert_eq!(
        connection
            .query(format!(
                "SELECT i FROM read_parquet('{path}', encryption_config={{footer_key:'synthetic'}})"
            ))?
            .value(0, 0),
        Value::integer(42)
    );
    let scan = format!("SELECT i FROM read_parquet('{path}', encryption_config={{footer_key:'synthetic'}})");
    connection.query(format!(
        "SELECT json_serialize_plan('{}')",
        scan.replace('\'', "''")
    ))?;
    // Column-level keys are deliberately unsupported by the frozen C++ contract too.
    let error = connection
        .query(format!(
            "COPY (SELECT 1 i) TO '{}' (FORMAT PARQUET, ENCRYPTION_CONFIG {{column_keys:{{i:'synthetic'}}}})",
            fixture.path("unsupported.parquet")
        ))
        .unwrap_err();
    assert!(error.raw_message().contains("column_keys not yet implemented"));
    Ok(())
}
