//! First-party PostgreSQL backend. SQL/catalog integration is owned by the runtime.
pub mod config;
mod tls;

use bytes::{Bytes, BytesMut};
use futures_util::{SinkExt, TryStreamExt};
use roastedduck_common::{Error, ErrorKind, Result};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tokio_postgres::types::{FromSql, IsNull, ToSql, Type, to_sql_checked};

fn error(error: impl std::fmt::Display) -> Error {
    Error::new(ErrorKind::Io, error.to_string())
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
impl Session {
    pub fn batch(&self, sql: &str) -> Result<()> {
        self.driver
            .wait(self.client.batch_execute(sql))?
            .map_err(pg_error)
    }
    pub fn describe(&self, sql: &str) -> Result<Vec<Column>> {
        self.driver
            .wait(self.client.prepare(sql))?
            .map(|s| columns(&s))
            .map_err(pg_error)
    }
    pub fn stream(&self, sql: &str, parameters: &[BinaryParameter]) -> Result<Rows> {
        let statement = self.driver.wait(self.client.prepare(sql))?.map_err(pg_error)?;
        let cols = columns(&statement);
        let stream = self
            .driver
            .wait(
                self.client
                    .query_raw(&statement, parameters.iter().map(|p| p as &(dyn ToSql + Sync))),
            )?
            .map_err(pg_error)?;
        Ok(Rows {
            driver: self.driver.clone(),
            columns: cols,
            stream: Box::pin(stream),
        })
    }
    pub fn copy_out(&self, sql: &str) -> Result<CopyReader> {
        let stream = self.driver.wait(self.client.copy_out(sql))?.map_err(pg_error)?;
        Ok(CopyReader {
            driver: self.driver.clone(),
            stream: Box::pin(stream),
        })
    }
    pub fn copy_in(&self, sql: &str) -> Result<CopyWriter> {
        let sink = self.driver.wait(self.client.copy_in(sql))?.map_err(pg_error)?;
        Ok(CopyWriter {
            driver: self.driver.clone(),
            sink: Box::pin(sink),
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
        let driver = self.driver.clone();
        driver.wait(async {
            let mut result = Vec::new();
            while result.len() < limit {
                let Some(row) = self.stream.as_mut().try_next().await.map_err(pg_error)? else {
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
        })?
    }
}
pub struct CopyReader {
    driver: Driver,
    stream: Pin<Box<tokio_postgres::CopyOutStream>>,
}
impl std::fmt::Debug for CopyReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PostgresCopyReader")
    }
}
impl CopyReader {
    pub fn next_chunk(&mut self) -> Result<Option<Bytes>> {
        let driver = self.driver.clone();
        driver.wait(self.stream.as_mut().try_next())?.map_err(pg_error)
    }
}
pub struct CopyWriter {
    driver: Driver,
    sink: Pin<Box<tokio_postgres::CopyInSink<Bytes>>>,
}
impl std::fmt::Debug for CopyWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PostgresCopyWriter")
    }
}
impl CopyWriter {
    pub fn write(&mut self, bytes: Bytes) -> Result<()> {
        let driver = self.driver.clone();
        driver.wait(self.sink.as_mut().send(bytes))?.map_err(pg_error)
    }
    pub fn finish(mut self) -> Result<u64> {
        let driver = self.driver.clone();
        driver.wait(self.sink.as_mut().finish())?.map_err(pg_error)
    }
}
