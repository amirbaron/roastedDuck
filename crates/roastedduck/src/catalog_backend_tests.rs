//! Exercise internal provider hooks through binding, optimization and execution.
use roastedduck_catalog::backend::{BackendScan, CatalogBackend, TableBackend};
use roastedduck_catalog::{CatalogEntry, CatalogType, EntryKind, Transaction};
use roastedduck_common::{Error, ErrorKind, Result};
use roastedduck_function::{FunctionClient, TableFunction};
use roastedduck_types::{DataChunk, LogicalType, Value, Vector};
use std::sync::Arc;

#[derive(Debug)]
struct Metadata {
    schema: Arc<CatalogEntry>,
    table: Arc<CatalogEntry>,
    fail_listing: Arc<std::sync::atomic::AtomicBool>,
}
impl CatalogBackend for Metadata {
    fn catalog_type(&self) -> &'static str {
        "test_remote"
    }
    fn default_schema(&self) -> &[u8] {
        b"public"
    }
    fn scan(
        &self,
        _: &Arc<FunctionClient>,
        _: &Transaction,
        kind: CatalogType,
        schema: &[u8],
    ) -> Result<Vec<Arc<CatalogEntry>>> {
        Ok(match kind {
            CatalogType::Schema => vec![self.schema.clone()],
            CatalogType::Table if schema == b"public" => vec![self.table.clone()],
            CatalogType::Sequence if self.fail_listing.load(std::sync::atomic::Ordering::Relaxed) => {
                return Err(Error::new(ErrorKind::Io, "provider enumeration failed"));
            }
            _ => vec![],
        })
    }
    fn lookup(
        &self,
        _: &Arc<FunctionClient>,
        _: &Transaction,
        kind: CatalogType,
        schema: &[u8],
        name: &[u8],
    ) -> Result<Option<Arc<CatalogEntry>>> {
        if name == b"broken" {
            return Err(Error::new(ErrorKind::Io, "provider connection failed"));
        }
        if kind == CatalogType::Schema && name == b"public" {
            return Ok(Some(self.schema.clone()));
        }
        Ok(
            (kind == CatalogType::Table && schema == b"public" && name == b"items")
                .then(|| self.table.clone()),
        )
    }
}
#[derive(Debug)]
struct Rows;
impl TableBackend for Rows {
    fn scan(&self, _: &Arc<FunctionClient>, _: &Transaction) -> Result<BackendScan> {
        let mut function =
            TableFunction::new("remote_test_scan", vec![]).with_scan(|_, ids, state, chunk| {
                let mut id = Vector::flat(LogicalType::INTEGER, 2);
                let mut label = Vector::flat(LogicalType::VARCHAR, 2);
                let ty = LogicalType::struct_type(vec![("score".into(), LogicalType::INTEGER)]);
                let mut details = Vector::flat(ty.clone(), 2);
                let count = if state.position == 0 {
                    id.set_value(0, &Value::integer(1))?;
                    id.set_value(1, &Value::integer(2))?;
                    label.set_value(0, &Value::varchar("one"))?;
                    label.set_value(1, &Value::varchar("two"))?;
                    details.set_value(0, &Value::from_children(ty.clone(), vec![Value::integer(10)]))?;
                    details.set_value(1, &Value::from_children(ty, vec![Value::integer(20)]))?;
                    state.position = 2;
                    2
                } else {
                    0
                };
                let columns = [id, label, details];
                *chunk = DataChunk::new(
                    ids.iter().map(|id| columns[*id as usize].clone()).collect(),
                    count,
                );
                Ok(false)
            });
        function.projection_pushdown = true;
        function.projected_output = true;
        Ok(BackendScan {
            function,
            bind_data: None,
            virtual_columns: vec![],
        })
    }
}

#[test]
fn remote_table_keeps_identity_without_native_storage_or_capabilities() -> Result<()> {
    let database = crate::Database::open_in_memory()?;
    let mut connection = database.connect()?;
    connection.query("ATTACH ':memory:' AS remote; CREATE SCHEMA remote.public; CREATE TABLE remote.public.template(id INTEGER,label VARCHAR,details STRUCT(score INTEGER))")?;
    let attached = database.instance().databases.get("remote").unwrap();
    let catalog = &attached.catalog;
    let native = catalog
        .get_entry(&Transaction::system(), CatalogType::Table, "public", "template")
        .unwrap();
    let EntryKind::Table(table) = &native.kind else {
        panic!("table fixture");
    };
    let mut table = table.clone();
    table.storage = None;
    table.backend = Some(Arc::new(Rows));
    let entry = Arc::new(catalog.new_entry("items", EntryKind::Table(table)));
    let fail_listing = Arc::new(std::sync::atomic::AtomicBool::new(false));
    catalog.set_backend(Arc::new(Metadata {
        schema: catalog.schema(&Transaction::system(), "public").unwrap(),
        table: entry.clone(),
        fail_listing: fail_listing.clone(),
    }))?;
    assert_eq!(catalog.catalog_type(), "test_remote");
    assert_eq!(
        connection
            .query("SELECT table_name FROM duckdb_tables() WHERE database_name='remote'")?
            .value(0, 0),
        Value::varchar("items")
    );
    assert_eq!(
        connection
            .query("SELECT count(*) FROM duckdb_columns() WHERE database_name='remote'")?
            .value(0, 0),
        Value::bigint(3)
    );
    assert_eq!(
        connection
            .query("SELECT details.score FROM remote.items WHERE id=2")?
            .value(0, 0),
        Value::integer(20)
    );
    fail_listing.store(true, std::sync::atomic::Ordering::Relaxed);
    let error = connection.query("SELECT * FROM duckdb_sequences()").unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Io);
    assert!(error.to_string().contains("provider enumeration failed"));
    let active = database.instance().transactions.active_count();
    assert!(connection.query("COPY FROM DATABASE remote TO memory").is_err());
    assert_eq!(database.instance().transactions.active_count(), active);
    fail_listing.store(false, std::sync::atomic::Ordering::Relaxed);
    connection.query("COPY FROM DATABASE remote TO memory")?;
    assert_eq!(
        connection
            .query("SELECT sum(details.score) FROM memory.public.items")?
            .value(0, 0),
        Value::hugeint(30)
    );
    assert!(Arc::ptr_eq(
        &entry,
        &catalog
            .lookup_entry(
                &Arc::default(),
                &Transaction::system(),
                CatalogType::Table,
                b"public",
                b"items"
            )?
            .unwrap()
    ));
    assert_eq!(
        connection
            .query("SELECT label FROM remote.items WHERE id=2")?
            .value(0, 0),
        Value::varchar("two")
    );
    assert_eq!(
        connection
            .query("SELECT count(*) FROM remote.public.items")?
            .value(0, 0),
        Value::bigint(2)
    );
    assert_eq!(
        connection
            .query("SELECT a.label FROM remote.items a JOIN remote.items b USING(id) WHERE b.id=1")?
            .value(0, 0),
        Value::varchar("one")
    );
    assert!(
        connection
            .query("EXPLAIN SELECT * FROM remote.items")?
            .value(1, 0)
            .to_varchar()?
            .contains("REMOTE_TEST_SCAN")
    );
    let error = connection.query("SELECT * FROM remote.broken").unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Io);
    assert!(error.to_string().contains("provider connection failed"));
    assert!(connection.query("SELECT rowid FROM remote.items").is_err());
    connection.query("USE remote")?;
    assert_eq!(
        connection.query("SELECT count(*) FROM items")?.value(0, 0),
        Value::bigint(2)
    );
    Ok(())
}

#[derive(Debug)]
struct Participant {
    name: &'static str,
    log: Arc<std::sync::Mutex<Vec<String>>>,
    manager: Arc<roastedduck_storage::transaction::TransactionManager>,
    fail_commit: bool,
}
impl roastedduck_storage::transaction::TransactionParticipant for Participant {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn commit(&self) -> Result<()> {
        // This takes the manager mutex: provider I/O must run outside that lock.
        let _ = self.manager.active_count();
        self.log.lock().unwrap().push(format!("commit {}", self.name));
        if self.fail_commit {
            Err(Error::new(ErrorKind::Io, "provider commit failed"))
        } else {
            Ok(())
        }
    }
    fn rollback(&self) -> Result<()> {
        let _ = self.manager.active_count();
        self.log.lock().unwrap().push(format!("rollback {}", self.name));
        Ok(())
    }
}

#[test]
fn remote_transactions_are_owned_reused_and_finished_outside_native_lock() -> Result<()> {
    let manager = Arc::new(roastedduck_storage::transaction::TransactionManager::default());
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    let transaction = manager.begin();
    let writer = transaction.participant(b"writer", || {
        Ok(Arc::new(Participant {
            name: "writer",
            log: log.clone(),
            manager: manager.clone(),
            fail_commit: false,
        }))
    })?;
    let reused = transaction.participant(b"WRITER", || panic!("must reuse participant"))?;
    assert!(Arc::ptr_eq(&writer, &reused));
    transaction.participant(b"reader", || {
        Ok(Arc::new(Participant {
            name: "reader",
            log: log.clone(),
            manager: manager.clone(),
            fail_commit: false,
        }))
    })?;
    transaction.modify_database(
        "writer",
        None,
        &roastedduck_storage::checkpoint_lock::DatabaseLocks::default(),
        false,
        false,
    )?;
    manager.commit(&transaction)?;
    assert_eq!(*log.lock().unwrap(), ["commit reader", "commit writer"]);
    drop(transaction);
    assert_eq!(log.lock().unwrap().len(), 2);
    assert_eq!(manager.active_count(), 0);

    log.lock().unwrap().clear();
    let transaction = manager.begin();
    for (name, fail_commit) in [("failed", true), ("pending", false)] {
        transaction.participant(name.as_bytes(), || {
            Ok(Arc::new(Participant {
                name,
                log: log.clone(),
                manager: manager.clone(),
                fail_commit,
            }))
        })?;
    }
    assert!(
        manager
            .commit(&transaction)
            .unwrap_err()
            .to_string()
            .contains("provider commit failed")
    );
    assert_eq!(
        *log.lock().unwrap(),
        ["commit failed", "rollback failed", "rollback pending"]
    );
    assert_eq!(manager.active_count(), 0);
    drop(transaction);
    assert_eq!(log.lock().unwrap().len(), 3);

    log.lock().unwrap().clear();
    {
        let transaction = Transaction::system();
        transaction.participant(b"abandoned", || {
            Ok(Arc::new(Participant {
                name: "abandoned",
                log: log.clone(),
                manager: manager.clone(),
                fail_commit: false,
            }))
        })?;
    }
    assert_eq!(*log.lock().unwrap(), ["rollback abandoned"]);
    Ok(())
}

#[test]
fn explicit_native_search_path_keeps_implicit_main_fallback() -> Result<()> {
    let database = crate::Database::open_in_memory()?;
    let mut connection = database.connect()?;
    connection.query(
        "CREATE SCHEMA alternate; CREATE TABLE fallback_item AS SELECT 42 i; SET search_path='alternate'",
    )?;
    assert_eq!(
        connection.query("SELECT * FROM fallback_item")?.value(0, 0),
        Value::integer(42)
    );
    Ok(())
}
