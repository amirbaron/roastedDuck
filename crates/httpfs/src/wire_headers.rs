//! Retain response header spelling for DuckDB logs without changing HTTP parsing.
//! ureq validates the protocol; this bounded observer only records header names.
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use ureq::unversioned::transport::{Buffers, ConnectionDetails, Connector, NextTimeout, Transport};

#[derive(Clone, Debug, Default)]
pub(super) struct Names(HashMap<String, String>);

thread_local! {
    static ACTIVE: RefCell<Option<Arc<Mutex<Names>>>> = const { RefCell::new(None) };
}

pub(super) struct Scope {
    current: Arc<Mutex<Names>>,
    previous: Option<Arc<Mutex<Names>>>,
}

impl Scope {
    pub fn enter() -> Self {
        let current = Arc::new(Mutex::new(Names::default()));
        let previous = ACTIVE.with(|active| active.replace(Some(Arc::clone(&current))));
        Self { current, previous }
    }

    pub fn names(&self) -> Names {
        self.current
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        ACTIVE.with(|active| active.replace(self.previous.take()));
    }
}

pub(super) fn name<'a, T>(response: &'a ureq::http::Response<T>, lower: &'a str) -> &'a str {
    response
        .extensions()
        .get::<Names>()
        .and_then(|names| names.0.get(lower))
        .map_or(lower, String::as_str)
}

#[derive(Debug, Default)]
struct Head {
    bytes: Vec<u8>,
    done: bool,
}

impl Head {
    fn feed(&mut self, bytes: &[u8], target: &Mutex<Names>) {
        for &byte in bytes {
            if self.done {
                return;
            }
            if self.bytes.len() == 128 * 1024 {
                self.bytes.clear();
                self.done = true;
                return;
            }
            self.bytes.push(byte);
            if !self.bytes.ends_with(b"\r\n\r\n") {
                continue;
            }
            let text = String::from_utf8_lossy(&self.bytes);
            let mut lines = text.split("\r\n");
            let status = lines
                .next()
                .and_then(|line| line.split_ascii_whitespace().nth(1))
                .and_then(|status| status.parse::<u16>().ok());
            if status.is_some_and(|status| (100..200).contains(&status) && status != 101) {
                self.bytes.clear();
                continue;
            }
            let mut names = Names::default();
            for line in lines {
                if let Some((name, _)) = line.split_once(':') {
                    names
                        .0
                        .entry(name.to_ascii_lowercase())
                        .or_insert_with(|| name.to_owned());
                }
            }
            *target.lock().unwrap_or_else(PoisonError::into_inner) = names;
            self.bytes.clear();
            self.done = true;
        }
    }
}

#[derive(Debug, Default)]
pub(super) struct HeaderConnector;

impl<In: Transport> Connector<In> for HeaderConnector {
    type Out = HeaderTransport<In>;
    fn connect(&self, _: &ConnectionDetails, inner: Option<In>) -> Result<Option<Self::Out>, ureq::Error> {
        Ok(inner.map(|inner| HeaderTransport {
            inner,
            target: None,
            head: Head::default(),
        }))
    }
}

#[derive(Debug)]
pub(super) struct HeaderTransport<In> {
    inner: In,
    target: Option<Arc<Mutex<Names>>>,
    head: Head,
}

impl<In: Transport> Transport for HeaderTransport<In> {
    fn buffers(&mut self) -> &mut dyn Buffers {
        self.inner.buffers()
    }

    fn transmit_output(&mut self, amount: usize, timeout: NextTimeout) -> Result<(), ureq::Error> {
        let active = ACTIVE.with(|active| active.borrow().clone());
        let same = match (&active, &self.target) {
            (Some(active), Some(target)) => Arc::ptr_eq(active, target),
            (None, None) => true,
            _ => false,
        };
        // A new request or an automatic redirect begins a new response head.
        // Body writes before the response headers retain the current observer.
        if !same || self.head.done {
            self.target = active;
            self.head = Head::default();
        }
        self.inner.transmit_output(amount, timeout)
    }

    fn await_input(&mut self, timeout: NextTimeout) -> Result<bool, ureq::Error> {
        let before = self.inner.buffers().input().len();
        let progressed = self.inner.await_input(timeout)?;
        if let Some(target) = &self.target {
            let bytes = self.inner.buffers().input();
            if bytes.len() >= before {
                self.head.feed(&bytes[before..], target);
            }
        }
        Ok(progressed)
    }

    fn is_open(&mut self) -> bool {
        self.inner.is_open()
    }
    fn is_tls(&self) -> bool {
        self.inner.is_tls()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn fragmented_interim_and_final_headers_preserve_first_spelling_only() {
        let wire = b"HTTP/1.1 100 Continue\r\nIgnore: interim\r\n\r\nHTTP/1.1 200 OK\r\nCoNtEnT-RaNgE: bytes 0-1/2\r\nX-Case: first\r\nx-case: second\r\n\r\nBody: not-a-header\r\n\r\n";
        for width in 1..wire.len() {
            let mut head = Head::default();
            let target = Mutex::new(Names::default());
            for part in wire.chunks(width) {
                head.feed(part, &target);
            }
            let names = target.lock().unwrap();
            assert_eq!(names.0.len(), 2);
            assert_eq!(names.0["content-range"], "CoNtEnT-RaNgE");
            assert_eq!(names.0["x-case"], "X-Case");
        }
    }

    #[test]
    fn oversized_observation_is_bounded_without_rejecting_transport_data() {
        let mut head = Head::default();
        let target = Mutex::new(Names::default());
        head.feed(&vec![b'x'; 129 * 1024], &target);
        assert!(head.done && head.bytes.is_empty());
        assert!(target.lock().unwrap().0.is_empty());
    }
}
