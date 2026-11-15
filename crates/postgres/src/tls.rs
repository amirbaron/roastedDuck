//! TLS policies corresponding to PostgreSQL connection options.
use crate::config::Parameters;
use roastedduck_common::{Error, Result};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime, pem::PemObject};
use std::sync::Arc;

#[derive(Debug)]
struct Verifier {
    roots: Option<rustls::RootCertStore>,
    hostname: bool,
    algorithms: rustls::crypto::WebPkiSupportedAlgorithms,
}
impl ServerCertVerifier for Verifier {
    fn verify_server_cert(
        &self,
        end: &CertificateDer<'_>,
        chain: &[CertificateDer<'_>],
        name: &ServerName<'_>,
        _ocsp: &[u8],
        now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        if let Some(roots) = &self.roots {
            let cert = rustls::server::ParsedCertificate::try_from(end)?;
            rustls::client::verify_server_cert_signed_by_trust_anchor(
                &cert,
                roots,
                chain,
                now,
                self.algorithms.all,
            )?;
            if self.hostname {
                rustls::client::verify_server_name(&cert, name)?;
            }
        }
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, signature, &self.algorithms)
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, signature, &self.algorithms)
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

pub(crate) fn connector(parameters: &Parameters) -> Result<tokio_postgres_rustls::MakeRustlsConnect> {
    let io = |error: &dyn std::fmt::Display| Error::new(roastedduck_common::ErrorKind::Io, error.to_string());
    let mode = parameters.get("sslmode").unwrap_or("prefer");
    let default_root = std::env::var("HOME")
        .ok()
        .map(|home| format!("{home}/.postgresql/root.crt"));
    let path = parameters.get("sslrootcert").map(str::to_owned).or(default_root);
    let present = path.as_ref().is_some_and(|p| std::path::Path::new(p).exists());
    let verify = matches!(mode, "verify-ca" | "verify-full") || mode == "require" && present;
    if parameters.get("sslcrl").is_some_and(|p| !p.is_empty()) {
        return Err(Error::not_implemented(
            "PostgreSQL certificate revocation lists are not implemented yet",
        ));
    }
    let roots = if verify {
        let path = path.ok_or_else(|| {
            Error::invalid_input("root certificate file is required for certificate verification")
        })?;
        let mut roots = rustls::RootCertStore::empty();
        for cert in CertificateDer::pem_file_iter(&path).map_err(|e| io(&e))? {
            roots.add(cert.map_err(|e| io(&e))?).map_err(|e| io(&e))?;
        }
        Some(roots)
    } else {
        None
    };
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier = Arc::new(Verifier {
        roots,
        hostname: mode == "verify-full",
        algorithms: provider.signature_verification_algorithms,
    });
    let builder = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| io(&e))?
        .dangerous()
        .with_custom_certificate_verifier(verifier);
    let mut config = if let Some(cert) = parameters.get("sslcert") {
        let certificates = CertificateDer::pem_file_iter(cert)
            .map_err(|e| io(&e))?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| io(&e))?;
        let key = parameters
            .get("sslkey")
            .ok_or_else(|| Error::invalid_input("sslkey is required with sslcert"))?;
        builder
            .with_client_auth_cert(
                certificates,
                PrivateKeyDer::from_pem_file(key).map_err(|e| io(&e))?,
            )
            .map_err(|e| io(&e))?
    } else {
        builder.with_no_client_auth()
    };
    if parameters.get("sslnegotiation") == Some("direct") {
        config.alpn_protocols = vec![b"postgresql".to_vec()];
    }
    Ok(tokio_postgres_rustls::MakeRustlsConnect::new(config))
}
