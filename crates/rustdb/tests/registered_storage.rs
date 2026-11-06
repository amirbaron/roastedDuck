//! Registered backends must carry real database blocks and WAL, not data-file snapshots.
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use rustdb::{Database, Error, ErrorKind, Result, Value};
use rustdb_common::ByteString;
use rustdb_function::registered_files::{FileHandle, FileSystem, OpenFlags, Registry};

type Bytes = Arc<Mutex<Vec<u8>>>;
#[derive(Debug, Default)]
pub(crate) struct MemoryFs {
    files: Mutex<BTreeMap<Vec<u8>, Bytes>>,
    calls: AtomicUsize,
    open_error: Mutex<Option<Error>>,
}
#[derive(Debug)]
struct Handle(Bytes);
impl FileHandle for Handle {
    fn read_at(&self, offset: u64, buffer: &mut [u8]) -> Result<usize> {
        let data = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let start = usize::try_from(offset)
            .map_err(|_| Error::invalid_input("test offset too large"))?
            .min(data.len());
        let n = buffer.len().min(data.len() - start).min(4093);
        buffer[..n].copy_from_slice(&data[start..start + n]);
        Ok(n)
    }
    fn write_at(&self, offset: u64, buffer: &[u8]) -> Result<usize> {
        let mut data = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let start = usize::try_from(offset).map_err(|_| Error::invalid_input("test offset too large"))?;
        let n = buffer.len().min(4093);
        let size = data.len().max(start + n);
        data.resize(size, 0);
        data[start..start + n].copy_from_slice(&buffer[..n]);
        Ok(n)
    }
    fn size(&self) -> Result<u64> {
        Ok(self.0.lock().unwrap_or_else(PoisonError::into_inner).len() as u64)
    }
    fn modified(&self) -> Result<i64> {
        Ok(0)
    }
    fn flush(&self) -> Result<()> {
        Ok(())
    }
    fn truncate(&self, size: u64) -> Result<()> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).resize(
            usize::try_from(size).map_err(|_| Error::invalid_input("test size too large"))?,
            0,
        );
        Ok(())
    }
}
impl FileSystem for MemoryFs {
    fn name(&self) -> &'static str {
        "registered_storage_test"
    }
    fn handles(&self, path: &[u8]) -> bool {
        path.starts_with(b"s3://")
    }
    fn open(&self, path: &[u8], write: bool) -> Result<Arc<dyn FileHandle>> {
        self.open_with_flags(
            path,
            OpenFlags {
                read: !write,
                write,
                create: write,
                truncate: write,
            },
        )
    }
    fn open_with_flags(&self, path: &[u8], flags: OpenFlags) -> Result<Arc<dyn FileHandle>> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        if let Some(error) = self
            .open_error
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
        {
            return Err(error);
        }
        let mut files = self.files.lock().unwrap_or_else(PoisonError::into_inner);
        if !files.contains_key(path) && !flags.create {
            return Err(Error::new(ErrorKind::Io, "test backend: missing file"));
        }
        let data = Arc::clone(files.entry(path.to_vec()).or_default());
        if flags.truncate {
            data.lock().unwrap_or_else(PoisonError::into_inner).clear();
        }
        Ok(Arc::new(Handle(data)))
    }
    fn glob(&self, path: &[u8]) -> Result<Vec<ByteString>> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        if path.iter().any(|byte| matches!(byte, b'*' | b'?' | b'[')) {
            return Err(Error::not_implemented(
                "Wildcard patterns are outside this test backend",
            ));
        }
        Ok(
            if self
                .files
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .contains_key(path)
            {
                vec![path.into()]
            } else {
                Vec::new()
            },
        )
    }
    fn is_file(&self, path: &[u8]) -> Result<bool> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Ok(self
            .files
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains_key(path))
    }
    fn is_dir(&self, _: &[u8]) -> Result<bool> {
        Ok(false)
    }
    fn mkdir(&self, _: &[u8]) -> Result<()> {
        Ok(())
    }
    fn remove(&self, path: &[u8]) -> Result<()> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.files
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(path);
        Ok(())
    }
    fn rename(&self, source: &[u8], target: &[u8]) -> Result<()> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        let mut files = self.files.lock().unwrap_or_else(PoisonError::into_inner);
        let data = files
            .remove(source)
            .ok_or_else(|| Error::new(ErrorKind::Io, "test backend: missing source"))?;
        files.insert(target.to_vec(), data);
        Ok(())
    }
}
impl MemoryFs {
    fn fork(&self) -> Arc<Self> {
        Arc::new(Self {
            files: Mutex::new(
                self.files
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .iter()
                    .map(|(path, bytes)| {
                        (
                            path.clone(),
                            Arc::new(Mutex::new(
                                bytes.lock().unwrap_or_else(PoisonError::into_inner).clone(),
                            )),
                        )
                    })
                    .collect(),
            ),
            calls: AtomicUsize::new(0),
            open_error: Mutex::default(),
        })
    }
    pub(crate) fn bytes(&self, path: &[u8]) -> Vec<u8> {
        self.files.lock().unwrap_or_else(PoisonError::into_inner)[path]
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
    pub(crate) fn fail_open(&self, error: Option<Error>) {
        *self.open_error.lock().unwrap_or_else(PoisonError::into_inner) = error;
    }
}
pub(crate) fn register(connection: &rustdb::Connection, fs: &Arc<MemoryFs>) -> Result<Arc<Registry>> {
    let cache = connection
        .function_client()
        .object_cache()
        .ok_or_else(|| Error::internal("Missing test filesystem registry"))?;
    let registry = Registry::initialize(&cache);
    registry.register(Arc::clone(fs) as Arc<dyn FileSystem>)?;
    Ok(registry)
}

#[test]
fn import_reads_registered_schema_load_and_csv_files_with_native_names() -> Result<()> {
    let fs = Arc::new(MemoryFs::default());
    for (path, bytes) in [
        (
            b"s3://import/schema.sql".as_slice(),
            b"CREATE TABLE \"import\xff\"(i INTEGER);".as_slice(),
        ),
        (
            b"s3://import/load.sql".as_slice(),
            b"COPY \"import\xff\" FROM 'old/data.csv' (FORMAT CSV, HEADER false);".as_slice(),
        ),
        (b"s3://import/data.csv".as_slice(), b"1\n2\n".as_slice()),
    ] {
        fs.files
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(path.to_vec(), Arc::new(Mutex::new(bytes.to_vec())));
    }
    let mut connection = Database::open_in_memory()?.connect()?;
    register(&connection, &fs)?;
    connection.query("IMPORT DATABASE 's3://import'")?;
    assert_eq!(
        connection
            .query(b"SELECT sum(i) FROM \"import\xff\"")?
            .value(0, 0),
        Value::hugeint(3)
    );
    assert!(connection.is_auto_commit());
    Ok(())
}
fn fixture() -> Result<Arc<MemoryFs>> {
    let fs = Arc::new(MemoryFs::default());
    let database = Database::open_in_memory()?;
    let mut connection = database.connect()?;
    register(&connection, &fs)?;
    connection.query("ATTACH 's3://fixture.duckdb' AS remote (READ_WRITE)")?;
    connection.query("CREATE TABLE remote.t AS SELECT 1 i, 'snow' s; CHECKPOINT remote")?;
    connection.query("DETACH remote")?;
    assert_eq!(&fs.bytes(b"s3://fixture.duckdb")[8..12], b"DUCK");
    Ok(fs)
}
#[test]
fn remote_default_read_only_and_backend_retention() -> Result<()> {
    let fs = fixture()?;
    let original = fs.bytes(b"s3://fixture.duckdb");
    let weak = Arc::downgrade(&fs);
    let database = Database::open_in_memory()?;
    let mut connection = database.connect()?;
    let registry = register(&connection, &fs)?;
    connection.query("ATTACH 's3://fixture.duckdb' AS remote")?;
    assert_eq!(
        connection
            .query("SELECT readonly FROM duckdb_databases() WHERE database_name='remote'")?
            .value(0, 0),
        Value::boolean(true)
    );
    registry.remove(fs.name());
    drop(fs);
    assert!(weak.upgrade().is_some());
    assert_eq!(
        connection.query("SELECT i FROM remote.t")?.value(0, 0),
        Value::integer(1)
    );
    assert!(
        connection
            .query("INSERT INTO remote.t VALUES (2, 'forbidden')")
            .is_err()
    );
    assert_eq!(
        weak.upgrade()
            .expect("test fixture invariant")
            .bytes(b"s3://fixture.duckdb"),
        original
    );
    drop(connection);
    drop(database);
    assert!(weak.upgrade().is_none());
    Ok(())
}
#[test]
fn registered_checkpoint_and_nonempty_wal_replay() -> Result<()> {
    let fs = fixture()?;
    let database = Database::open_in_memory()?;
    let mut writer = database.connect()?;
    let registry = register(&writer, &fs)?;
    writer.query("ATTACH 's3://fixture.duckdb' AS remote (READ_WRITE)")?;
    // WAL creation occurs after registry removal, when no thread-local lookup can find it.
    registry.remove(fs.name());
    writer.query("INSERT INTO remote.t VALUES (2, 'wal')")?;
    assert_ne!(fs.bytes(b"s3://fixture.duckdb.wal"), [] as [u8; 0]);
    let recovered = fs.fork();
    let other = Database::open_in_memory()?;
    let mut reader = other.connect()?;
    register(&reader, &recovered)?;
    reader.query("ATTACH 's3://fixture.duckdb' AS remote (READ_ONLY)")?;
    assert_eq!(
        reader.query("SELECT sum(i) FROM remote.t")?.value(0, 0),
        Value::hugeint(3)
    );
    writer.query("CHECKPOINT remote")?;
    assert!(
        !fs.files
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains_key(b"s3://fixture.duckdb.wal".as_slice())
    );
    Ok(())
}
#[test]
fn torn_registered_wal_truncates_before_receiver_writes() -> Result<()> {
    let fs = fixture()?;
    let database = Database::open_in_memory()?;
    let mut writer = database.connect()?;
    register(&writer, &fs)?;
    writer.query(
        "ATTACH 's3://fixture.duckdb' AS remote (READ_WRITE); INSERT INTO remote.t VALUES (2, 'wal')",
    )?;
    let recovered = fs.fork();
    recovered.files.lock().unwrap_or_else(PoisonError::into_inner)[b"s3://fixture.duckdb.wal".as_slice()]
        .lock()
        .expect("test fixture invariant")
        .extend_from_slice(&[1, 2, 3]);
    let other = Database::open_in_memory()?;
    let mut receiver = other.connect()?;
    register(&receiver, &recovered)?;
    receiver.query("ATTACH 's3://fixture.duckdb' AS remote (READ_WRITE)")?;
    receiver.query("INSERT INTO remote.t VALUES (3, 'receiver')")?;
    let final_copy = recovered.fork();
    let last = Database::open_in_memory()?;
    let mut last_reader = last.connect()?;
    register(&last_reader, &final_copy)?;
    last_reader.query("ATTACH 's3://fixture.duckdb' AS remote (READ_ONLY)")?;
    assert_eq!(
        last_reader.query("SELECT sum(i) FROM remote.t")?.value(0, 0),
        Value::hugeint(6)
    );
    Ok(())
}
#[test]
fn disabled_registered_backend_is_rejected_before_callbacks() -> Result<()> {
    let fs = fixture()?;
    let database = Database::open_in_memory()?;
    let mut connection = database.connect()?;
    register(&connection, &fs)?;
    connection.query("SET disabled_filesystems='registered_storage_test'")?;
    let before = fs.calls.load(Ordering::Relaxed);
    assert_eq!(
        connection
            .query("ATTACH 's3://fixture.duckdb' AS remote (READ_ONLY)")
            .unwrap_err()
            .kind(),
        ErrorKind::Permission
    );
    assert_eq!(fs.calls.load(Ordering::Relaxed), before);
    Ok(())
}

#[test]
fn registered_open_errors_preserve_type_subtype_and_metadata() -> Result<()> {
    let fs = fixture()?;
    let database = Database::open_in_memory()?;
    let mut connection = database.connect()?;
    register(&connection, &fs)?;
    let expected = Error::new(ErrorKind::Invalid, "backend callback sentinel")
        .with_subtype("python_callback")
        .with_extra_info("backend", "test");
    fs.fail_open(Some(expected.clone()));
    let error = connection
        .query("ATTACH 's3://fixture.duckdb' AS remote (READ_ONLY)")
        .unwrap_err();
    assert_eq!(error, expected);
    Ok(())
}

#[test]
fn disabling_an_attached_backend_rejects_new_wal_before_callbacks() -> Result<()> {
    let fs = fixture()?;
    let database = Database::open_in_memory()?;
    let mut connection = database.connect()?;
    register(&connection, &fs)?;
    connection.query("ATTACH 's3://fixture.duckdb' AS remote (READ_WRITE)")?;
    connection.query("SET disabled_filesystems='registered_storage_test'")?;
    let before = fs.calls.load(Ordering::Relaxed);
    let error = connection
        .query("INSERT INTO remote.t VALUES (2, 'blocked')")
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Transaction);
    assert_eq!(
        error.raw_message(),
        "Failed to commit: File system registered_storage_test has been disabled by configuration"
    );
    assert_eq!(fs.calls.load(Ordering::Relaxed), before);
    assert_eq!(
        connection.query("SELECT sum(i) FROM remote.t")?.value(0, 0),
        Value::hugeint(1)
    );
    Ok(())
}

#[test]
fn disabling_external_access_does_not_revoke_attached_database_writes() -> Result<()> {
    let fs = fixture()?;
    let database = Database::open_in_memory()?;
    let mut connection = database.connect()?;
    register(&connection, &fs)?;
    connection.query("ATTACH 's3://fixture.duckdb' AS remote (READ_WRITE)")?;
    connection.query("SET enable_external_access=false")?;
    connection.query("INSERT INTO remote.t VALUES (2, 'existing database')")?;
    assert_eq!(
        connection.query("SELECT sum(i) FROM remote.t")?.value(0, 0),
        Value::hugeint(3)
    );
    assert_ne!(fs.bytes(b"s3://fixture.duckdb.wal"), [] as [u8; 0]);
    Ok(())
}

#[test]
fn registered_data_files_use_magic_detection_and_virtual_views() -> Result<()> {
    let fs = Arc::new(MemoryFs::default());
    let database = Database::open_in_memory()?;
    let mut connection = database.connect()?;
    register(&connection, &fs)?;
    for format in ["csv", "json", "parquet"] {
        connection.query(format!(
            "COPY(SELECT i, 'snow' s FROM range(3)t(i)) TO 's3://data.{format}' (FORMAT {format})"
        ))?;
        let error = connection
            .query(format!("ATTACH 's3://data.{format}' AS remote"))
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Catalog);
        assert_eq!(
            error.raw_message(),
            "Cannot launch in-memory database in read-only mode!"
        );
        connection.query(format!("ATTACH 's3://data.{format}' AS remote (READ_WRITE)"))?;
        for view in ["file", "remote"] {
            assert_eq!(
                connection
                    .query(format!("SELECT sum(i) FROM remote.{view}"))?
                    .value(0, 0),
                Value::hugeint(3)
            );
        }
        let metadata =
            connection.query("SELECT path,readonly FROM duckdb_databases() WHERE database_name='remote'")?;
        assert!(metadata.value(0, 0).is_null());
        assert_eq!(metadata.value(1, 0), Value::boolean(false));
        connection.query("DETACH remote")?;
    }
    Ok(())
}
