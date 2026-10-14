//! Dividing memory between concurrently running operators (DuckDB's
//! `TemporaryMemoryManager`).
//!
//! Operators that may go out of core (sort, hash join, hash aggregate, window) register a
//! [`TemporaryMemoryState`] and report how much memory they would like ("remaining size").
//! The manager hands out reservations so that the sum stays below 90% of the memory limit,
//! minimising a cost model of materialisation when it is oversubscribed.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use crate::DEFAULT_BLOCK_ALLOC_SIZE;

/// 512 blocks per state per thread (0.125 GiB per thread).
const MINIMUM_RESERVATION_PER_STATE_PER_THREAD: u64 = 512 * DEFAULT_BLOCK_ALLOC_SIZE as u64;
/// Or 1/16th of the memory limit, whichever is smaller.
const MINIMUM_RESERVATION_MEMORY_LIMIT_DIVISOR: u64 = 16;
/// Share of the memory limit managed here.
const MAXIMUM_MEMORY_LIMIT_RATIO: f64 = 0.9;
/// Share of the free memory a single state may get.
const MAXIMUM_FREE_MEMORY_RATIO: f64 = 0.9;
/// Room left for between this many minimum reservations of other states.
const MINIMUM_REMAINING_STATE_RESERVATIONS: u64 = 8;
const MAXIMUM_REMAINING_STATE_RESERVATIONS: u64 = 32;
const OPTIMIZATION_ITERATIONS_MULTIPLIER: usize = 5;

/// The query-level facts the manager needs (DuckDB reads them from the `ClientContext`).
#[derive(Clone, Copy, Debug)]
pub struct TemporaryMemoryContext {
    /// The buffer manager's memory limit.
    pub memory_limit: u64,
    /// The per-query memory limit.
    pub query_max_memory: u64,
    /// Whether data can be offloaded to a temporary directory.
    pub has_temporary_directory: bool,
    /// Number of worker threads.
    pub threads: u64,
    /// Number of open connections.
    pub connections: u64,
    /// `SET force_external`: always give states their minimum.
    pub force_external: bool,
}

#[derive(Debug, Default)]
struct StateValues {
    remaining_size: AtomicU64,
    minimum_reservation: AtomicU64,
    reservation: AtomicU64,
    materialization_penalty: AtomicU64,
}

#[derive(Debug, Default)]
struct ManagerState {
    memory_limit: u64,
    has_temporary_directory: bool,
    threads: u64,
    connections: u64,
    query_max_memory: u64,
    active: Vec<Weak<StateValues>>,
    reservation: u64,
    remaining_size: u64,
}

/// Assigns memory reservations to [`TemporaryMemoryState`]s.
#[derive(Debug, Default)]
pub struct TemporaryMemoryManager {
    state: Arc<Mutex<ManagerState>>,
}

/// One operator's claim on temporary memory; unregisters itself when dropped.
#[derive(Debug)]
pub struct TemporaryMemoryState {
    manager: Arc<Mutex<ManagerState>>,
    values: Arc<StateValues>,
}

fn lock(state: &Mutex<ManagerState>) -> MutexGuard<'_, ManagerState> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

impl ManagerState {
    fn configure(&mut self, context: &TemporaryMemoryContext) {
        self.memory_limit = (MAXIMUM_MEMORY_LIMIT_RATIO * context.memory_limit as f64) as u64;
        self.has_temporary_directory = context.has_temporary_directory;
        self.threads = context.threads;
        self.connections = context.connections;
        self.query_max_memory = context.query_max_memory;
    }

    fn default_minimum_reservation(&self) -> u64 {
        (self
            .threads
            .saturating_mul(MINIMUM_RESERVATION_PER_STATE_PER_THREAD))
        .min(self.memory_limit / MINIMUM_RESERVATION_MEMORY_LIMIT_DIVISOR)
    }

    fn set_remaining_size(&mut self, state: &StateValues, size: u64) {
        self.remaining_size -= state.remaining_size.load(Ordering::Relaxed);
        state.remaining_size.store(size, Ordering::Relaxed);
        self.remaining_size += size;
    }

    fn set_reservation(&mut self, state: &StateValues, reservation: u64) {
        self.reservation -= state.reservation.load(Ordering::Relaxed);
        state.reservation.store(reservation, Ordering::Relaxed);
        self.reservation += reservation;
    }

    fn active_states(&self) -> Vec<Arc<StateValues>> {
        self.active.iter().filter_map(Weak::upgrade).collect()
    }

    /// `TemporaryMemoryManager::UpdateState`.
    fn update_state(&mut self, context: &TemporaryMemoryContext, state: &Arc<StateValues>) {
        self.configure(context);
        let remaining = state.remaining_size.load(Ordering::Relaxed);
        let current = state.reservation.load(Ordering::Relaxed);
        let lower_bound = state.minimum_reservation.load(Ordering::Relaxed).min(remaining);
        let reservation = if remaining == 0 {
            0
        } else if context.force_external {
            lower_bound
        } else if !self.has_temporary_directory {
            // Cannot offload, so memory usage cannot be limited.
            remaining
        } else if self.reservation - current + lower_bound >= self.memory_limit {
            lower_bound
        } else {
            let free_memory = self.memory_limit - (self.reservation - current);
            let upper_bound = remaining
                .min(self.query_max_memory)
                .min((MAXIMUM_FREE_MEMORY_RATIO * free_memory as f64) as u64)
                .min(free_memory);
            if lower_bound >= upper_bound {
                lower_bound
            } else if self.remaining_size > self.memory_limit {
                self.compute_reservation(state)
            } else {
                upper_bound
            }
        };
        self.set_reservation(state, reservation);
    }

    /// Distributes free memory by gradient descent on a materialisation cost model
    /// (`TemporaryMemoryManager::ComputeReservation`).
    fn compute_reservation(&self, target: &Arc<StateValues>) -> u64 {
        let initial = |state: &StateValues| {
            let reservation = state
                .minimum_reservation
                .load(Ordering::Relaxed)
                .max(state.reservation.load(Ordering::Relaxed));
            reservation
                .min(state.remaining_size.load(Ordering::Relaxed))
                .max(1)
        };
        let remaining = |state: &StateValues| state.remaining_size.load(Ordering::Relaxed);
        let mut states = Vec::new();
        let mut res = Vec::new();
        let mut target_index = None;
        let mut sum_of_initial = 0u64;
        for state in self.active_states() {
            let initial_reservation = initial(&state);
            sum_of_initial += initial_reservation;
            if Arc::ptr_eq(&state, target) {
                target_index = Some(states.len());
            } else if initial_reservation >= remaining(&state) {
                continue;
            }
            states.push(state);
            res.push(initial_reservation);
        }
        let Some(target_index) = target_index else {
            return initial(target);
        };
        if sum_of_initial >= self.memory_limit {
            return res[target_index];
        }
        let n = states.len();
        let free_memory = self.memory_limit - sum_of_initial;
        let mut der = vec![f64::MAX; n];

        let mut remaining_memory = free_memory;
        let iterations = OPTIMIZATION_ITERATIONS_MULTIPLIER * n;
        let mut opt_idx = 0;
        while opt_idx < iterations && remaining_memory > 0 {
            compute_derivatives(&states, &res, &mut der);
            let candidate = (0..n)
                .filter(|&i| res[i] < remaining(&states[i]))
                .min_by(|&a, &b| der[a].total_cmp(&der[b]));
            let Some(min_idx) = candidate else {
                break;
            };
            let iter_memory = (remaining_memory as f64 / (iterations - opt_idx) as f64).ceil() as u64;
            let state_room = remaining(&states[min_idx]) - res[min_idx];
            let delta = iter_memory.min(state_room);
            res[min_idx] += delta;
            remaining_memory -= delta;
            // If the last round could not hand out everything, run one more.
            if !(opt_idx == iterations - 1 && delta < iter_memory) {
                opt_idx += 1;
            }
        }

        // Bound reservations by the free-memory ratio, assigning in derivative order so
        // the order in which states call this matters less.
        let max_res: Vec<u64> = states
            .iter()
            .map(|state| remaining(state).saturating_sub(1))
            .collect();
        compute_derivatives(&states, &max_res, &mut der);
        let mut order: Vec<usize> = (0..n).collect();
        order.sort_by(|&a, &b| der[a].total_cmp(&der[b]));

        let mut remaining_memory = free_memory;
        let other_states = self.connections.clamp(
            MINIMUM_REMAINING_STATE_RESERVATIONS,
            MAXIMUM_REMAINING_STATE_RESERVATIONS,
        );
        for idx in order {
            let initial_reservation = initial(&states[idx]);
            let state_remaining = initial_reservation + remaining_memory;
            let upper_bound = ((MAXIMUM_FREE_MEMORY_RATIO * state_remaining as f64) as u64)
                .min(other_states * self.default_minimum_reservation());
            let reservation = res[idx].min(upper_bound).max(initial_reservation);
            if idx == target_index {
                return reservation;
            }
            remaining_memory = remaining_memory.saturating_sub(reservation - initial_reservation);
        }
        initial(target)
    }
}

/// Derivatives of the cost function `(Σ (1 - throughput_i) · penalty_i) · (1 - geomean
/// throughput)` with respect to each reservation.
fn compute_derivatives(states: &[Arc<StateValues>], res: &[u64], der: &mut [f64]) {
    let n = states.len();
    let size = |state: &StateValues| state.remaining_size.load(Ordering::Relaxed).max(1) as f64;
    let penalty = |state: &StateValues| state.materialization_penalty.load(Ordering::Relaxed) as f64;
    let mut log_throughput_prod = 0.0;
    let mut mat_cost = 0.0;
    for (state, &reservation) in states.iter().zip(res) {
        let throughput = reservation as f64 / size(state);
        log_throughput_prod += throughput.ln();
        mat_cost += penalty(state) * (1.0 - throughput);
    }
    let nd = n as f64;
    let geomean = (log_throughput_prod / nd).exp();
    let tp_mult = 1.0 - geomean;
    let intermediate = -(geomean * mat_cost) / nd;
    for (i, state) in states.iter().enumerate() {
        der[i] = intermediate / res[i] as f64 - penalty(state) * tp_mult / size(state);
    }
}

impl TemporaryMemoryManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a new state with the default minimum reservation
    /// (`TemporaryMemoryManager::Register`).
    pub fn register(&self, context: &TemporaryMemoryContext) -> TemporaryMemoryState {
        let mut state = lock(&self.state);
        state.configure(context);
        let minimum = state.default_minimum_reservation();
        let values = Arc::new(StateValues {
            materialization_penalty: AtomicU64::new(1),
            minimum_reservation: AtomicU64::new(minimum),
            ..StateValues::default()
        });
        state.set_remaining_size(&values, minimum);
        state.set_reservation(&values, minimum);
        state.active.retain(|weak| weak.strong_count() > 0);
        state.active.push(Arc::downgrade(&values));
        TemporaryMemoryState {
            manager: Arc::clone(&self.state),
            values,
        }
    }

    /// Sum of all reservations.
    pub fn total_reservation(&self) -> u64 {
        lock(&self.state).reservation
    }
}

impl TemporaryMemoryState {
    /// Sets the size this state would need to stay in memory, without updating the
    /// reservation.
    pub fn set_remaining_size(&self, size: u64) {
        let mut manager = lock(&self.manager);
        manager.set_remaining_size(&self.values, size);
    }

    /// Sets the needed size and recomputes the reservation.
    pub fn set_remaining_size_and_update_reservation(&self, context: &TemporaryMemoryContext, size: u64) {
        let mut manager = lock(&self.manager);
        manager.set_remaining_size(&self.values, size);
        manager.update_state(context, &self.values);
    }

    /// Recomputes the reservation for the current remaining size.
    pub fn update_reservation(&self, context: &TemporaryMemoryContext) {
        let mut manager = lock(&self.manager);
        manager.update_state(context, &self.values);
    }

    /// Releases the reservation (the operator is done) without unregistering.
    pub fn set_zero(&self) {
        let mut manager = lock(&self.manager);
        manager.set_remaining_size(&self.values, 0);
        manager.set_reservation(&self.values, 0);
    }

    pub fn remaining_size(&self) -> u64 {
        self.values.remaining_size.load(Ordering::Relaxed)
    }

    pub fn reservation(&self) -> u64 {
        self.values.reservation.load(Ordering::Relaxed)
    }

    pub fn minimum_reservation(&self) -> u64 {
        self.values.minimum_reservation.load(Ordering::Relaxed)
    }

    pub fn set_minimum_reservation(&self, minimum: u64) {
        self.values.minimum_reservation.store(minimum, Ordering::Relaxed);
    }

    /// Weight of this state in the cost model (how expensive materialising it is).
    pub fn set_materialization_penalty(&self, penalty: u64) {
        let _manager = lock(&self.manager);
        self.values
            .materialization_penalty
            .store(penalty, Ordering::Relaxed);
    }

    pub fn materialization_penalty(&self) -> u64 {
        self.values.materialization_penalty.load(Ordering::Relaxed)
    }
}

impl Drop for TemporaryMemoryState {
    fn drop(&mut self) {
        let mut manager = lock(&self.manager);
        manager.set_reservation(&self.values, 0);
        manager.set_remaining_size(&self.values, 0);
        let values = Arc::as_ptr(&self.values);
        manager
            .active
            .retain(|weak| weak.strong_count() > 0 && !std::ptr::eq(weak.as_ptr(), values));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context(memory_limit: u64) -> TemporaryMemoryContext {
        TemporaryMemoryContext {
            memory_limit,
            query_max_memory: memory_limit,
            has_temporary_directory: true,
            threads: 4,
            connections: 1,
            force_external: false,
        }
    }

    #[test]
    fn single_state_gets_what_fits() {
        let manager = TemporaryMemoryManager::new();
        let ctx = context(1 << 30);
        let state = manager.register(&ctx);
        // min(4 * 128 MiB, 0.9 GiB / 16)
        let minimum = ((0.9 * (1u64 << 30) as f64) as u64) / 16;
        assert_eq!(state.reservation(), minimum);
        state.set_remaining_size_and_update_reservation(&ctx, 100 << 20);
        assert_eq!(state.reservation(), 100 << 20);
        // Asking for more than the limit is bounded by 90% of the free memory.
        state.set_remaining_size_and_update_reservation(&ctx, 10 << 30);
        assert!(state.reservation() < (1 << 30));
        assert!(state.reservation() > minimum);
        drop(state);
        assert_eq!(manager.total_reservation(), 0);
    }

    #[test]
    fn oversubscribed_states_share_memory() {
        let manager = TemporaryMemoryManager::new();
        let ctx = context(1 << 30);
        let a = manager.register(&ctx);
        let b = manager.register(&ctx);
        a.set_remaining_size_and_update_reservation(&ctx, 4 << 30);
        b.set_remaining_size_and_update_reservation(&ctx, 4 << 30);
        a.update_reservation(&ctx);
        let limit = (0.9 * (1u64 << 30) as f64) as u64;
        assert!(a.reservation() + b.reservation() <= limit + 1);
        assert!(a.reservation() >= a.minimum_reservation());
        assert!(b.reservation() >= b.minimum_reservation());
    }

    #[test]
    fn without_temp_directory_reservation_is_the_remaining_size() {
        let manager = TemporaryMemoryManager::new();
        let ctx = TemporaryMemoryContext {
            has_temporary_directory: false,
            ..context(1 << 20)
        };
        let state = manager.register(&ctx);
        state.set_remaining_size_and_update_reservation(&ctx, 1 << 30);
        assert_eq!(state.reservation(), 1 << 30);
        state.set_zero();
        assert_eq!(manager.total_reservation(), 0);
    }
}
