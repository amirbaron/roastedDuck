/// Per-thread execution context handed to every operator call.
#[derive(Debug, Clone, Default)]
pub struct ExecutionContext {
    /// Index of the worker thread running the current task.
    pub thread_index: usize,
}
