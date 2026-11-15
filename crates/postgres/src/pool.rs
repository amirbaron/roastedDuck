//! Engine-owned PostgreSQL session leases, matching the pinned dbconnector pool.
use crate::Session;
use roastedduck_common::{Error, ErrorKind, Result};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::sync::{
    Arc, Condvar, Mutex, PoisonError, Weak,
    atomic::{AtomicBool, Ordering},
};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AcquireMode {
    Force,
    Wait,
    Try,
}
impl AcquireMode {
    pub fn parse(value: &str) -> Result<Self> {
        if value.eq_ignore_ascii_case("force") {
            Ok(Self::Force)
        } else if value.eq_ignore_ascii_case("wait") {
            Ok(Self::Wait)
        } else if value.eq_ignore_ascii_case("try") {
            Ok(Self::Try)
        } else {
            Err(pool_error(format!("Invalid unsupported acquire mode: '{value}'")))
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Force => "force",
            Self::Wait => "wait",
            Self::Try => "try",
        }
    }
}
#[derive(Clone, Debug)]
pub struct Config {
    pub acquire_mode: AcquireMode,
    pub max_connections: u64,
    pub wait_timeout_millis: u64,
    pub thread_local_cache: bool,
    pub max_lifetime_millis: u64,
    pub idle_timeout_millis: u64,
    pub reaper: bool,
    pub health_check_query: String,
}
impl Default for Config {
    fn default() -> Self {
        let hardware = std::thread::available_parallelism().map_or(0, std::num::NonZeroUsize::get) as u64;
        Self {
            acquire_mode: AcquireMode::Force,
            max_connections: (hardware * 3 / 2).clamp(4, 32),
            wait_timeout_millis: 30000,
            thread_local_cache: false,
            max_lifetime_millis: 0,
            idle_timeout_millis: 60000,
            reaper: true,
            health_check_query: "SELECT 1".into(),
        }
    }
}
impl Config {
    fn now(&self) -> Option<Instant> {
        (self.max_lifetime_millis > 0 || self.idle_timeout_millis > 0).then(Instant::now)
    }
    pub fn reaper_period_millis(&self) -> u64 {
        let lifetime = self.max_lifetime_millis / 1000;
        let idle = self.idle_timeout_millis / 1000;
        let seconds = match (lifetime, idle) {
            (0, 0) => 30,
            (0, n) | (n, 0) => n,
            (a, b) => a.min(b),
        };
        (seconds / 2).clamp(1, 60) * 1000
    }
}
#[derive(Clone, Debug, Default)]
pub struct Metrics {
    pub total_connections: u64,
    pub available_connections: u64,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub try_failures: u64,
    pub thread_local_cache_hits: u64,
    pub thread_local_cache_misses: u64,
    pub reaper_running: bool,
}
#[derive(Debug)]
struct Cached {
    session: Session,
    created: Option<Instant>,
    returned: Option<Instant>,
}
impl Cached {
    fn expired(&self, config: &Config, now: Option<Instant>) -> bool {
        expired(self.created, config.max_lifetime_millis, now)
            || expired(self.returned, config.idle_timeout_millis, now)
    }
}
#[derive(Debug)]
struct State {
    config: Config,
    metrics: Metrics,
    available: VecDeque<Cached>,
    shutdown: bool,
    generation: u64,
}
#[derive(Debug)]
struct Shared {
    state: Mutex<State>,
    available: Condvar,
    reaper: Condvar,
}
type Factory = Arc<dyn Fn() -> Result<Session> + Send + Sync>;
pub struct Pool {
    shared: Arc<Shared>,
    factory: Factory,
    reaper: Mutex<Option<JoinHandle<()>>>,
}
impl std::fmt::Debug for Pool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PostgresPool")
            .field("metrics", &self.snapshot().1)
            .finish_non_exhaustive()
    }
}
#[derive(Debug)]
pub struct Lease {
    pool: Option<Arc<Pool>>,
    connection: Option<Cached>,
}
impl Lease {
    pub fn session(&self) -> Result<&Session> {
        self.connection
            .as_ref()
            .map(|c| &c.session)
            .ok_or_else(|| pool_error("PooledConnection::GetConnection - no connection available"))
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        if let (Some(pool), Some(connection)) = (self.pool.take(), self.connection.take()) {
            pool.give_back(connection);
        }
    }
}
#[derive(Default)]
struct ThreadCache {
    owner: Weak<Pool>,
    connection: Option<Cached>,
}
impl Drop for ThreadCache {
    fn drop(&mut self) {
        if let (Some(pool), Some(connection)) = (self.owner.upgrade(), self.connection.take()) {
            pool.return_cached(connection, true);
        }
    }
}
thread_local! { static THREAD_CACHE: RefCell<ThreadCache> = RefCell::default(); }

impl Pool {
    pub fn new(config: Config, factory: Factory) -> Result<Arc<Self>> {
        let start = config.reaper;
        let result = Arc::new(Self {
            shared: Arc::new(Shared {
                state: Mutex::new(State {
                    config,
                    metrics: Metrics::default(),
                    available: VecDeque::new(),
                    shutdown: false,
                    generation: 0,
                }),
                available: Condvar::new(),
                reaper: Condvar::new(),
            }),
            factory,
            reaper: Mutex::default(),
        });
        if start {
            result.set_reaper(true)?;
        }
        Ok(result)
    }
    pub fn snapshot(&self) -> (Config, Metrics) {
        let state = self.shared.state.lock().unwrap_or_else(PoisonError::into_inner);
        let mut metrics = state.metrics.clone();
        metrics.available_connections = state.available.len() as u64;
        (state.config.clone(), metrics)
    }
    pub fn acquire(self: &Arc<Self>, interrupted: Option<&AtomicBool>) -> Result<Lease> {
        let config = self.snapshot().0;
        let mode = if config.max_connections == 0 {
            AcquireMode::Force
        } else {
            config.acquire_mode
        };
        self.acquire_mode(mode, interrupted)?
            .ok_or_else(|| pool_error("PooledConnection::GetConnection - no connection available"))
    }
    pub fn try_acquire(self: &Arc<Self>) -> Result<Option<Lease>> {
        self.acquire_mode(AcquireMode::Try, None)
    }
    fn acquire_mode(
        self: &Arc<Self>,
        mode: AcquireMode,
        interrupted: Option<&AtomicBool>,
    ) -> Result<Option<Lease>> {
        let initial = self.snapshot().0;
        if mode != AcquireMode::Force && initial.max_connections == 0 {
            return Err(pool_error(
                "Connection pool is disabled (pool_size=0). Use the force acquire mode to create connections without pooling.",
            ));
        }
        let mut created_at = initial.now();
        if let Some(connection) = self.take_thread_local(&initial) {
            return Ok(Some(Lease {
                pool: Some(self.clone()),
                connection: Some(connection),
            }));
        }
        let deadline = Instant::now().checked_add(Duration::from_millis(initial.wait_timeout_millis));
        loop {
            if interrupted.is_some_and(|f| f.load(Ordering::Relaxed)) {
                return Err(Error::new(ErrorKind::Interrupt, "Interrupted!"));
            }
            let mut state = self.shared.state.lock().unwrap_or_else(PoisonError::into_inner);
            if state.shutdown {
                return if mode == AcquireMode::Try {
                    Ok(None)
                } else {
                    Err(pool_error("Connection pool has been shut down"))
                };
            }
            let config = state.config.clone();
            if let Some(connection) = state.available.pop_front() {
                drop(state);
                if mode == AcquireMode::Wait {
                    created_at = config.now();
                }
                let healthy = !connection.expired(&config, created_at)
                    && connection
                        .session
                        .ping(&config.health_check_query)
                        .unwrap_or(false);
                let mut state = self.shared.state.lock().unwrap_or_else(PoisonError::into_inner);
                if healthy && !state.shutdown {
                    state.metrics.cache_hits += 1;
                    drop(state);
                    return Ok(Some(Lease {
                        pool: Some(self.clone()),
                        connection: Some(connection),
                    }));
                }
                state.metrics.total_connections = state.metrics.total_connections.saturating_sub(1);
                drop(state);
                drop(connection);
                self.shared.available.notify_all();
                continue;
            }
            if mode == AcquireMode::Force || state.metrics.total_connections < config.max_connections {
                let tracked = config.max_connections != 0;
                if tracked {
                    state.metrics.total_connections += 1;
                }
                state.metrics.cache_misses += 1;
                drop(state);
                match (self.factory)() {
                    Ok(session) => {
                        return Ok(Some(Lease {
                            pool: tracked.then(|| self.clone()),
                            connection: Some(Cached {
                                session,
                                created: created_at,
                                returned: None,
                            }),
                        }));
                    }
                    Err(error) => {
                        if tracked {
                            self.discard();
                        }
                        return Err(error);
                    }
                }
            }
            if mode == AcquireMode::Try {
                state.metrics.try_failures += 1;
                return Ok(None);
            }
            let remaining = deadline.map_or(Duration::from_millis(20), |d| {
                d.saturating_duration_since(Instant::now())
            });
            if remaining.is_zero() {
                return Err(pool_error(format!(
                    "Connection pool timeout: all {} connections in use, waited {}ms",
                    state.config.max_connections, state.config.wait_timeout_millis
                )));
            }
            drop(
                self.shared
                    .available
                    .wait_timeout(state, remaining.min(Duration::from_millis(20)))
                    .unwrap_or_else(PoisonError::into_inner),
            );
        }
    }
    fn take_thread_local(self: &Arc<Self>, config: &Config) -> Option<Cached> {
        if !config.thread_local_cache {
            return None;
        }
        let candidate = THREAD_CACHE.with(|slot| {
            let mut slot = slot.borrow_mut();
            if slot.owner.upgrade().is_none() {
                slot.connection = None;
            }
            if slot
                .owner
                .upgrade()
                .is_some_and(|owner| Arc::ptr_eq(&owner, self))
            {
                slot.connection.take()
            } else {
                None
            }
        });
        if let Some(connection) = candidate {
            if !connection.expired(config, config.now())
                && connection
                    .session
                    .ping(&config.health_check_query)
                    .unwrap_or(false)
            {
                let mut state = self.shared.state.lock().unwrap_or_else(PoisonError::into_inner);
                state.metrics.thread_local_cache_hits += 1;
                state.metrics.cache_hits += 1;
                return Some(connection);
            }
            self.discard();
        }
        self.shared
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .metrics
            .thread_local_cache_misses += 1;
        None
    }
    fn give_back(self: &Arc<Self>, mut connection: Cached) {
        let config = self.snapshot().0;
        let now = config.now();
        if expired(connection.created, config.max_lifetime_millis, now)
            || !connection
                .session
                .ping(&config.health_check_query)
                .unwrap_or(false)
            || connection
                .session
                .reset_for_pool(&config.health_check_query)
                .is_err()
        {
            self.discard();
            return;
        }
        connection.returned = now;
        let mut pending = Some(connection);
        if self.snapshot().0.thread_local_cache {
            THREAD_CACHE.with(|slot| {
                let mut slot = slot.borrow_mut();
                if slot
                    .owner
                    .upgrade()
                    .is_some_and(|owner| !Arc::ptr_eq(&owner, self))
                    || slot.connection.is_some()
                {
                    return;
                }
                let state = self.shared.state.lock().unwrap_or_else(PoisonError::into_inner);
                if state.shutdown
                    || state.metrics.total_connections >= state.config.max_connections
                        && state.available.is_empty()
                {
                    return;
                }
                slot.owner = Arc::downgrade(self);
                slot.connection = pending.take();
            });
        }
        if let Some(connection) = pending {
            self.return_cached(connection, false);
        }
    }
    fn return_cached(&self, connection: Cached, check_expired: bool) {
        let mut state = self.shared.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.shutdown
            || check_expired && connection.expired(&state.config, state.config.now())
            || state.metrics.total_connections > state.config.max_connections
        {
            state.metrics.total_connections = state.metrics.total_connections.saturating_sub(1);
            drop(state);
            drop(connection);
        } else {
            state.available.push_back(connection);
        }
        self.shared.available.notify_one();
    }
    fn discard(&self) {
        let mut state = self.shared.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.metrics.total_connections = state.metrics.total_connections.saturating_sub(1);
        drop(state);
        self.shared.available.notify_one();
    }
    pub fn set_max_connections(&self, value: u64) {
        let mut retired = Vec::new();
        let mut state = self.shared.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.config.max_connections = value;
        while state.metrics.total_connections > value {
            let Some(connection) = state.available.pop_back() else {
                break;
            };
            retired.push(connection);
            state.metrics.total_connections -= 1;
        }
        drop(state);
        drop(retired);
        self.shared.available.notify_all();
    }
    pub fn update(&self, change: impl FnOnce(&mut Config)) {
        change(
            &mut self
                .shared
                .state
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .config,
        );
        self.shared.reaper.notify_all();
    }
    pub fn set_reaper(&self, enabled: bool) -> Result<()> {
        let mut handle = self.reaper.lock().unwrap_or_else(PoisonError::into_inner);
        let mut state = self.shared.state.lock().unwrap_or_else(PoisonError::into_inner);
        if enabled {
            if state.shutdown || state.config.now().is_none() || state.metrics.reaper_running {
                return Ok(());
            }
        } else {
            state.metrics.reaper_running = false;
            state.generation += 1;
        }
        drop(state);
        self.shared.reaper.notify_all();
        if let Some(old) = handle.take() {
            let _ = old.join();
        }
        if !enabled {
            return Ok(());
        }
        let mut state = self.shared.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.metrics.reaper_running = true;
        state.generation += 1;
        let generation = state.generation;
        drop(state);
        let shared = self.shared.clone();
        match std::thread::Builder::new()
            .name("postgres-pool-reaper".into())
            .spawn(move || reap(&shared, generation))
        {
            Ok(thread) => {
                *handle = Some(thread);
                Ok(())
            }
            Err(error) => {
                self.shared
                    .state
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .metrics
                    .reaper_running = false;
                Err(pool_error(error.to_string()))
            }
        }
    }
    pub fn shutdown(&self) {
        let _ = self.set_reaper(false);
        let mut state = self.shared.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.shutdown = true;
        let retired = std::mem::take(&mut state.available);
        drop(state);
        drop(retired);
        self.shared.available.notify_all();
    }
}
impl Drop for Pool {
    fn drop(&mut self) {
        self.shutdown();
    }
}
fn expired(point: Option<Instant>, millis: u64, now: Option<Instant>) -> bool {
    if millis == 0 || now.is_none() {
        return false;
    }
    point.is_none_or(|point| {
        now.is_some_and(|now| now.saturating_duration_since(point) >= Duration::from_millis(millis))
    })
}
fn pool_error(message: impl Into<std::borrow::Cow<'static, str>>) -> Error {
    Error::new(ErrorKind::Invalid, message)
}
fn reap(shared: &Shared, generation: u64) {
    let mut state = shared.state.lock().unwrap_or_else(PoisonError::into_inner);
    loop {
        let deadline = Instant::now() + Duration::from_millis(state.config.reaper_period_millis());
        loop {
            if !state.metrics.reaper_running || state.generation != generation {
                return;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            state = shared
                .reaper
                .wait_timeout(state, remaining)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        if state.config.now().is_none() {
            state.metrics.reaper_running = false;
            return;
        }
        let mut retired = Vec::new();
        let mut available = VecDeque::new();
        let now = Some(Instant::now());
        while let Some(connection) = state.available.pop_front() {
            if connection.expired(&state.config, now) {
                state.metrics.total_connections = state.metrics.total_connections.saturating_sub(1);
                retired.push(connection);
            } else {
                available.push_back(connection);
            }
        }
        state.available = available;
        drop(state);
        if !retired.is_empty() {
            shared.available.notify_all();
        }
        drop(retired);
        state = shared.state.lock().unwrap_or_else(PoisonError::into_inner);
    }
}
