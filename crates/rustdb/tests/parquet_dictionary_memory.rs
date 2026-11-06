//! Parquet reusable dictionaries remain compressed through accounted COPY batches.
#![allow(
    clippy::unwrap_used,
    reason = "Controlled low-memory fixtures and metadata assertions"
)]
use rustdb::{Database, Result};
use rustdb_parquet::reader::{ParquetReader, ScanColumn};
use rustdb_parquet::schema::ParquetOptions;
use rustdb_types::{SelectionVector, VectorKind};
use std::path::PathBuf;
use std::sync::Arc;
struct Fixture(PathBuf);
impl Fixture {
    fn new(name: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("rustdb-dictionary-memory-{}-{name}", std::process::id()));
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
fn roundtrip(rows: usize, limit: &str) -> Result<()> {
    let fixture = Fixture::new(limit);
    let mut c = Database::open_in_memory()?.connect()?;
    c.query(format!("SET memory_limit='{limit}'; SET temp_directory=NULL"))?;
    let input = fixture.path("input.parquet");
    let output = fixture.path("output.parquet");
    c.query(format!(
        "COPY(SELECT repeat('a',50000)s FROM range({rows}))TO '{input}'"
    ))?;
    c.query(format!("COPY(FROM '{input}')TO '{output}'"))?;
    let result = c.query(format!(
        "SELECT count(*),sum(length(s)),min(s)=repeat('a',50000),max(s)=repeat('a',50000)FROM '{output}'"
    ))?;
    assert_eq!(result.value(0, 0).as_i128(), Some(rows as i128));
    assert_eq!(result.value(1, 0).as_i128(), Some((rows * 50000) as i128));
    assert_eq!(result.value(2, 0).as_bool(), Some(true));
    assert_eq!(result.value(3, 0).as_bool(), Some(true));

    Ok(())
}
#[test]
fn constant_dictionary_copy_fits_ten_mb_without_spill() -> Result<()> {
    roundtrip(1000, "10MB")
}
#[test]
fn constant_dictionary_copy_fits_hundred_mb_without_spill() -> Result<()> {
    roundtrip(100_000, "100MB")
}
#[test]
fn dictionary_identity_null_slot_and_slices_survive_reader_release() -> Result<()> {
    let fixture = Fixture::new("identity");
    let db = Database::open_in_memory()?;
    let mut c = db.connect()?;
    let input = fixture.path("input.parquet");
    c.query(format!("COPY(SELECT CASE WHEN i%3=0 THEN NULL WHEN i%2=0 THEN repeat('a',50000)ELSE repeat('b',50000)END s FROM range(6000)t(i))TO '{input}'"))?;
    let reader = ParquetReader::open(input.as_bytes(), ParquetOptions::default())?;
    let mut scan = reader.initialize_scan(vec![0], &[ScanColumn::column(0)], Vec::new())?;
    let mut first = Vec::new();
    let mut second = Vec::new();
    assert_eq!(scan.scan(&mut first)?, 2048);
    assert_eq!(scan.scan(&mut second)?, 2048);
    assert_eq!(first[0].kind(), VectorKind::Dictionary);
    let (_, _, one) = first[0]
        .storage_dictionary_parts()
        .expect("reusable dictionary metadata");
    let (_, _, two) = second[0]
        .storage_dictionary_parts()
        .expect("shared dictionary metadata");
    assert_eq!(one.size(), 3);
    assert!(Arc::ptr_eq(one, two));
    let sliced = first[0].slice(&SelectionVector::new(vec![1, 2, 3]));
    assert!(Arc::ptr_eq(one, sliced.storage_dictionary_parts().unwrap().2));
    drop(scan);
    drop(reader);
    drop(c);
    assert_eq!(sliced.get_value(0).as_str().unwrap(), "b".repeat(50000));
    assert_eq!(sliced.get_value(1).as_str().unwrap(), "a".repeat(50000));
    assert!(sliced.get_value(2).is_null());
    Ok(())
}
#[test]
fn changing_nullable_rowgroup_dictionaries_keep_values_and_hashes() -> Result<()> {
    let fixture = Fixture::new("rowgroups");
    let mut c = Database::open_in_memory()?.connect()?;
    let input = fixture.path("input.parquet");
    let output = fixture.path("output.parquet");
    c.query(format!("COPY(SELECT i,CASE WHEN i%7=0 THEN NULL ELSE repeat(chr((65+i//2048)::INTEGER),50000)END s FROM range(10000)t(i))TO '{input}'(ROW_GROUP_SIZE 2048)"))?;
    // This is a value/hash freshness check. The separate original regressions
    // retain their 10/100MB limits; C++ also rejects this repartition at 20MB.
    c.query(format!("COPY(FROM '{input}')TO '{output}'(ROW_GROUP_SIZE 4096)"))?;
    let summary = |c: &mut rustdb::Connection, path: &str| -> Result<_> {
        c.query(format!(
            "SELECT count(*),count(s),sum(length(s)),sum(hash(s)::HUGEINT)FROM '{path}'"
        ))
    };
    let before = summary(&mut c, &input)?;
    let after = summary(&mut c, &output)?;
    for column in 0..4 {
        assert_eq!(before.value(column, 0), after.value(column, 0));
    }
    assert_eq!(after.value(0, 0).as_i128(), Some(10000));
    Ok(())
}
