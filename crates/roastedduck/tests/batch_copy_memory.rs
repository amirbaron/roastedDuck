//! Ordered batched writers preserve rows while their raw batches use managed memory.
#![allow(
    clippy::unwrap_used,
    reason = "Synthetic SDK fixtures and expected error assertions"
)]
use roastedduck::{Database, Result, Value};
use std::path::PathBuf;
struct Fixture(PathBuf);
impl Fixture {
    fn new(label: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("roastedduck-batch-copy-{}-{label}", std::process::id()));
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
fn connection() -> Result<roastedduck::Connection> {
    let mut c = Database::open_in_memory()?.connect()?;
    c.query("SET threads=4; SET memory_limit='96MB'; SET enable_external_file_cache=false; SET preserve_insertion_order=true")?;
    Ok(c)
}
#[test]
fn ordered_parquet_copy_preserves_all_rows_under_memory_pressure() -> Result<()> {
    let f = Fixture::new("pressure");
    let mut c = connection()?;
    let input = f.path("input.parquet");
    let output = f.path("output.parquet");
    c.query(format!("COPY(SELECT i,repeat(md5(i::VARCHAR),4)payload FROM range(600000)t(i))TO '{input}'(FORMAT PARQUET,ROW_GROUP_SIZE 32768)"))?;
    c.query(format!(
        "COPY '{input}' TO '{output}'(FORMAT PARQUET,ROW_GROUP_SIZE 122880)"
    ))?;
    c.query("SET memory_limit='512MB'")?;
    assert_eq!(
        c.query(format!(
            "SELECT count(*)FROM(SELECT i,row_number()OVER()n FROM '{output}')WHERE i<>n-1"
        ))?
        .value(0, 0),
        Value::bigint(0)
    );
    assert_eq!(c.query(format!("SELECT count(*)FROM((FROM '{input}' EXCEPT ALL FROM '{output}')UNION ALL(FROM '{output}' EXCEPT ALL FROM '{input}'))"))?.value(0,0),Value::bigint(0));
    Ok(())
}
#[test]
fn uneven_batches_and_final_repartition_keep_order() -> Result<()> {
    let f = Fixture::new("uneven");
    let mut c = connection()?;
    let mut files = Vec::new();
    for (part, size) in [999, 5_000, 32_768, 99_979].into_iter().enumerate() {
        let path = f.path(&format!("{part}.parquet"));
        let start = part * 50_000;
        let end = start + 50_000;
        c.query(format!("COPY(SELECT i,i::VARCHAR s FROM range({start},{end})t(i))TO '{path}'(FORMAT PARQUET,ROW_GROUP_SIZE {size})"))?;
        files.push(format!("'{path}'"));
    }
    let output = f.path("combined.parquet");
    c.query(format!(
        "COPY(FROM read_parquet([{}]))TO '{output}'(FORMAT PARQUET,ROW_GROUP_SIZE 5000)",
        files.join(",")
    ))?;
    assert_eq!(
        c.query(format!("SELECT count(*)FROM '{output}'"))?.value(0, 0),
        Value::bigint(200_000)
    );
    assert_eq!(
        c.query(format!(
            "SELECT count(*)FROM(SELECT i,s,row_number()OVER()n FROM '{output}')WHERE i<>n-1 OR s<>i::VARCHAR"
        ))?
        .value(0, 0),
        Value::bigint(0)
    );
    Ok(())
}
#[test]
fn ordered_csv_batch_scan_preserves_nullable_unicode_values() -> Result<()> {
    let f = Fixture::new("csv");
    let mut c = connection()?;
    let path = f.path("output.csv");
    c.query("CREATE TABLE sample AS SELECT i,CASE WHEN i%3=0 THEN NULL ELSE '雪,\"שלום\"' END s FROM range(100000)t(i)")?;
    c.query(format!("COPY sample TO '{path}'(FORMAT CSV,HEADER true)"))?;
    c.query("SET memory_limit='512MB'")?;
    assert_eq!(c.query(format!("SELECT count(*)FROM((FROM sample EXCEPT ALL SELECT * FROM read_csv('{path}',header=true))UNION ALL(SELECT * FROM read_csv('{path}',header=true) EXCEPT ALL FROM sample))"))?.value(0,0),Value::bigint(0));
    assert_eq!(c.query(format!("SELECT count(*)FROM(SELECT i,row_number()OVER()n FROM read_csv('{path}',header=true))WHERE i<>n-1"))?.value(0,0),Value::bigint(0));
    Ok(())
}
#[test]
fn failing_parallel_copy_cleans_output_and_connection_recovers() -> Result<()> {
    let f = Fixture::new("failure");
    let mut c = connection()?;
    let path = f.path("failed.parquet");
    let error=c.query(format!("COPY(SELECT i,CASE WHEN i=90000 THEN error('injected batch copy failure') ELSE i::VARCHAR END s FROM range(100000)t(i))TO '{path}'(FORMAT PARQUET,ROW_GROUP_SIZE 2048)")).unwrap_err();
    assert!(error.raw_message().contains("injected batch copy failure"));
    assert!(!std::path::Path::new(&path).exists());
    assert_eq!(c.query("SELECT 42")?.value(0, 0), Value::integer(42));
    Ok(())
}
