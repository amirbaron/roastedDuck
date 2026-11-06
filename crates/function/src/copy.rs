//! Copy functions (DuckDB's `CopyFunction`): the file formats of `COPY ... TO` and
//! `COPY ... FROM` (and `EXPORT DATABASE`). A format provides the options it accepts,
//! a writer (bind, global state per file, sink, finalize) and a reader (a table function
//! bound with the target table's columns).

use std::any::Any;
use std::fmt;
use std::sync::Arc;

use rustdb_common::{CaseInsensitiveMap, Result};
use rustdb_types::{DataChunk, LogicalType, Value};

use crate::{FunctionData, TableFunction};

/// Whether a copy option applies to reading, writing or both (DuckDB's
/// `CopyOptionMode`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyOptionMode {
    WriteOnly,
    ReadOnly,
    ReadWrite,
}

/// An option a copy function accepts (DuckDB's `CopyOption`).
#[derive(Clone, Debug)]
pub struct CopyOption {
    pub ty: LogicalType,
    pub mode: CopyOptionMode,
}

impl CopyOption {
    pub fn new(ty: LogicalType, mode: CopyOptionMode) -> Self {
        Self { ty, mode }
    }
}

/// What `copy_to_bind` receives (DuckDB's `CopyFunctionBindInput`).
#[derive(Debug)]
pub struct CopyFunctionBindInput<'a> {
    /// The options of the statement that are not generic COPY options.
    pub options: &'a CaseInsensitiveMap<Vec<Value>, rustdb_common::ByteString>,
    pub file_path: &'a [u8],
    pub format: &'a [u8],
    /// The extension of written files; the function may extend it (e.g. `.gz`).
    pub file_extension: String,
    /// The binder's client context (the planner's `ClientContext`).
    pub binder: Option<&'a dyn Any>,
}

/// What `copy_from_bind` receives (DuckDB's `CopyFromFunctionBindInput`).
#[derive(Debug)]
pub struct CopyFromBindInput<'a> {
    pub options: &'a CaseInsensitiveMap<Vec<Value>, rustdb_common::ByteString>,
    pub file_path: &'a [u8],
    pub format: &'a [u8],
    /// The binder's client context (the planner's `ClientContext`).
    pub binder: Option<&'a dyn Any>,
}

/// What a write is part of (DuckDB's `CopyToType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyToType {
    CopyToFile,
    ExportDatabase,
}

/// What `copy_to_select` receives (DuckDB's `CopyToSelectInput`): the columns of the
/// query being written.
#[derive(Debug)]
pub struct CopyToSelectInput<'a> {
    pub names: &'a [rustdb_common::ByteString],
    pub types: &'a [LogicalType],
    pub options: &'a CaseInsensitiveMap<Vec<Value>, rustdb_common::ByteString>,
    pub copy_to_type: CopyToType,
    /// The binder's client context (the planner's `ClientContext`).
    pub binder: Option<&'a dyn Any>,
}

/// `copy_to_select`: the types the format wants the written columns cast to, or `None`
/// to write them as they are (the binder adds the casts in a projection).
pub type CopyToSelect = Arc<dyn Fn(&CopyToSelectInput<'_>) -> Result<Option<Vec<LogicalType>>> + Send + Sync>;

/// The state of one written file (DuckDB's `GlobalFunctionData`); the function keeps
/// its writer behind interior mutability.
pub type CopyGlobalState = Box<dyn Any + Send + Sync>;

/// `copy_to_bind`: the bind data of a write of `names` / `types`.
pub type CopyToBind = Arc<
    dyn Fn(
            &mut CopyFunctionBindInput<'_>,
            &[rustdb_common::ByteString],
            &[LogicalType],
        ) -> Result<Arc<dyn FunctionData>>
        + Send
        + Sync,
>;
/// `copy_to_initialize_global`: opens the file at the path.
pub type CopyToInitializeGlobal =
    Arc<dyn Fn(&dyn FunctionData, &[u8]) -> Result<CopyGlobalState> + Send + Sync>;
/// `copy_to_initialize_operator`: hands a file's state the logger of the COPY operator,
/// when the query logs it.
pub type CopyToInitializeOperator =
    Arc<dyn Fn(&CopyGlobalState, &crate::logging::OperatorLogger) + Send + Sync>;
/// `copy_to_sink`: writes a chunk.
pub type CopyToSink =
    Arc<dyn Fn(&dyn FunctionData, &CopyGlobalState, &DataChunk) -> Result<()> + Send + Sync>;
/// `copy_to_finalize`: completes and closes the file.
pub type CopyToFinalize = Arc<dyn Fn(&dyn FunctionData, &CopyGlobalState) -> Result<()> + Send + Sync>;
/// What `RETURN_STATS` reports about a written file (DuckDB's
/// `CopyFunctionFileStatistics`).
#[derive(Clone, Debug, Default)]
pub struct CopyFunctionFileStatistics {
    pub row_count: u64,
    pub file_size_bytes: u64,
    pub footer_size_bytes: Value,
    /// Per column: statistic name -> value.
    pub column_statistics: Vec<(rustdb_common::ByteString, Vec<(String, Value)>)>,
}

/// `copy_to_get_written_statistics`: the statistics of a finalized file.
pub type CopyToGetWrittenStatistics =
    Arc<dyn Fn(&dyn FunctionData, &CopyGlobalState) -> Result<CopyFunctionFileStatistics> + Send + Sync>;
/// The bytes written to a file so far (for `FILE_SIZE_BYTES` rotation).
pub type CopyToFileSize = Arc<dyn Fn(&CopyGlobalState) -> u64 + Send + Sync>;
/// `rotate_files`: whether a write with this bind data and `FILE_SIZE_BYTES` limit is
/// split into several files.
pub type CopyRotateFiles = Arc<dyn Fn(&dyn FunctionData, Option<u64>) -> bool + Send + Sync>;
/// `rotate_next_file`: whether the current file is complete and the next chunk goes to
/// a new one.
pub type CopyRotateNextFile =
    Arc<dyn Fn(&CopyGlobalState, &dyn FunctionData, Option<u64>) -> bool + Send + Sync>;
/// `copy_from_bind`: the bind data of the reader for the target columns.
pub type CopyFromBind = Arc<
    dyn Fn(
            &CopyFromBindInput<'_>,
            &[rustdb_common::ByteString],
            &[LogicalType],
        ) -> Result<Arc<dyn FunctionData>>
        + Send
        + Sync,
>;

/// How a COPY runs (DuckDB's `CopyFunctionExecutionMode`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyFunctionExecutionMode {
    /// One thread writes the rows in order.
    RegularCopyToFile,
    /// Several threads write, order not kept.
    ParallelCopyToFile,
    /// Several threads prepare batches that are written in batch order.
    BatchCopyToFile,
}

/// `execution_mode`: how a write runs, given whether the order must be kept and whether
/// every source numbers its rows in batches.
pub type CopyExecutionMode =
    Arc<dyn Fn(&dyn FunctionData, bool, bool) -> CopyFunctionExecutionMode + Send + Sync>;
/// A batch prepared for writing (DuckDB's `PreparedBatchData`).
pub type PreparedBatch = Box<dyn Any + Send>;
/// Replayable, buffer-managed rows passed to a batched writer. A column scan
/// visits only that column; callers keep at most one raw chunk pinned at a time.
pub trait CopyBatch: Send + Sync {
    fn count(&self) -> usize;
    fn scan(&self, column: Option<usize>, visit: &mut dyn FnMut(&DataChunk) -> Result<()>) -> Result<()>;
}
/// `prepare_batch`: encodes the rows of a batch (any thread, concurrently).
pub type CopyPrepareBatch =
    Arc<dyn Fn(&dyn FunctionData, &CopyGlobalState, &dyn CopyBatch) -> Result<PreparedBatch> + Send + Sync>;
/// `flush_batch`: writes a prepared batch (one thread at a time, in batch order).
pub type CopyFlushBatch =
    Arc<dyn Fn(&dyn FunctionData, &CopyGlobalState, PreparedBatch) -> Result<()> + Send + Sync>;
/// The state of one thread of a parallel write (DuckDB's `LocalFunctionData`).
pub type CopyLocalState = Box<dyn Any + Send>;
/// `copy_to_initialize_local`.
pub type CopyToInitializeLocal = Arc<dyn Fn(&dyn FunctionData) -> Result<CopyLocalState> + Send + Sync>;
/// `copy_to_sink` with the thread's local state (parallel writes).
pub type CopyToSinkLocal = Arc<
    dyn Fn(&dyn FunctionData, &CopyGlobalState, &mut CopyLocalState, &DataChunk) -> Result<()> + Send + Sync,
>;
/// `copy_to_combine`: hands a thread's remaining rows to the file.
pub type CopyToCombine =
    Arc<dyn Fn(&dyn FunctionData, &CopyGlobalState, CopyLocalState) -> Result<()> + Send + Sync>;
/// `desired_batch_size`: the rows per prepared batch (0: any).
pub type CopyDesiredBatchSize = Arc<dyn Fn(&dyn FunctionData) -> u64 + Send + Sync>;

/// What a `plan` callback sees (the bound query of `COPY ... TO`).
#[derive(Debug)]
pub struct CopyPlanInput<'a> {
    pub names: &'a [rustdb_common::ByteString],
    pub types: &'a [LogicalType],
    /// The options of the statement (the generic COPY options included).
    pub options: &'a CaseInsensitiveMap<Vec<Value>, rustdb_common::ByteString>,
}

/// How a `plan` callback rewrites `COPY ... TO`: as a COPY in `format` with `options` of
/// `SELECT <select_list> FROM (<query>)`, where `select_list` contains native expressions over positional
/// references (`#1`, ...) of the query's columns.
#[derive(Debug)]
pub struct CopyPlanRewrite {
    pub format: rustdb_common::ByteString,
    pub options: CaseInsensitiveMap<Vec<Value>, rustdb_common::ByteString>,
    pub select_list: Vec<rustdb_ast::ParsedExpression>,
}

/// `copy_to_plan_t`: rewrites a `COPY ... TO` into another COPY (DuckDB's `CopyFunction::plan`).
pub type CopyPlan = Arc<dyn Fn(&CopyPlanInput<'_>) -> Result<CopyPlanRewrite> + Send + Sync>;

/// A file format of COPY (DuckDB's `CopyFunction`).
#[derive(Clone)]
pub struct CopyFunction {
    pub name: Arc<str>,
    /// The default file extension.
    pub extension: String,
    /// The options the format accepts (`copy_options`).
    pub options: Vec<(String, CopyOption)>,
    /// Casts the written columns to types the format supports (`copy_to_select`).
    pub copy_to_select: Option<CopyToSelect>,
    pub copy_to_bind: Option<CopyToBind>,
    pub copy_to_initialize_global: Option<CopyToInitializeGlobal>,
    /// Gives a file's state the operator to log for (`initialize_operator`).
    pub copy_to_initialize_operator: Option<CopyToInitializeOperator>,
    pub copy_to_sink: Option<CopyToSink>,
    pub copy_to_finalize: Option<CopyToFinalize>,
    /// The statistics of a written file for `RETURN_STATS` (`copy_to_get_written_statistics`).
    pub copy_to_get_written_statistics: Option<CopyToGetWrittenStatistics>,
    /// Whether the format can split its output by size (`rotate_files`), and the size.
    pub copy_to_file_size: Option<CopyToFileSize>,
    /// Splits writes into several files (`rotate_files`); without it, writes rotate on
    /// `FILE_SIZE_BYTES` alone.
    pub rotate_files: Option<CopyRotateFiles>,
    /// When to start the next file (`rotate_next_file`).
    pub rotate_next_file: Option<CopyRotateNextFile>,
    pub copy_from_bind: Option<CopyFromBind>,
    pub copy_from_function: Option<TableFunction>,
    /// How writes run (`execution_mode`); regular without it.
    pub execution_mode: Option<CopyExecutionMode>,
    /// Batched writes (`prepare_batch`, `flush_batch`, `desired_batch_size`).
    pub prepare_batch: Option<CopyPrepareBatch>,
    pub flush_batch: Option<CopyFlushBatch>,
    pub desired_batch_size: Option<CopyDesiredBatchSize>,
    /// Parallel writes with thread-local state (`copy_to_initialize_local`,
    /// `copy_to_sink`, `copy_to_combine`).
    pub copy_to_initialize_local: Option<CopyToInitializeLocal>,
    pub copy_to_sink_local: Option<CopyToSinkLocal>,
    pub copy_to_combine: Option<CopyToCombine>,
    /// Rewrites `COPY ... TO` into another COPY (`plan`); the format then has no option
    /// list of its own.
    pub plan: Option<CopyPlan>,
    /// Writes the bind data of a `COPY ... TO`, for serialized plans (`serialize`).
    pub serialize: Option<CopySerialize>,
}

/// See [`CopyFunction::serialize`] (DuckDB's `copy_to_serialize_t`).
pub type CopySerialize = Arc<
    dyn Fn(&mut dyn rustdb_serde::Serializer, &dyn FunctionData, &CopyFunction) -> Result<()> + Send + Sync,
>;

impl CopyFunction {
    pub fn new(name: impl Into<Arc<str>>) -> Self {
        Self {
            name: name.into(),
            extension: String::new(),
            options: Vec::new(),
            copy_to_select: None,
            copy_to_bind: None,
            copy_to_initialize_global: None,
            copy_to_initialize_operator: None,
            copy_to_sink: None,
            copy_to_finalize: None,
            copy_to_get_written_statistics: None,
            copy_to_file_size: None,
            rotate_files: None,
            rotate_next_file: None,
            copy_from_bind: None,
            copy_from_function: None,
            execution_mode: None,
            prepare_batch: None,
            flush_batch: None,
            desired_batch_size: None,
            copy_to_initialize_local: None,
            copy_to_sink_local: None,
            copy_to_combine: None,
            plan: None,
            serialize: None,
        }
    }
}

impl fmt::Debug for CopyFunction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CopyFunction")
            .field("name", &self.name)
            .field("extension", &self.extension)
            .finish_non_exhaustive()
    }
}
