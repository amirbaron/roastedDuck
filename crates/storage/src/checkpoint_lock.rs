//! The checkpoint lock of a database (DuckDB's `StorageLock` behind
//! `DuckTransactionManager::checkpoint_lock`): transactions that write to the database
//! hold it shared until they end; a checkpoint that replaces the in-memory table data
//! with the checkpointed data needs it exclusively.

use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

#[derive(Debug, Default)]
struct LockState {
    shared: usize,
    exclusive: bool,
}

/// A shared/exclusive lock whose guards own a handle to it.
#[derive(Debug, Default)]
pub struct CheckpointLock {
    state: Mutex<LockState>,
    released: Condvar,
}

impl CheckpointLock {
    fn lock(&self) -> MutexGuard<'_, LockState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Takes the lock shared, waiting while a checkpoint holds it (`GetSharedLock`).
    pub fn shared(self: &Arc<Self>) -> SharedCheckpointGuard {
        let mut state = self.lock();
        while state.exclusive {
            state = self.released.wait(state).unwrap_or_else(PoisonError::into_inner);
        }
        state.shared += 1;
        SharedCheckpointGuard(Arc::clone(self))
    }

    /// Takes the lock exclusively if nobody holds it (`TryGetExclusiveLock`).
    pub fn try_exclusive(self: &Arc<Self>) -> Option<ExclusiveCheckpointGuard> {
        let mut state = self.lock();
        if state.exclusive || state.shared > 0 {
            return None;
        }
        state.exclusive = true;
        Some(ExclusiveCheckpointGuard(Arc::clone(self)))
    }

    /// Takes the lock exclusively, waiting until every holder released it (the wait
    /// loop of `FORCE CHECKPOINT` in `DuckTransactionManager::Checkpoint`).
    pub fn exclusive(self: &Arc<Self>) -> ExclusiveCheckpointGuard {
        let mut state = self.lock();
        while state.exclusive || state.shared > 0 {
            state = self.released.wait(state).unwrap_or_else(PoisonError::into_inner);
        }
        state.exclusive = true;
        ExclusiveCheckpointGuard(Arc::clone(self))
    }
}

/// A shared hold of a [`CheckpointLock`], released on drop.
#[derive(Debug)]
pub struct SharedCheckpointGuard(Arc<CheckpointLock>);

impl Drop for SharedCheckpointGuard {
    fn drop(&mut self) {
        let mut state = self.0.lock();
        state.shared = state.shared.saturating_sub(1);
        if state.shared == 0 {
            self.0.released.notify_all();
        }
    }
}

/// The exclusive hold of a [`CheckpointLock`], released on drop.
#[derive(Debug)]
pub struct ExclusiveCheckpointGuard(Arc<CheckpointLock>);

impl Drop for ExclusiveCheckpointGuard {
    fn drop(&mut self) {
        self.0.lock().exclusive = false;
        self.0.released.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exclusive_excludes_shared_holders() {
        let lock = Arc::new(CheckpointLock::default());
        let shared = lock.shared();
        assert!(lock.try_exclusive().is_none());
        drop(shared);
        let exclusive = lock.try_exclusive();
        assert!(exclusive.is_some());
        assert!(lock.try_exclusive().is_none());
        drop(exclusive);
        let _shared = lock.shared();
    }
}
