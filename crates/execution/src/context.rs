use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use rustdb_buffer::{BufferManager, BufferManagerConfig};
use rustdb_common::{Error, ErrorKind, Result};

use crate::parallel::TaskScheduler;

/// Per-database resources shared by every query (the parts of DuckDB's `ClientContext`
/// and `DatabaseInstance` that execution needs).
#[derive(Debug)]
pub struct ClientEnv {
    /// The database's buffer manager; operators allocate buffer-managed memory from it.
    pub buffer_manager: BufferManager,
    /// The thread pool running queries; its size is the `threads` setting.
    pub scheduler: TaskScheduler,
}

impl ClientEnv {
    /// An environment with its own buffer manager and one thread per core, DuckDB's
    /// defaults (for tests and standalone use).
    pub fn standalone() -> Result<Arc<Self>> {
        Ok(Arc::new(Self::new(BufferManager::new(
            BufferManagerConfig::default(),
        )?)))
    }

    /// An environment over `buffer_manager` with one thread per core.
    pub fn new(buffer_manager: BufferManager) -> Self {
        Self {
            buffer_manager,
            scheduler: TaskScheduler::new(default_threads()),
        }
    }

    /// The `threads` setting: how many threads may work on one query.
    pub fn threads(&self) -> usize {
        self.scheduler.number_of_threads()
    }
}

/// The default `threads` setting: the number of hardware threads.
pub fn default_threads() -> usize {
    std::thread::available_parallelism().map_or(1, usize::from)
}

/// Per-thread execution context handed to every operator call (DuckDB's
/// `ExecutionContext` with its `ThreadContext`).
#[derive(Debug, Clone)]
pub struct ExecutionContext {
    pub env: Arc<ClientEnv>,
    /// Index of the task running the current pipeline.
    pub thread_index: usize,
    /// Collects per-operator rows and timings under `EXPLAIN ANALYZE`.
    pub profiler: Option<Arc<crate::QueryProfiler>>,
    /// Set when the query fails on another thread: work stops at the next chunk (DuckDB's
    /// `ClientContext::interrupted`).
    interrupted: Arc<AtomicBool>,
}

impl ExecutionContext {
    pub fn new(env: Arc<ClientEnv>) -> Self {
        Self {
            env,
            thread_index: 0,
            profiler: None,
            interrupted: Arc::default(),
        }
    }

    /// A context for one task of a query whose interruption flag is `interrupted`.
    pub(crate) fn for_task(
        env: Arc<ClientEnv>,
        profiler: Option<Arc<crate::QueryProfiler>>,
        interrupted: Arc<AtomicBool>,
        thread_index: usize,
    ) -> Self {
        Self {
            env,
            thread_index,
            profiler,
            interrupted,
        }
    }

    /// Fails with DuckDB's `InterruptException` once the query was interrupted.
    pub fn check_interrupted(&self) -> Result<()> {
        if self.interrupted.load(Ordering::Relaxed) {
            return Err(Error::new(ErrorKind::Interrupt, "Interrupted!"));
        }
        Ok(())
    }
}
