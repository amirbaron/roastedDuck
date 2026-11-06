//! Persisted expression ARTs bind from native buffers, then replay key mutations.
#![allow(clippy::unwrap_used, reason = "Controlled persisted-index fixtures")]
use rustdb::{Config, Database, Result};
use std::path::PathBuf;
struct Fixture(PathBuf);
impl Fixture {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!("rustdb-art-expression-{}-{label}", std::process::id()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn db(&self) -> String {
        self.0.join("data.db").to_str().unwrap().to_owned()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn create(path: &str, wal_index: bool) -> Result<()> {
    let db = Database::open(path, &Config::default())?;
    let mut c = db.connect()?;
    c.query("SET checkpoint_threshold='1TB';CREATE TABLE data(id INTEGER PRIMARY KEY,label VARCHAR,v INTEGER);INSERT INTO data SELECT i,'padding',i+1 FROM range(2000000)t(i);CHECKPOINT")?;
    c.query("CREATE UNIQUE INDEX expr_idx ON data((abs(v)))")?;
    if !wal_index {
        c.query("CHECKPOINT")?;
    }
    Ok(())
}
#[test]
fn cold_expression_checkpoint_uses_persisted_tree_at_one_mb() -> Result<()> {
    let fixture = Fixture::new("cold");
    let path = fixture.db();
    create(&path, false)?;
    let db = Database::open(&path, &Config::default())?;
    let mut c = db.connect()?;
    c.query("SET memory_limit='1MB';SET temp_directory=NULL;CHECKPOINT")?;
    c.query("SET memory_limit='1GB'")?;
    assert!(c.query("INSERT INTO data VALUES(3000002,'dup',-2)").is_err());
    assert_eq!(
        c.query("SELECT count(*)FROM data")?.value(0, 0).as_i128(),
        Some(2_000_000)
    );
    Ok(())
}
fn replay(path: &str, wal_index: bool, close_unbound: bool) -> Result<()> {
    {
        let db = Database::open(path, &Config::default())?;
        let mut c = db.connect()?;
        c.query("SET checkpoint_threshold='1TB';CREATE TABLE data(id INTEGER PRIMARY KEY,label VARCHAR,v INTEGER);INSERT INTO data SELECT i,'padding',i+1 FROM range(10000)t(i);CHECKPOINT;PRAGMA disable_checkpoint_on_shutdown")?;
        c.query("CREATE UNIQUE INDEX expr_idx ON data((abs(v)))")?;
        if !wal_index {
            c.query("CHECKPOINT")?;
        }
        c.query("INSERT INTO data VALUES(100001,'new',-140000);DELETE FROM data WHERE id=4;UPDATE data SET v=-150000 WHERE id=5")?;
        c.query(
            "BEGIN;INSERT INTO data VALUES(100002,'rollback',-160000);DELETE FROM data WHERE id=8;ROLLBACK",
        )?;
    }
    assert!(std::fs::metadata(format!("{path}.wal")).unwrap().len() > 0);
    if close_unbound {
        for _ in 0..2 {
            // Opening and closing without a connection must not checkpoint a stale
            // expression tree or discard the only recoverable WAL mutations.
            drop(Database::open(path, &Config::default())?);
            assert!(std::fs::metadata(format!("{path}.wal")).unwrap().len() > 0);
        }
    }
    let db = Database::open(path, &Config::default())?;
    let mut c = db.connect()?;
    c.query("CHECKPOINT")?;
    for value in [140_000, 150_000, 9] {
        c.query("BEGIN")?;
        assert!(
            c.query(format!("INSERT INTO data VALUES(200002,'duplicate',{value})"))
                .is_err()
        );
        c.query("ROLLBACK")?;
    }
    c.query("BEGIN;INSERT INTO data VALUES(200002,'freed',5);INSERT INTO data VALUES(200003,'rolledback',160000);ROLLBACK")?;
    assert_eq!(
        c.query("SELECT count(*)FROM data")?.value(0, 0).as_i128(),
        Some(10_000)
    );
    assert_eq!(
        c.query("SELECT v FROM data WHERE id=5")?.value(0, 0).as_i128(),
        Some(-150_000)
    );
    Ok(())
}
#[test]
fn persisted_expression_tree_applies_later_wal_changes() -> Result<()> {
    let f = Fixture::new("disk-wal");
    replay(&f.db(), false, false)
}
#[test]
fn expression_create_index_wal_buffers_apply_later_changes() -> Result<()> {
    let f = Fixture::new("create-wal");
    replay(&f.db(), true, false)
}

#[test]
fn unbound_persisted_expression_survives_shutdown_and_reopen() -> Result<()> {
    let f = Fixture::new("disk-shutdown");
    replay(&f.db(), false, true)
}

#[test]
fn unbound_wal_expression_survives_shutdown_and_reopen() -> Result<()> {
    let f = Fixture::new("wal-shutdown");
    replay(&f.db(), true, true)
}

#[test]
fn plain_create_index_wal_restores_without_a_table_scan() -> Result<()> {
    let fixture = Fixture::new("plain-wal");
    let path = fixture.db();
    {
        let db = Database::open(&path, &Config::default())?;
        let mut c = db.connect()?;
        c.query("SET checkpoint_threshold='1TB';CREATE TABLE data(id INTEGER,v INTEGER);INSERT INTO data SELECT i,i+1 FROM range(100000)t(i);CHECKPOINT;PRAGMA disable_checkpoint_on_shutdown;CREATE UNIQUE INDEX plain_idx ON data(v)")?;
    }
    let db = Database::open_in_memory()?;
    let mut c = db.connect()?;
    c.query("SET memory_limit='1MB';SET temp_directory=NULL")?;
    c.query(format!("ATTACH '{}' AS persisted", path.replace('\'', "''")))?;
    c.query("SET memory_limit='1GB'")?;
    assert!(c.query("INSERT INTO persisted.data VALUES(200002,2)").is_err());
    assert_eq!(
        c.query("SELECT count(*)FROM persisted.data")?
            .value(0, 0)
            .as_i128(),
        Some(100_000)
    );
    Ok(())
}

#[test]
fn legacy_art_metadata_keeps_buffers_lazy() -> Result<()> {
    let fixture = Fixture::new("legacy");
    // DuckDB's v0.9.1 fixture: PK, UNIQUE, FK and explicit ART indexes.
    let mut bytes = vec![0; 1_847_296];
    rustdb_parquet::compression::decompress(
        rustdb_parquet::thrift::CompressionCodec::GZIP,
        include_bytes!("fixtures/index_0-9-1.db.gz"),
        &mut bytes,
        b"index_0-9-1.db.gz",
    )?;
    let path = fixture.db();
    std::fs::write(&path, bytes).unwrap();
    let db = Database::open_in_memory()?;
    let mut c = db.connect()?;
    c.query(format!(
        "ATTACH '{}' AS legacy (READ_ONLY)",
        path.replace('\'', "''")
    ))?;
    assert_eq!(
        c.query("SELECT memory_usage_bytes FROM duckdb_memory() WHERE tag='ART_INDEX'")?
            .value(0, 0)
            .as_i128(),
        Some(0)
    );
    assert_eq!(
        c.query("SELECT id FROM legacy.pk_tbl WHERE id=2")?
            .value(0, 0)
            .as_i128(),
        Some(2)
    );
    assert_eq!(
        c.query("SELECT i FROM legacy.idx_tbl WHERE i=11")?
            .value(0, 0)
            .as_i128(),
        Some(11)
    );
    assert!(
        c.query("SELECT memory_usage_bytes FROM duckdb_memory() WHERE tag='ART_INDEX'")?
            .value(0, 0)
            .as_i128()
            .unwrap()
            > 0
    );
    Ok(())
}

#[test]
fn transactional_native_index_wal_accepts_keys_already_in_index_buffers() -> Result<()> {
    let fixture = Fixture::new("transactional-index");
    let path = fixture.db();
    {
        let db = Database::open(&path, &Config::default())?;
        let mut c = db.connect()?;
        c.query("SET checkpoint_threshold='1TB';PRAGMA disable_checkpoint_on_shutdown")?;
        for (name, expression) in [("plain", "v"), ("expr", "abs(v)")] {
            c.query(format!("CREATE TABLE {name}(v INTEGER);INSERT INTO {name} SELECT i+42 FROM range(100)t(i);BEGIN;INSERT INTO {name} VALUES(0),(1);CREATE UNIQUE INDEX {name}_idx ON {name}(({expression}));DELETE FROM {name} WHERE v=1;COMMIT"))?;
        }
    }
    let db = Database::open(&path, &Config::default())?;
    let mut c = db.connect()?;
    c.query("CHECKPOINT")?;
    for name in ["plain", "expr"] {
        assert!(c.query(format!("INSERT INTO {name} VALUES(0)")).is_err());
        c.query(format!("INSERT INTO {name} VALUES(1)"))?;
        assert_eq!(
            c.query(format!("SELECT count(*) FROM {name}"))?
                .value(0, 0)
                .as_i128(),
            Some(102)
        );
    }
    Ok(())
}

#[test]
fn primary_key_alter_wal_restores_native_tree_at_one_mb() -> Result<()> {
    let fixture = Fixture::new("primary-alter");
    let path = fixture.db();
    {
        let db = Database::open(&path, &Config::default())?;
        let mut c = db.connect()?;
        c.query("SET checkpoint_threshold='1TB';CREATE TABLE data(id INTEGER,label VARCHAR,v INTEGER);INSERT INTO data SELECT i,'pad',i+1 FROM range(100000)t(i);CHECKPOINT;PRAGMA disable_checkpoint_on_shutdown;ALTER TABLE data ADD PRIMARY KEY(v)")?;
    }
    {
        let db = Database::open_in_memory()?;
        let mut c = db.connect()?;
        c.query("SET memory_limit='1MB';SET temp_directory=NULL")?;
        c.query(format!("ATTACH '{}' AS disk", path.replace('\'', "''")))?;
        c.query("SET memory_limit='1GB'")?;
        assert!(
            c.query("INSERT INTO disk.data VALUES(200001,'duplicate',2)")
                .is_err()
        );
        assert!(
            c.query("INSERT INTO disk.data VALUES(200001,'null',NULL)")
                .is_err()
        );
        c.query("INSERT INTO disk.data VALUES(200001,'new',200000);CHECKPOINT")?;
    }
    let db = Database::open_in_memory()?;
    let mut c = db.connect()?;
    c.query("SET memory_limit='1MB';SET temp_directory=NULL")?;
    c.query(format!("ATTACH '{}' AS disk", path.replace('\'', "''")))?;
    c.query("SET memory_limit='1GB'")?;
    assert_eq!(
        c.query("SELECT count(*) FROM disk.data")?.value(0, 0).as_i128(),
        Some(100_001)
    );
    assert!(
        c.query("INSERT INTO disk.data VALUES(200002,'duplicate',200000)")
            .is_err()
    );
    Ok(())
}
