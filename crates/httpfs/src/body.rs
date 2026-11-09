//! Count response body bytes as they are consumed, without buffering the response.
use std::io::{self, Read};
use std::sync::{Arc, Weak};

use roastedduck_function::FunctionClient;

pub(super) struct Body {
    inner: ureq::Body,
    client: Option<Weak<FunctionClient>>,
    query_id: u64,
}

impl Body {
    pub(super) fn new(inner: ureq::Body, client: Option<&Arc<FunctionClient>>) -> Self {
        Self {
            inner,
            client: client.map(Arc::downgrade),
            query_id: client.map_or(0, |client| client.query_id()),
        }
    }

    pub(super) fn as_reader(&mut self) -> BodyReader<ureq::BodyReader<'_>> {
        BodyReader {
            inner: self.inner.as_reader(),
            client: self.client.clone(),
            query_id: self.query_id,
        }
    }

    pub(super) fn into_reader(self) -> BodyReader<ureq::BodyReader<'static>> {
        BodyReader {
            inner: self.inner.into_reader(),
            client: self.client,
            query_id: self.query_id,
        }
    }
}

pub struct BodyReader<R> {
    inner: R,
    client: Option<Weak<FunctionClient>>,
    query_id: u64,
}

impl<R> std::fmt::Debug for BodyReader<R> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("HTTPBodyReader").finish_non_exhaustive()
    }
}

impl<R: Read> Read for BodyReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let count = self.inner.read(buffer)?;
        if count != 0
            && let Some(client) = self.client.as_ref().and_then(Weak::upgrade)
        {
            client.record_http_received(self.query_id, count as u64);
        }
        Ok(count)
    }
}
