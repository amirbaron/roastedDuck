//! Waking operators that blocked (DuckDB's `InterruptState`, `parallel/interrupt.cpp`).
//!
//! An operator that cannot continue (a streaming result collector whose buffer is full)
//! keeps the interrupt state of its task and returns `Blocked`; the task is descheduled.
//! Once the operator can continue, [`InterruptState::callback`] reschedules the task.

use std::sync::Weak;

use super::executor::ExecutorShared;

/// Who to wake when a blocked operator can continue (`InterruptState`).
#[derive(Clone, Debug, Default)]
pub struct InterruptState {
    mode: InterruptMode,
}

#[derive(Clone, Debug, Default)]
enum InterruptMode {
    /// The operator is not run by a task and must not block (`NO_INTERRUPTS`).
    #[default]
    NoInterrupts,
    /// Reschedule task `task_id` of the executor (`TASK`); a no-op once the query ended.
    Task {
        executor: Weak<ExecutorShared>,
        task_id: u64,
    },
}

impl InterruptState {
    /// The state of task `task_id` of `executor`.
    pub(crate) fn task(executor: Weak<ExecutorShared>, task_id: u64) -> Self {
        Self {
            mode: InterruptMode::Task { executor, task_id },
        }
    }

    /// Whether an operator may block with this state.
    pub fn can_block(&self) -> bool {
        matches!(self.mode, InterruptMode::Task { .. })
    }

    /// Wakes the query thread waiting for tasks: a streaming result has a chunk to read.
    pub fn wake_query_thread(&self) {
        if let InterruptMode::Task { executor, .. } = &self.mode
            && let Some(executor) = executor.upgrade()
        {
            executor.wake_query_thread();
        }
    }

    /// Reschedules the blocked task (`Callback`).
    pub fn callback(&self) {
        match &self.mode {
            InterruptMode::Task { executor, task_id } => {
                if let Some(executor) = executor.upgrade() {
                    executor.reschedule_task(*task_id);
                }
            }
            InterruptMode::NoInterrupts => {
                debug_assert!(
                    false,
                    "Callback made on InterruptState without valid interrupt mode specified"
                );
            }
        }
    }
}
