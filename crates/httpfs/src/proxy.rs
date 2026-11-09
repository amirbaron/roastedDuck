//! HTTP uses absolute-form proxy requests; HTTPS keeps the CONNECT tunnel.

use std::fmt;
use std::io::{self, Write};
use std::sync::Arc;

use ureq::Error;
use ureq::unversioned::transport::time::Instant;
use ureq::unversioned::transport::{
    Buffers, ConnectProxyConnector, ConnectionDetails, Connector, Either, NextTimeout, TcpConnector,
    Transport, TransportAdapter,
};

#[derive(Debug, Default)]
pub(crate) struct ProxyConnector;

/// Retain the resolution stage so installer diagnostics distinguish a proxy DNS
/// failure from connection refusal, timeout and origin failures.
#[derive(Debug, Default)]
pub(super) struct InstallerResolver;

#[derive(Debug)]
pub(super) struct ProxyNameResolutionError(Error);

impl fmt::Display for ProxyNameResolutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Could not resolve proxy name: {}", self.0)
    }
}

impl std::error::Error for ProxyNameResolutionError {}

impl ureq::unversioned::resolver::Resolver for InstallerResolver {
    fn resolve(
        &self,
        uri: &ureq::http::Uri,
        config: &ureq::config::Config,
        timeout: NextTimeout,
    ) -> Result<ureq::unversioned::resolver::ResolvedSocketAddrs, Error> {
        ureq::unversioned::resolver::DefaultResolver::default()
            .resolve(uri, config, timeout)
            .map_err(|error| {
                if config
                    .proxy()
                    .is_some_and(|proxy| proxy.uri().authority() == uri.authority())
                    && matches!(error, Error::HostNotFound | Error::Io(_))
                {
                    Error::Other(Box::new(ProxyNameResolutionError(error)))
                } else {
                    error
                }
            })
    }
}

impl<In: Transport> Connector<In> for ProxyConnector {
    type Out = Either<In, Box<dyn Transport>>;

    fn connect(
        &self,
        details: &ConnectionDetails<'_>,
        chained: Option<In>,
    ) -> Result<Option<Self::Out>, Error> {
        if let Some(transport) = chained {
            return Ok(Some(Either::A(transport)));
        }
        let proxy = details.config.proxy().filter(|proxy| {
            details.uri.scheme_str() == Some("http") && proxy.protocol() == ureq::ProxyProtocol::Http
        });
        let Some(proxy) = proxy else {
            return ConnectProxyConnector::default().connect(details, None::<In>);
        };
        let proxy_details = ConnectionDetails {
            uri: proxy.uri(),
            addrs: details
                .resolver
                .resolve(proxy.uri(), details.config, details.timeout)?,
            config: details.config,
            request_level: details.request_level,
            resolver: details.resolver,
            now: details.now,
            timeout: details.timeout,
            current_time: details.current_time.clone(),
            run_connector: details.run_connector.clone(),
            register_shutdown: details.register_shutdown.clone(),
        };
        let Some(transport) = TcpConnector::default().connect(&proxy_details, None::<()>)? else {
            return Ok(None);
        };
        let authority = details
            .uri
            .authority()
            .ok_or_else(|| invalid("Missing proxy target host"))?;
        let authority = authority.as_str().rsplit('@').next().unwrap_or_default();
        let authorization = if proxy.username().is_some() || proxy.password().is_some() {
            let credentials = format!(
                "{}:{}",
                proxy.username().unwrap_or_default(),
                proxy.password().unwrap_or_default()
            );
            let mut encoded = b"Basic ".to_vec();
            roastedduck_types::blob::write_base64(credentials.as_bytes(), &mut encoded);
            Some(encoded)
        } else {
            None
        };
        Ok(Some(Either::B(Box::new(ForwardTransport {
            inner: transport.boxed(),
            origin: format!("http://{authority}"),
            authorization,
            head: Vec::new(),
            body: Body::Empty,
            clock: details.current_time.clone(),
        }))))
    }
}

fn invalid(message: &'static str) -> Error {
    Error::Io(io::Error::new(io::ErrorKind::InvalidData, message))
}

struct ForwardTransport {
    inner: Box<dyn Transport>,
    origin: String,
    authorization: Option<Vec<u8>>,
    head: Vec<u8>,
    body: Body,
    clock: Arc<dyn Fn() -> Instant + Send + Sync>,
}

impl fmt::Debug for ForwardTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Neither buffered headers nor proxy credentials belong in debug output.
        formatter
            .debug_struct("ForwardProxyTransport")
            .finish_non_exhaustive()
    }
}

impl Transport for ForwardTransport {
    fn buffers(&mut self) -> &mut dyn Buffers {
        self.inner.buffers()
    }

    fn transmit_output(&mut self, amount: usize, timeout: NextTimeout) -> Result<(), Error> {
        if amount == 0 {
            return Ok(());
        }
        if !matches!(self.body, Body::Empty) {
            let inner = &mut self.inner;
            self.body.consume(&inner.buffers().output()[..amount])?;
            return inner.transmit_output(amount, timeout);
        }
        let bytes = self.buffers().output()[..amount].to_vec();
        self.head.extend_from_slice(&bytes);
        let Some(end) = self
            .head
            .windows(4)
            .position(|bytes| bytes == b"\r\n\r\n")
            .map(|pos| pos + 4)
        else {
            // A request head can span multiple output buffers. Upload bodies
            // are streamed unchanged once the complete head has been rewritten.
            return Ok(());
        };
        let (mut output, mut body) =
            rewrite_head(&self.head[..end], &self.origin, self.authorization.as_deref())?;
        body.consume(&self.head[end..])?;
        output.extend_from_slice(&self.head[end..]);
        self.head.clear();
        self.body = body;
        let inner = std::mem::replace(&mut self.inner, ().boxed());
        let mut writer = TransportAdapter::new(inner, self.clock.clone());
        writer.set_timeout(timeout);
        let result = writer.write_all(&output);
        self.inner = writer.into_inner();
        result.map_err(Error::Io)
    }

    fn await_input(&mut self, timeout: NextTimeout) -> Result<bool, Error> {
        self.inner.await_input(timeout)
    }

    fn is_open(&mut self) -> bool {
        self.inner.is_open()
    }

    fn is_tls(&self) -> bool {
        false
    }
}

fn rewrite_head(head: &[u8], origin: &str, authorization: Option<&[u8]>) -> Result<(Vec<u8>, Body), Error> {
    let end = head
        .windows(2)
        .position(|bytes| bytes == b"\r\n")
        .ok_or_else(|| invalid("Invalid proxy request line"))?;
    let line = &head[..end];
    let target = line
        .iter()
        .position(|byte| *byte == b' ')
        .ok_or_else(|| invalid("Missing proxy request target"))?
        + 1;
    let mut output =
        Vec::with_capacity(head.len() + origin.len() + authorization.map_or(0, <[u8]>::len) + 24);
    output.extend_from_slice(&line[..target]);
    if line.get(target) == Some(&b'/') {
        output.extend_from_slice(origin.as_bytes());
    }
    output.extend_from_slice(&line[target..]);
    output.extend_from_slice(b"\r\n");
    let mut length = 0;
    let mut chunked = false;
    for line in head[end + 2..].split(|byte| *byte == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() {
            continue;
        }
        let colon = line
            .iter()
            .position(|byte| *byte == b':')
            .ok_or_else(|| invalid("Invalid proxy request header"))?;
        let (name, value) = (&line[..colon], &line[colon + 1..]);
        if name.eq_ignore_ascii_case(b"content-length") {
            length = std::str::from_utf8(value)
                .ok()
                .and_then(|text| text.trim().parse().ok())
                .ok_or_else(|| invalid("Invalid proxy content length"))?;
        }
        if name.eq_ignore_ascii_case(b"transfer-encoding") {
            chunked = value
                .split(|byte| *byte == b',')
                .any(|value| value.trim_ascii().eq_ignore_ascii_case(b"chunked"));
        }
        if authorization.is_some() && name.eq_ignore_ascii_case(b"proxy-authorization") {
            continue;
        }
        output.extend_from_slice(line);
        output.extend_from_slice(b"\r\n");
    }
    if let Some(authorization) = authorization {
        output.extend_from_slice(b"Proxy-Authorization: ");
        output.extend_from_slice(authorization);
        output.extend_from_slice(b"\r\n");
    }
    output.extend_from_slice(b"\r\n");
    let body = if chunked {
        Body::Chunked(Chunked {
            phase: Phase::Size,
            line: Vec::new(),
        })
    } else if length > 0 {
        Body::Fixed(length)
    } else {
        Body::Empty
    };
    Ok((output, body))
}

enum Body {
    Empty,
    Fixed(u64),
    Chunked(Chunked),
}

impl Body {
    fn consume(&mut self, bytes: &[u8]) -> Result<(), Error> {
        let finished = match self {
            Self::Empty => {
                if !bytes.is_empty() {
                    return Err(invalid("Unexpected proxy request body"));
                }
                false
            }
            Self::Fixed(remaining) => {
                *remaining = remaining
                    .checked_sub(bytes.len() as u64)
                    .ok_or_else(|| invalid("Proxy request body exceeds content length"))?;
                *remaining == 0
            }
            Self::Chunked(chunked) => chunked.consume(bytes)?,
        };
        if finished {
            *self = Self::Empty;
        }
        Ok(())
    }
}

enum Phase {
    Size,
    Data(u64),
    Delimiter(u8),
    Trailers,
    Done,
}

struct Chunked {
    phase: Phase,
    line: Vec<u8>,
}

impl Chunked {
    fn consume(&mut self, mut bytes: &[u8]) -> Result<bool, Error> {
        while !bytes.is_empty() {
            match &mut self.phase {
                Phase::Size | Phase::Trailers => {
                    self.line.push(bytes[0]);
                    bytes = &bytes[1..];
                    if self.line.ends_with(b"\r\n") {
                        if matches!(self.phase, Phase::Trailers) {
                            if self.line.len() == 2 {
                                self.phase = Phase::Done;
                            }
                        } else {
                            let size = self.line[..self.line.len() - 2]
                                .split(|byte| *byte == b';')
                                .next()
                                .unwrap_or_default();
                            let size = std::str::from_utf8(size)
                                .ok()
                                .and_then(|text| u64::from_str_radix(text.trim(), 16).ok())
                                .ok_or_else(|| invalid("Invalid proxy chunk size"))?;
                            self.phase = if size == 0 {
                                Phase::Trailers
                            } else {
                                Phase::Data(size)
                            };
                        }
                        self.line.clear();
                    }
                }
                Phase::Data(remaining) => {
                    let count = (*remaining).min(bytes.len() as u64) as usize;
                    *remaining -= count as u64;
                    bytes = &bytes[count..];
                    if *remaining == 0 {
                        self.phase = Phase::Delimiter(0);
                    }
                }
                Phase::Delimiter(position) => {
                    if bytes[0] != b"\r\n"[*position as usize] {
                        return Err(invalid("Invalid proxy chunk delimiter"));
                    }
                    bytes = &bytes[1..];
                    *position += 1;
                    if *position == 2 {
                        self.phase = Phase::Size;
                    }
                }
                Phase::Done => return Err(invalid("Bytes after proxy chunked request")),
            }
        }
        Ok(matches!(self.phase, Phase::Done))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use ureq::unversioned::transport::LazyBuffers;

    #[derive(Debug)]
    struct Recording {
        buffers: LazyBuffers,
        wire: Arc<Mutex<Vec<u8>>>,
    }

    impl Transport for Recording {
        fn buffers(&mut self) -> &mut dyn Buffers {
            &mut self.buffers
        }

        fn transmit_output(&mut self, amount: usize, _: NextTimeout) -> Result<(), Error> {
            self.wire
                .lock()
                .unwrap()
                .extend_from_slice(&self.buffers.output()[..amount]);
            Ok(())
        }

        fn await_input(&mut self, _: NextTimeout) -> Result<bool, Error> {
            Ok(false)
        }

        fn is_open(&mut self) -> bool {
            true
        }
    }

    fn transport() -> (ForwardTransport, Arc<Mutex<Vec<u8>>>) {
        let wire = Arc::new(Mutex::new(Vec::new()));
        let inner = Recording {
            buffers: LazyBuffers::new(128, 128),
            wire: wire.clone(),
        };
        (
            ForwardTransport {
                inner: Box::new(inner),
                origin: "http://fixture.invalid".to_owned(),
                authorization: None,
                head: Vec::new(),
                body: Body::Empty,
                clock: Arc::new(Instant::now),
            },
            wire,
        )
    }

    fn send(transport: &mut ForwardTransport, bytes: &[u8]) {
        transport.buffers().output()[..bytes.len()].copy_from_slice(bytes);
        transport
            .transmit_output(bytes.len(), NextTimeout::default())
            .unwrap();
    }

    #[test]
    fn fragmented_headers_and_method_looking_body_survive_connection_reuse() {
        let (mut transport, wire) = transport();
        let body = b"GET /payload HTTP/1.1\r\n\r\n";
        let head = format!(
            "PUT /object HTTP/1.1\r\nHost: fixture.invalid\r\nContent-Length: {}\r\n\r\n",
            body.len()
        );
        for part in head.as_bytes().chunks(7) {
            send(&mut transport, part);
        }
        send(&mut transport, body);
        send(
            &mut transport,
            b"GET /next HTTP/1.1\r\nHost: fixture.invalid\r\n\r\n",
        );
        let mut expected = format!("PUT http://fixture.invalid/object HTTP/1.1\r\nHost: fixture.invalid\r\nContent-Length: {}\r\n\r\n", body.len()).into_bytes();
        expected.extend_from_slice(body);
        expected
            .extend_from_slice(b"GET http://fixture.invalid/next HTTP/1.1\r\nHost: fixture.invalid\r\n\r\n");
        assert_eq!(*wire.lock().unwrap(), expected);
    }

    #[test]
    fn chunked_body_and_trailers_are_unchanged_at_every_byte_boundary() {
        let (mut transport, wire) = transport();
        let head = b"POST /object HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n";
        let body = b"5\r\nGET /\r\n0\r\nX-Trailer: ignored\r\n\r\n";
        send(&mut transport, head);
        for byte in body {
            send(&mut transport, &[*byte]);
        }
        send(&mut transport, b"HEAD /next HTTP/1.1\r\n\r\n");
        let mut expected =
            b"POST http://fixture.invalid/object HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
        expected.extend_from_slice(body);
        expected.extend_from_slice(b"HEAD http://fixture.invalid/next HTTP/1.1\r\n\r\n");
        assert_eq!(*wire.lock().unwrap(), expected);
    }
}
