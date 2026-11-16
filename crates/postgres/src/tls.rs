//! TLS policies corresponding to PostgreSQL connection options.
use crate::config::Parameters;
use roastedduck_common::{Error, Result};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{
    CertificateDer, CertificateRevocationListDer, PrivateKeyDer, ServerName, UnixTime, pem::PemObject,
};
use std::sync::Arc;
use x509_cert::der::Decode;

#[derive(Debug)]
struct Verifier {
    certificates: Option<Arc<rustls::client::WebPkiServerVerifier>>,
    crls: Vec<x509_cert::crl::CertificateList>,
    require_crl: bool,
    hostname: bool,
    algorithms: rustls::crypto::WebPkiSupportedAlgorithms,
}
impl ServerCertVerifier for Verifier {
    fn verify_server_cert(
        &self,
        end: &CertificateDer<'_>,
        chain: &[CertificateDer<'_>],
        name: &ServerName<'_>,
        ocsp: &[u8],
        now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        if let Some(certificates) = &self.certificates {
            // webpki validates the chain/revocation before checking its name.
            // verify-ca deliberately omits only that final hostname check.
            match certificates.verify_server_cert(end, chain, name, ocsp, now) {
                Err(rustls::Error::InvalidCertificate(
                    rustls::CertificateError::NotValidForName
                    | rustls::CertificateError::NotValidForNameContext { .. },
                )) if !self.hostname => {}
                result => {
                    result?;
                }
            }
            if self.require_crl && self.crls.is_empty() {
                return Err(rustls::Error::InvalidCertificate(
                    rustls::CertificateError::UnknownRevocationStatus,
                ));
            }
            if self.crls.is_empty() {
                return Ok(ServerCertVerified::assertion());
            }
            // webpki checks nextUpdate but currently ignores thisUpdate.
            for certificate in std::iter::once(end).chain(chain) {
                let certificate = x509_cert::Certificate::from_der(certificate)
                    .map_err(|_| rustls::Error::InvalidCertificate(rustls::CertificateError::BadEncoding))?;
                for crl in &self.crls {
                    if crl.tbs_cert_list.issuer == certificate.tbs_certificate.issuer
                        && crl.tbs_cert_list.this_update.to_unix_duration().as_secs() > now.as_secs()
                    {
                        return Err(rustls::Error::General(
                            "certificate revocation list is not yet valid".into(),
                        ));
                    }
                }
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
    let verify = matches!(mode, "verify-ca" | "verify-full") || present;
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
    let mut crls = Vec::new();
    let mut require_crl = false;
    let certificates = if let Some(roots) = roots {
        let default_crl = std::env::var("HOME")
            .ok()
            .map(|h| format!("{h}/.postgresql/root.crl"));
        let path = parameters
            .get("sslcrl")
            .filter(|p| !p.is_empty())
            .or(default_crl.as_deref());
        // Like X509_STORE_load_locations, missing/unreadable files and invalid PEM
        // do not enable revocation checking. Loaded DER is checked strictly.
        let encoded = path
            .and_then(|p| CertificateRevocationListDer::pem_file_iter(p).ok())
            .and_then(|items| items.collect::<std::result::Result<Vec<_>, _>>().ok())
            .unwrap_or_default();
        let parsed = encoded
            .iter()
            .map(|e| x509_cert::crl::CertificateList::from_der(e))
            .collect::<std::result::Result<Vec<_>, _>>();
        match parsed {
            Ok(parsed) => crls = parsed,
            Err(error) => {
                return Err(io(&format!(
                    "could not parse certificate revocation list: {error}"
                )));
            }
        }
        require_crl = !encoded.is_empty()
            || path
                .and_then(|p| CertificateDer::pem_file_iter(p).ok())
                .is_some_and(|mut certs| certs.any(|c| c.is_ok()));
        Some(
            rustls::client::WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider.clone())
                .with_crls(encoded)
                .enforce_revocation_expiration()
                .build()
                .map_err(|e| io(&e))?,
        )
    } else {
        None
    };
    let verifier = Arc::new(Verifier {
        certificates,
        crls,
        require_crl,
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
