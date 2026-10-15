use std::sync::Arc;

use rustdb_buffer::{BufferManager, BufferManagerConfig};
use rustdb_common::Result;

/// Per-query resources shared by every operator (the parts of DuckDB's `ClientContext`
/// and `DatabaseInstance` that execution needs).
#[derive(Debug, Clone)]
pub struct ClientEnv {
    /// The database's buffer manager; operators allocate buffer-managed memory from it.
    pub buffer_manager: BufferManager,
    /// The `threads` setting: how many workers may run a pipeline.
    pub threads: usize,
}

impl ClientEnv {
    /// An environment with its own buffer manager using DuckDB's defaults (for tests and
    /// standalone use).
    pub fn standalone() -> Result<Arc<Self>> {
        let threads = std::thread::available_parallelism().map_or(1, usize::from);
        Ok(Arc::new(Self {
            buffer_manager: BufferManager::new(BufferManagerConfig::default())?,
            threads,
        }))
    }
}

/// Per-thread execution context handed to every operator call.
#[derive(Debug, Clone)]
pub struct ExecutionContext {
    pub env: Arc<ClientEnv>,
    /// Index of the worker thread running the current task.
    pub thread_index: usize,
}

impl ExecutionContext {
    pub fn new(env: Arc<ClientEnv>) -> Self {
        Self { env, thread_index: 0 }
    }
}
