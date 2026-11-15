//! First-party PostgreSQL backend. SQL/catalog integration is owned by the runtime.
pub mod binary_file;
pub mod codec;
pub mod config;
pub mod copy;
pub mod copy_rows;
pub mod parameters;
pub mod pool;
mod tls;

use bytes::{Bytes, BytesMut};
use futures_util::{SinkExt, TryStreamExt};
use roastedduck_common::{Error, ErrorKind, Result};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio_postgres::types::{FromSql, IsNull, ToSql, Type, to_sql_checked};

fn error(error: impl std::fmt::Display) -> Error {
    Error::new(ErrorKind::Io, error.to_string())
}

/// Retain the pending future across timer ticks. Dropping/recreating a query on
/// each tick could execute it twice. On interruption, send PostgreSQL's separate
/// cancellation request before returning ownership to the caller.
async fn interruptible<F: Future>(
    future: F,
    interrupted: Option<&AtomicBool>,
    token: &tokio_postgres::CancelToken,
    parameters: &config::Parameters,
) -> Result<F::Output> {
    let Some(interrupted) = interrupted else {
        return Ok(future.await);
    };
    if interrupted.load(Ordering::Relaxed) {
        return Err(Error::new(ErrorKind::Interrupt, "Interrupted!"));
    }
    let mut future = std::pin::pin!(future);
    loop {
        match tokio::time::timeout(std::time::Duration::from_millis(20), future.as_mut()).await {
            Ok(result) => return Ok(result),
            Err(_) if interrupted.load(Ordering::Relaxed) => {
                let cancel = async {
                    if parameters.get("sslmode") == Some("disable") {
                        let _ = token.cancel_query(tokio_postgres::NoTls).await;
                    } else if let Ok(connector) = tls::connector(parameters) {
                        let _ = token.cancel_query(connector).await;
                    }
                };
                // A dead endpoint must not turn cancellation into an unbounded wait.
                let _ = tokio::time::timeout(std::time::Duration::from_secs(2), cancel).await;
                return Err(Error::new(ErrorKind::Interrupt, "Interrupted!"));
            }
            Err(_) => {}
        }
    }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "map_err adapter takes ownership of the provider error"
)]
fn pg_error(error: tokio_postgres::Error) -> Error {
    use std::error::Error as _;
    if let Some(database) = error.as_db_error() {
        let mut message = format!("{}: {}", database.severity(), database.message());
        if let Some(detail) = database.detail() {
            message.push_str("\nDETAIL: ");
            message.push_str(detail);
        }
        if let Some(hint) = database.hint() {
            message.push_str("\nHINT: ");
            message.push_str(hint);
        }
        return Error::new(ErrorKind::Io, message).with_extra_info("sqlstate", database.code().code());
    }
    let mut message = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    Error::new(ErrorKind::Io, message)
}

#[derive(Debug)]
struct RuntimeOwner(Option<tokio::runtime::Runtime>);
impl Drop for RuntimeOwner {
    fn drop(&mut self) {
        if let Some(runtime) = self.0.take() {
            if tokio::runtime::Handle::try_current().is_ok() {
                let _ = std::thread::spawn(move || drop(runtime)).join();
            } else {
                drop(runtime);
            }
        }
    }
}

/// An owned I/O runtime, shared by a database's PostgreSQL connections.
#[derive(Clone, Debug)]
pub struct Driver(Arc<RuntimeOwner>);
impl Driver {
    pub fn new() -> Result<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(error)?;
        Ok(Self(Arc::new(RuntimeOwner(Some(runtime)))))
    }
    fn wait<F: Future + Send>(&self, future: F) -> Result<F::Output>
    where
        F::Output: Send,
    {
        let runtime = self
            .0
            .0
            .as_ref()
            .ok_or_else(|| Error::internal("PostgreSQL runtime is closed"))?;
        if tokio::runtime::Handle::try_current().is_ok() {
            std::thread::scope(|scope| scope.spawn(|| runtime.block_on(future)).join())
                .map_err(|_| Error::internal("PostgreSQL runtime worker panicked"))
        } else {
            Ok(runtime.block_on(future))
        }
    }
    pub fn connect(&self, parameters: &config::Parameters) -> Result<Session> {
        let config = parameters.wire_config()?;
        let mode = parameters.get("sslmode").unwrap_or("prefer");
        let (client, task) = self.wait(async {
            if mode == "disable" || mode == "allow" {
                match config.connect(tokio_postgres::NoTls).await {
                    Ok((client, connection)) => return Ok((client, tokio::spawn(connection))),
                    Err(e) if mode == "disable" => return Err(pg_error(e)),
                    Err(_) => {}
                }
            }
            let mut config = config;
            if mode == "allow" {
                config.ssl_mode(tokio_postgres::config::SslMode::Require);
            }
            let connector = tls::connector(parameters)?;
            config
                .connect(connector)
                .await
                .map(|(client, c)| (client, tokio::spawn(c)))
                .map_err(pg_error)
        })??;
        Ok(Session {
            driver: self.clone(),
            client,
            task,
            parameters: parameters.clone(),
        })
    }
}

/// Wire bytes for an extended-query parameter. Encoding follows the DuckDB input
/// type, so the server can reject a mismatched inferred type as upstream does.
#[derive(Clone, Debug)]
pub struct BinaryParameter(pub Option<Vec<u8>>);

/// SQL `postgres_query` parameters use the pinned extension's per-value wire format.
#[derive(Clone, Debug)]
pub struct QueryParameter {
    pub bytes: Option<Vec<u8>>,
    pub text: bool,
}
impl ToSql for QueryParameter {
    fn to_sql(
        &self,
        _ty: &Type,
        out: &mut BytesMut,
    ) -> std::result::Result<IsNull, Box<dyn std::error::Error + Send + Sync>> {
        if let Some(bytes) = &self.bytes {
            out.extend_from_slice(bytes);
            Ok(IsNull::No)
        } else {
            Ok(IsNull::Yes)
        }
    }
    fn accepts(_ty: &Type) -> bool {
        true
    }
    fn encode_format(&self, _ty: &Type) -> tokio_postgres::types::Format {
        if self.text {
            tokio_postgres::types::Format::Text
        } else {
            tokio_postgres::types::Format::Binary
        }
    }
    to_sql_checked!();
}
impl ToSql for BinaryParameter {
    fn to_sql(
        &self,
        _ty: &Type,
        out: &mut BytesMut,
    ) -> std::result::Result<IsNull, Box<dyn std::error::Error + Send + Sync>> {
        if let Some(bytes) = &self.0 {
            out.extend_from_slice(bytes);
            Ok(IsNull::No)
        } else {
            Ok(IsNull::Yes)
        }
    }
    fn accepts(_ty: &Type) -> bool {
        true
    }
    to_sql_checked!();
}
#[derive(Debug)]
struct Raw(Vec<u8>);
impl<'a> FromSql<'a> for Raw {
    fn from_sql(
        _ty: &Type,
        bytes: &'a [u8],
    ) -> std::result::Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Ok(Self(bytes.to_vec()))
    }
    fn accepts(_ty: &Type) -> bool {
        true
    }
}

#[derive(Clone, Debug)]
pub struct Column {
    pub name: String,
    pub data_type: Type,
    pub type_modifier: i32,
    pub table_oid: Option<u32>,
    pub column_id: Option<i16>,
}

#[derive(Clone, Debug)]
pub struct Description {
    pub columns: Vec<Column>,
    pub parameter_types: Vec<Type>,
}
fn columns(statement: &tokio_postgres::Statement) -> Vec<Column> {
    statement
        .columns()
        .iter()
        .map(|c| Column {
            name: c.name().into(),
            data_type: c.type_().clone(),
            type_modifier: c.type_modifier(),
            table_oid: c.table_oid(),
            column_id: c.column_id(),
        })
        .collect()
}

#[derive(Debug)]
pub struct Session {
    driver: Driver,
    client: tokio_postgres::Client,
    task: tokio::task::JoinHandle<std::result::Result<(), tokio_postgres::Error>>,
    parameters: config::Parameters,
}
impl Drop for Session {
    fn drop(&mut self) {
        self.task.abort();
    }
}
/// The last result of a Simple Query message, as returned by upstream `PQexec`.
#[derive(Debug, Default)]
pub struct TextResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Option<String>>>,
}
impl Session {
    pub fn catalog_column(
        &self,
        name: String,
        oid: u32,
        modifier: i32,
        interrupted: Option<&AtomicBool>,
    ) -> Result<Column> {
        let data_type = self
            .driver
            .wait(interruptible(
                self.client.resolve_type(oid),
                interrupted,
                &self.client.cancel_token(),
                &self.parameters,
            ))??
            .map_err(pg_error)?;
        Ok(Column {
            name,
            data_type,
            type_modifier: modifier,
            table_oid: None,
            column_id: None,
        })
    }
    pub fn text_query(&self, sql: &str, interrupted: Option<&AtomicBool>) -> Result<TextResult> {
        let messages = self
            .driver
            .wait(interruptible(
                self.client.simple_query(sql),
                interrupted,
                &self.client.cancel_token(),
                &self.parameters,
            ))??
            .map_err(pg_error)?;
        let mut current = TextResult::default();
        let mut last = TextResult::default();
        for message in messages {
            match message {
                tokio_postgres::SimpleQueryMessage::RowDescription(columns) => {
                    current.columns = columns.iter().map(|c| c.name().to_owned()).collect();
                }
                tokio_postgres::SimpleQueryMessage::Row(row) => current
                    .rows
                    .push((0..row.len()).map(|i| row.get(i).map(str::to_owned)).collect()),
                tokio_postgres::SimpleQueryMessage::CommandComplete(_) => last = std::mem::take(&mut current),
                _ => {}
            }
        }
        Ok(last)
    }
    /// A pool health check succeeds only for a tuple-producing final command.
    pub fn ping(&self, sql: &str) -> Result<bool> {
        if self.is_closed() {
            return Ok(false);
        }
        if sql.is_empty() {
            return Ok(true);
        }
        let messages = self
            .driver
            .wait(self.client.simple_query(sql))?
            .map_err(pg_error)?;
        let mut rows = false;
        let mut last = false;
        for message in messages {
            match message {
                tokio_postgres::SimpleQueryMessage::RowDescription(_) => rows = true,
                tokio_postgres::SimpleQueryMessage::CommandComplete(_) => {
                    last = rows;
                    rows = false;
                }
                _ => {}
            }
        }
        Ok(last)
    }

    pub fn reset_for_pool(&mut self, health_check: &str) -> Result<()> {
        if !matches!(self.client.transaction_status(), b'I' | b'T' | b'E') {
            self.driver
                .wait(self.client.check_connection())?
                .map_err(pg_error)?;
        }
        let in_transaction = matches!(self.client.transaction_status(), b'T' | b'E');
        self.client.clear_session_cache();
        let reset = if in_transaction {
            self.batch("ROLLBACK")
        } else {
            Ok(())
        };
        // DISCARD ALL must be the only command in its protocol query message.
        if reset.and_then(|()| self.batch("DISCARD ALL")).is_ok() {
            return Ok(());
        }
        let session = self.driver.connect(&self.parameters)?;
        *self = session;
        if self.ping(health_check)? {
            Ok(())
        } else {
            Err(Error::internal("Connection reset failure"))
        }
    }

    pub fn batch(&self, sql: &str) -> Result<()> {
        self.driver
            .wait(self.client.batch_execute(sql))?
            .map_err(pg_error)
    }
    pub fn batch_interruptible(&self, sql: &str, interrupted: Option<&AtomicBool>) -> Result<()> {
        self.driver
            .wait(interruptible(
                self.client.batch_execute(sql),
                interrupted,
                &self.client.cancel_token(),
                &self.parameters,
            ))??
            .map_err(pg_error)
    }
    pub fn describe(&self, sql: &str) -> Result<Vec<Column>> {
        self.driver
            .wait(self.client.prepare(sql))?
            .map(|s| columns(&s))
            .map_err(pg_error)
    }
    pub fn describe_interruptible(&self, sql: &str, interrupted: Option<&AtomicBool>) -> Result<Vec<Column>> {
        self.driver
            .wait(interruptible(
                self.client.prepare(sql),
                interrupted,
                &self.client.cancel_token(),
                &self.parameters,
            ))??
            .map(|statement| columns(&statement))
            .map_err(pg_error)
    }
    pub fn describe_query(&self, sql: &str, interrupted: Option<&AtomicBool>) -> Result<Description> {
        self.driver
            .wait(interruptible(
                self.client.prepare(sql),
                interrupted,
                &self.client.cancel_token(),
                &self.parameters,
            ))??
            .map(|statement| Description {
                columns: columns(&statement),
                parameter_types: statement.params().to_vec(),
            })
            .map_err(pg_error)
    }
    pub fn stream(&self, sql: &str, parameters: &[BinaryParameter]) -> Result<Rows> {
        self.stream_interruptible(sql, parameters, None)
    }
    pub fn stream_interruptible(
        &self,
        sql: &str,
        parameters: &[BinaryParameter],
        interrupted: Option<&AtomicBool>,
    ) -> Result<Rows> {
        let parameters = parameters
            .iter()
            .map(|p| p as &(dyn ToSql + Sync))
            .collect::<Vec<_>>();
        self.stream_parameters(sql, &parameters, false, interrupted)
    }
    pub fn stream_text(
        &self,
        sql: &str,
        parameters: &[BinaryParameter],
        interrupted: Option<&AtomicBool>,
    ) -> Result<Rows> {
        let parameters = parameters
            .iter()
            .map(|p| p as &(dyn ToSql + Sync))
            .collect::<Vec<_>>();
        self.stream_parameters(sql, &parameters, true, interrupted)
    }
    pub fn stream_query(
        &self,
        sql: &str,
        parameters: &[QueryParameter],
        text: bool,
        interrupted: Option<&AtomicBool>,
    ) -> Result<Rows> {
        let parameters = parameters
            .iter()
            .map(|p| p as &(dyn ToSql + Sync))
            .collect::<Vec<_>>();
        self.stream_parameters(sql, &parameters, text, interrupted)
    }
    fn stream_parameters(
        &self,
        sql: &str,
        parameters: &[&(dyn ToSql + Sync)],
        text: bool,
        interrupted: Option<&AtomicBool>,
    ) -> Result<Rows> {
        let token = self.client.cancel_token();
        let (cols, stream) = self.driver.wait(interruptible(
            async {
                let statement = self.client.prepare(sql).await.map_err(pg_error)?;
                let cols = columns(&statement);
                let stream = if text {
                    self.client.query_raw_text(&statement, parameters.to_vec()).await
                } else {
                    self.client.query_raw(&statement, parameters.to_vec()).await
                }
                .map_err(pg_error)?;
                Ok::<_, Error>((cols, stream))
            },
            interrupted,
            &token,
            &self.parameters,
        ))???;
        Ok(Rows {
            driver: self.driver.clone(),
            columns: cols,
            stream: Box::pin(stream),
            exhausted: false,
            cancel_token: token,
            parameters: self.parameters.clone(),
        })
    }
    pub fn copy_out(&self, sql: &str) -> Result<CopyReader> {
        self.copy_out_interruptible(sql, None)
    }
    pub fn copy_out_interruptible(&self, sql: &str, interrupted: Option<&AtomicBool>) -> Result<CopyReader> {
        let token = self.client.cancel_token();
        let stream = self
            .driver
            .wait(interruptible(
                self.client.copy_out(sql),
                interrupted,
                &token,
                &self.parameters,
            ))??
            .map_err(pg_error)?;
        Ok(CopyReader {
            driver: self.driver.clone(),
            stream: Box::pin(stream),
            exhausted: false,
            cancel_token: token,
            parameters: self.parameters.clone(),
        })
    }
    pub fn copy_in(&self, sql: &str) -> Result<CopyWriter> {
        let sink = self.driver.wait(self.client.copy_in(sql))?.map_err(pg_error)?;
        Ok(CopyWriter {
            driver: self.driver.clone(),
            sink: Box::pin(sink),
            cancel_token: self.client.cancel_token(),
            parameters: self.parameters.clone(),
        })
    }
    pub fn cancel(&self) -> Result<()> {
        let token = self.client.cancel_token();
        if self.parameters.get("sslmode") == Some("disable") {
            self.driver
                .wait(token.cancel_query(tokio_postgres::NoTls))?
                .map_err(pg_error)
        } else {
            self.driver
                .wait(token.cancel_query(tls::connector(&self.parameters)?))?
                .map_err(pg_error)
        }
    }
    pub fn is_closed(&self) -> bool {
        self.client.is_closed()
    }
}

pub struct Rows {
    driver: Driver,
    pub columns: Vec<Column>,
    stream: Pin<Box<tokio_postgres::RowStream>>,
    exhausted: bool,
    cancel_token: tokio_postgres::CancelToken,
    parameters: config::Parameters,
}
impl std::fmt::Debug for Rows {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PostgresRows")
            .field("columns", &self.columns)
            .finish_non_exhaustive()
    }
}
impl Rows {
    pub fn next_batch(&mut self, limit: usize) -> Result<Vec<Vec<Option<Vec<u8>>>>> {
        self.next_batch_interruptible(limit, None)
    }
    pub fn next_batch_interruptible(
        &mut self,
        limit: usize,
        interrupted: Option<&AtomicBool>,
    ) -> Result<Vec<Vec<Option<Vec<u8>>>>> {
        if self.exhausted {
            return Ok(Vec::new());
        }
        let driver = self.driver.clone();
        driver.wait(interruptible(
            async {
                let mut result = Vec::new();
                while result.len() < limit {
                    let Some(row) = self.stream.as_mut().try_next().await.map_err(pg_error)? else {
                        self.exhausted = true;
                        break;
                    };
                    result.push(
                        (0..row.len())
                            .map(|i| {
                                row.try_get::<_, Option<Raw>>(i)
                                    .map(|v| v.map(|v| v.0))
                                    .map_err(pg_error)
                            })
                            .collect::<Result<_>>()?,
                    );
                }
                Ok(result)
            },
            interrupted,
            &self.cancel_token,
            &self.parameters,
        ))??
    }
}
pub struct CopyReader {
    driver: Driver,
    cancel_token: tokio_postgres::CancelToken,
    parameters: config::Parameters,
    stream: Pin<Box<tokio_postgres::CopyOutStream>>,
    exhausted: bool,
}
impl std::fmt::Debug for CopyReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PostgresCopyReader")
    }
}
impl CopyReader {
    pub fn next_chunk(&mut self) -> Result<Option<Bytes>> {
        self.next_chunk_interruptible(None)
    }
    pub fn next_chunk_interruptible(&mut self, interrupted: Option<&AtomicBool>) -> Result<Option<Bytes>> {
        if self.exhausted {
            return Ok(None);
        }
        let driver = self.driver.clone();
        let result = driver
            .wait(interruptible(
                self.stream.as_mut().try_next(),
                interrupted,
                &self.cancel_token,
                &self.parameters,
            ))??
            .map_err(pg_error)?;
        self.exhausted = result.is_none();
        Ok(result)
    }
}

pub struct CopyWriter {
    driver: Driver,
    sink: Pin<Box<tokio_postgres::CopyInSink<Bytes>>>,
    cancel_token: tokio_postgres::CancelToken,
    parameters: config::Parameters,
}
impl std::fmt::Debug for CopyWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PostgresCopyWriter")
    }
}
impl CopyWriter {
    pub fn write(&mut self, bytes: Bytes) -> Result<()> {
        self.write_interruptible(bytes, None)
    }
    pub fn write_interruptible(&mut self, bytes: Bytes, interrupted: Option<&AtomicBool>) -> Result<()> {
        let driver = self.driver.clone();
        driver
            .wait(interruptible(
                self.sink.as_mut().send(bytes),
                interrupted,
                &self.cancel_token,
                &self.parameters,
            ))??
            .map_err(pg_error)
    }
    pub fn finish(self) -> Result<u64> {
        self.finish_interruptible(None)
    }
    pub fn finish_interruptible(mut self, interrupted: Option<&AtomicBool>) -> Result<u64> {
        let driver = self.driver.clone();
        driver
            .wait(interruptible(
                self.sink.as_mut().finish(),
                interrupted,
                &self.cancel_token,
                &self.parameters,
            ))??
            .map_err(pg_error)
    }
}
