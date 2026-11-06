//! OpenSSL transport for pinned UREQ3.5, using the HTTPFS CA-file search order.
//! Apple native trust supplements file-based roots, as `CURLSSLOPT_NATIVE_CA` does.
use std::fmt;
use std::io::{self, Read, Write};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use openssl::ssl::{HandshakeError, SslConnector, SslMethod, SslStream, SslVerifyMode};
#[cfg(target_vendor = "apple")]
use openssl::x509::X509StoreContextRef;
use openssl::x509::X509VerifyResult;
use ureq::Error;
use ureq::unversioned::transport::time::{Duration, Instant};
use ureq::unversioned::transport::{
    Buffers, ConnectionDetails, Connector, Either, LazyBuffers, NextTimeout, Transport, TransportAdapter,
};

const CA_FILES: &[&str] = &[
    "/etc/ssl/certs/ca-certificates.crt",
    "/etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem",
    "/etc/pki/tls/certs/ca-bundle.crt",
    "/etc/ssl/ca-bundle.pem",
    "/etc/ssl/cert.pem",
];

#[derive(Debug)]
pub(super) struct CertificateError {
    pub hostname: bool,
}
impl fmt::Display for CertificateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TLS certificate validation failed")
    }
}
impl std::error::Error for CertificateError {}

struct Context {
    connector: SslConnector,
    #[cfg(target_vendor = "apple")]
    hostname: openssl::ex_data::Index<openssl::ssl::Ssl, String>,
}

pub(super) struct TlsConnector {
    ca: String,
    verify: bool,
    native_roots: bool,
    context: OnceLock<Context>,
}
impl fmt::Debug for TlsConnector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpenSslTlsConnector")
            .field("verify", &self.verify)
            .field("native_roots", &self.native_roots)
            .finish_non_exhaustive()
    }
}
fn error(error: impl std::error::Error + Send + Sync + 'static) -> Error {
    Error::Other(Box::new(error))
}

impl TlsConnector {
    pub(super) fn new(ca: String, verify: bool, native_roots: bool) -> Self {
        Self {
            ca,
            verify,
            native_roots,
            context: OnceLock::new(),
        }
    }
    fn build(&self) -> Result<Context, Error> {
        let mut builder = SslConnector::builder(SslMethod::tls_client()).map_err(error)?;
        // The vendored OpenSSL build path must not become a release trust path.
        let store = openssl::x509::store::X509StoreBuilder::new()
            .map_err(error)?
            .build();
        builder.set_cert_store(store);
        if self.verify {
            if !self.ca.is_empty() {
                builder.set_ca_file(&self.ca).map_err(error)?;
            } else if let Some(path) = CA_FILES.iter().find(|path| std::path::Path::new(path).exists()) {
                builder.set_ca_file(path).map_err(error)?;
            }
        }
        #[cfg(target_vendor = "apple")]
        let hostname = openssl::ssl::Ssl::new_ex_index::<String>().map_err(error)?;
        if self.verify {
            #[cfg(target_vendor = "apple")]
            {
                let native_roots = self.native_roots;
                builder.set_verify_callback(SslVerifyMode::PEER, move |valid, store| {
                    valid || native_roots && native_trust(store, hostname)
                });
            }
            #[cfg(not(target_vendor = "apple"))]
            builder.set_verify(SslVerifyMode::PEER);
        } else {
            builder.set_verify(SslVerifyMode::NONE);
        }
        Ok(Context {
            connector: builder.build(),
            #[cfg(target_vendor = "apple")]
            hostname,
        })
    }
}

#[cfg(target_vendor = "apple")]
fn native_trust(
    store: &X509StoreContextRef,
    hostname: openssl::ex_data::Index<openssl::ssl::Ssl, String>,
) -> bool {
    // Only supplement missing trust anchors. Expiry, signatures, key usage and
    // hostname failures remain failures; the platform also checks the full chain.
    if !matches!(store.error().as_raw(), 2 | 18 | 19 | 20 | 21) {
        return false;
    }
    let Some(domain) = openssl::x509::X509StoreContext::ssl_idx()
        .ok()
        .and_then(|index| store.ex_data(index))
        .and_then(|ssl| ssl.ex_data(hostname))
    else {
        return false;
    };
    let Some(chain) = store.chain() else {
        return false;
    };
    let mut certs = Vec::new();
    for cert in chain {
        let Ok(der) = cert.to_der() else {
            return false;
        };
        let Ok(cert) = security_framework::certificate::SecCertificate::from_der(&der) else {
            return false;
        };
        certs.push(cert);
    }
    if certs.is_empty() {
        return false;
    }
    let policy = security_framework::policy::SecPolicy::create_ssl(
        security_framework::secure_transport::SslProtocolSide::SERVER,
        Some(domain),
    );
    let Ok(trust) = security_framework::trust::SecTrust::create_with_certificates(&certs, &[policy]) else {
        return false;
    };
    trust.evaluate_with_error().is_ok()
}

impl<In: Transport> Connector<In> for TlsConnector {
    type Out = Either<In, TlsTransport>;
    fn connect(&self, details: &ConnectionDetails, chained: Option<In>) -> Result<Option<Self::Out>, Error> {
        let Some(transport) = chained else {
            return Ok(None);
        };
        if !details.needs_tls() || transport.is_tls() {
            return Ok(Some(Either::A(transport)));
        }
        if self.context.get().is_none() {
            let _ = self.context.set(self.build()?);
        }
        let context = self
            .context
            .get()
            .ok_or(Error::Tls("TLS context initialization failed"))?;
        let authority = details
            .uri
            .authority()
            .ok_or_else(|| Error::BadUri("TLS URL has no authority".into()))?;
        let domain = authority.host().trim_start_matches('[').trim_end_matches(']');
        let mut config = context.connector.configure().map_err(error)?;
        config.set_verify_hostname(self.verify);
        let ssl = config.into_ssl(domain).map_err(error)?;
        #[cfg(target_vendor = "apple")]
        let ssl = {
            let mut ssl = ssl;
            ssl.set_ex_data(context.hostname, domain.to_owned());
            ssl
        };
        let mut inner = TransportAdapter::new(transport.boxed(), Arc::clone(&details.current_time));
        let mut timeout = details.timeout;
        if let (Duration::Exact(after), Instant::Exact(start), Instant::Exact(now)) =
            (timeout.after, details.now, (details.current_time)())
        {
            timeout.after = Duration::Exact(after.saturating_sub(now.saturating_duration_since(start)));
        }
        inner.set_timeout(timeout);
        let captured = Arc::new(Mutex::new(None));
        let adapter = Capture {
            inner,
            captured: Arc::clone(&captured),
        };
        let stream = match ssl.connect(adapter) {
            Ok(stream) => stream,
            Err(HandshakeError::Failure(mid) | HandshakeError::WouldBlock(mid)) => {
                if let Some(error) = captured.lock().unwrap_or_else(PoisonError::into_inner).take() {
                    return Err(error);
                }
                let verify = mid.ssl().verify_result();
                if verify != X509VerifyResult::OK {
                    return Err(error(CertificateError {
                        hostname: matches!(verify.as_raw(), 62 | 64),
                    }));
                }
                return Err(Error::Io(io::Error::other(mid.error().to_string())));
            }
            Err(HandshakeError::SetupFailure(e)) => return Err(error(e)),
        };
        Ok(Some(Either::B(TlsTransport {
            stream,
            buffers: LazyBuffers::new(
                details.config.input_buffer_size(),
                details.config.output_buffer_size(),
            ),
        })))
    }
}

struct Capture {
    inner: TransportAdapter,
    captured: Arc<Mutex<Option<Error>>>,
}
impl Capture {
    fn capture(&self, e: io::Error) -> io::Error {
        let kind = e.kind();
        let text = e.to_string();
        match e.into_inner() {
            Some(cause) => match cause.downcast::<Error>() {
                Ok(e) => {
                    *self.captured.lock().unwrap_or_else(PoisonError::into_inner) = Some(*e);
                    io::Error::other("HTTP transport failed")
                }
                Err(cause) => io::Error::new(kind, cause),
            },
            None => io::Error::new(kind, text),
        }
    }
    fn take(&self) -> Result<(), Error> {
        self.captured
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
            .map_or(Ok(()), Err)
    }
}
impl Read for Capture {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        let result = self.inner.read(b);
        result.map_err(|e| self.capture(e))
    }
}
impl Write for Capture {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        let result = self.inner.write(b);
        result.map_err(|e| self.capture(e))
    }
    fn flush(&mut self) -> io::Result<()> {
        let result = self.inner.flush();
        result.map_err(|e| self.capture(e))
    }
}

pub(super) struct TlsTransport {
    stream: SslStream<Capture>,
    buffers: LazyBuffers,
}
impl fmt::Debug for TlsTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpenSslTlsTransport").finish_non_exhaustive()
    }
}
impl Transport for TlsTransport {
    fn buffers(&mut self) -> &mut dyn Buffers {
        &mut self.buffers
    }
    fn transmit_output(&mut self, amount: usize, timeout: NextTimeout) -> Result<(), Error> {
        self.stream.get_mut().inner.set_timeout(timeout);
        let result = self.stream.write_all(&self.buffers.output()[..amount]);
        self.stream.get_ref().take()?;
        result.map_err(Error::Io)
    }
    fn await_input(&mut self, timeout: NextTimeout) -> Result<bool, Error> {
        self.stream.get_mut().inner.set_timeout(timeout);
        let result = self.stream.read(self.buffers.input_append_buf());
        self.stream.get_ref().take()?;
        let amount = result.map_err(Error::Io)?;
        self.buffers.input_appended(amount);
        Ok(amount > 0)
    }
    fn is_open(&mut self) -> bool {
        self.stream.get_mut().inner.get_mut().is_open()
    }
    fn is_tls(&self) -> bool {
        true
    }
}
