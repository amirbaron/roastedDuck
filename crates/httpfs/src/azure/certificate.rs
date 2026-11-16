//! RSA certificate assertions use the existing Rust ring implementation.
use super::io_error;
use ring::{
    digest,
    rand::{SecureRandom, SystemRandom},
    signature,
};
use roastedduck_common::Result;
use roastedduck_types::blob::{parse_base64, write_base64};
use std::fmt::Write as _;

pub(super) struct Certificate {
    key: signature::RsaKeyPair,
    header: String,
}

fn base64url(bytes: &[u8]) -> String {
    let mut text = Vec::new();
    write_base64(bytes, &mut text);
    String::from_utf8_lossy(&text)
        .trim_end_matches('=')
        .replace('+', "-")
        .replace('/', "_")
}
fn pem(text: &str, label: &str) -> Result<Vec<u8>> {
    let content = text
        .split_once(&format!("-----BEGIN {label}-----"))
        .and_then(|(_, s)| s.split_once(&format!("-----END {label}-----")).map(|(s, _)| s))
        .ok_or_else(|| io_error(format!("Certificate file does not contain {label}")))?;
    let content: Vec<u8> = content.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    let mut bytes = Vec::new();
    parse_base64(&content, &mut bytes).map_err(io_error)?;
    Ok(bytes)
}
impl Certificate {
    pub fn load(path: &str) -> Result<Self> {
        let text = std::fs::read_to_string(path).map_err(io_error)?;
        let cert = pem(&text, "CERTIFICATE")?;
        let key = if text.contains("-----BEGIN PRIVATE KEY-----") {
            signature::RsaKeyPair::from_pkcs8(&pem(&text, "PRIVATE KEY")?).map_err(io_error)?
        } else {
            signature::RsaKeyPair::from_der(&pem(&text, "RSA PRIVATE KEY")?).map_err(io_error)?
        };
        // SHA-1 identifies the certificate in x5t; signatures use RSA/SHA-256.
        let thumbprint = digest::digest(&digest::SHA1_FOR_LEGACY_USE_ONLY, &cert);
        let mut hex = String::new();
        for b in thumbprint.as_ref() {
            let _ = write!(hex, "{b:02X}");
        }
        let header =
            serde_json::json!({"x5t":base64url(thumbprint.as_ref()), "kid":hex, "alg":"RS256", "typ":"JWT"});
        Ok(Self {
            key,
            header: base64url(header.to_string().as_bytes()),
        })
    }
    pub fn assertion(&self, audience: &str, client: &str) -> Result<String> {
        let random = SystemRandom::new();
        let mut uuid = [0u8; 16];
        random.fill(&mut uuid).map_err(io_error)?;
        uuid[6] = (uuid[6] & 15) | 64;
        uuid[8] = (uuid[8] & 63) | 128;
        let mut hex = String::new();
        for b in uuid {
            let _ = write!(hex, "{b:02x}");
        }
        let id = format!(
            "{}-{}-{}-{}-{}",
            &hex[..8],
            &hex[8..12],
            &hex[12..16],
            &hex[16..20],
            &hex[20..]
        );
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let payload =
            serde_json::json!({"aud":audience,"iss":client,"sub":client,"jti":id,"nbf":now,"exp":now+600});
        let message = format!("{}.{}", self.header, base64url(payload.to_string().as_bytes()));
        let mut signature = vec![0u8; self.key.public().modulus_len()];
        self.key
            .sign(
                &signature::RSA_PKCS1_SHA256,
                &random,
                message.as_bytes(),
                &mut signature,
            )
            .map_err(io_error)?;
        Ok(format!("{message}.{}", base64url(&signature)))
    }
}
