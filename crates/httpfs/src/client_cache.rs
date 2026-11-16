//! Database-local idle HTTP clients. Each file acquires a client on first I/O.
use std::sync::{Arc, Mutex, PoisonError};

use roastedduck_common::Result;
use roastedduck_function::logging::{LogLevel, LogScope, LogTypeInfo};

use super::Parameters;

const CAPACITY: usize = 256;

pub fn log_type() -> LogTypeInfo {
    LogTypeInfo {
        name: "HTTPFSInfo".into(),
        level: LogLevel::Info,
        structured_type: None,
    }
}

#[derive(Clone, PartialEq, Eq)]
struct Key {
    origin: String,
    verify: bool,
    httplib: bool,
    ca: String,
    keep_alive: bool,
    redirects: u32,
    proxy: String,
    proxy_user: String,
    proxy_password: String,
}

impl std::fmt::Debug for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpClientKey").finish_non_exhaustive()
    }
}

impl Key {
    fn new(parameters: &Parameters, url: &str) -> Self {
        // Invalid URLs retain the request path's transport-specific diagnostic.
        let origin = url
            .replace(' ', "%20")
            .parse::<ureq::http::Uri>()
            .ok()
            .and_then(|uri| {
                uri.authority()
                    .map(|authority| format!("{}://{authority}", uri.scheme_str().unwrap_or("http")))
            })
            .unwrap_or_default();
        Self {
            origin,
            verify: parameters.verify,
            httplib: parameters.httplib,
            ca: parameters.ca.clone(),
            keep_alive: parameters.keep_alive,
            redirects: parameters.redirects,
            proxy: parameters.proxy.clone(),
            proxy_user: parameters.proxy_user.clone(),
            proxy_password: parameters.proxy_password.clone(),
        }
    }
}

#[derive(Debug, Default)]
struct PoolState {
    enabled: bool,
    httplib: bool,
    generation: u64,
    idle: Vec<(Key, ureq::Agent)>,
}

#[derive(Debug, Default)]
struct Pool(Mutex<PoolState>);

impl Pool {
    fn current() -> Option<Arc<Self>> {
        let client = roastedduck_function::file_system::opener()?;
        Some(
            client
                .object_cache()?
                .get_or_insert_with("httpfs.connection_pool", Self::default),
        )
    }

    fn set_enabled(&self, enabled: bool) {
        let mut state = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if !state.httplib {
            state.enabled = enabled;
        }
    }

    fn reset(&self, httplib: bool) {
        let mut state = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        state.enabled = false;
        state.httplib = httplib;
        state.generation = state.generation.wrapping_add(1);
        state.idle.clear();
    }
}

pub(super) fn set_enabled(enabled: bool) {
    if let Some(pool) = Pool::current() {
        pool.set_enabled(enabled);
    }
}

pub(super) fn reset(httplib: bool) {
    if let Some(pool) = Pool::current() {
        pool.reset(httplib);
    }
}

#[derive(Debug)]
pub(super) struct Lease {
    agent: ureq::Agent,
    key: Key,
    pool: Option<Arc<Pool>>,
    generation: u64,
}

impl std::ops::Deref for Lease {
    type Target = ureq::Agent;
    fn deref(&self) -> &Self::Target {
        &self.agent
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        if let Some(pool) = &self.pool {
            let mut state = pool.0.lock().unwrap_or_else(PoisonError::into_inner);
            if state.enabled && !state.httplib && state.generation == self.generation {
                if state.idle.len() == CAPACITY {
                    state.idle.remove(0);
                }
                state.idle.push((self.key.clone(), self.agent.clone()));
            }
        }
    }
}

#[derive(Debug, Default)]
pub(super) struct Client(Mutex<Option<Arc<Lease>>>);

impl Client {
    pub fn get(&self, parameters: &Parameters, url: &str, cache_allowed: bool) -> Result<Arc<Lease>> {
        let mut owned = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(lease) = &*owned {
            return Ok(Arc::clone(lease));
        }
        let key = Key::new(parameters, url);
        let pool = if cache_allowed && !key.origin.is_empty() && !parameters.httplib {
            Pool::current()
        } else {
            None
        };
        let mut generation = 0;
        let mut enabled = false;
        let cached = pool.as_ref().and_then(|pool| {
            let mut state = pool.0.lock().unwrap_or_else(PoisonError::into_inner);
            generation = state.generation;
            enabled = state.enabled && !state.httplib;
            if !enabled {
                return None;
            }
            state
                .idle
                .iter()
                .position(|(candidate, _)| candidate == &key)
                .map(|position| state.idle.remove(position).1)
        });
        if enabled
            && let Some(client) = roastedduck_function::file_system::opener()
            && client
                .settings()
                .and_then(|s| s.current_setting("enable_http_logging").ok())
                .and_then(|v| v.as_bool())
                .unwrap_or(true)
            && let Some(logger) = client.logger()
            && logger.should_log(LogScope::Connection, "HTTPFSInfo", LogLevel::Info)
        {
            let event = if cached.is_some() {
                "connection_cache_hit"
            } else {
                "connection_cache_miss"
            };
            logger.write_log(
                LogScope::Connection,
                "HTTPFSInfo",
                LogLevel::Info,
                &format!("{{\"type\":\"{event}\",\"host\":\"{}\"}}", key.origin),
            )?;
        }
        let agent = match cached {
            Some(agent) => agent,
            None => parameters.agent()?,
        };
        let lease = Arc::new(Lease {
            agent,
            key,
            pool,
            generation,
        });
        *owned = Some(Arc::clone(&lease));
        Ok(lease)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn lease(pool: &Arc<Pool>, generation: u64) -> Lease {
        let parameters = Parameters::current("http://owned.invalid/a").unwrap();
        Lease {
            agent: parameters.agent().unwrap(),
            key: Key::new(&parameters, "http://owned.invalid/a"),
            pool: Some(Arc::clone(pool)),
            generation,
        }
    }

    #[test]
    fn idle_pool_is_bounded_when_many_live_clients_return() {
        let pool = Arc::new(Pool::default());
        pool.set_enabled(true);
        let clients: Vec<_> = (0..CAPACITY + 8).map(|_| lease(&pool, 0)).collect();
        drop(clients);
        assert_eq!(pool.0.lock().unwrap().idle.len(), CAPACITY);
    }

    #[test]
    fn backend_reset_rejects_clients_borrowed_before_the_reset() {
        let pool = Arc::new(Pool::default());
        pool.set_enabled(true);
        let previous = lease(&pool, 0);
        pool.reset(false);
        pool.set_enabled(true);
        drop(previous);
        assert!(pool.0.lock().unwrap().idle.is_empty());
        drop(lease(&pool, 1));
        assert_eq!(pool.0.lock().unwrap().idle.len(), 1);
    }
}
