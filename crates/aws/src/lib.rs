//! AWS credential provider integration. SQL binding belongs to the engine crate.
//!
//! Provider ordering is owned here, not delegated to the SDK's evolving default
//! chain. The transport uses the engine's existing ring dependency; the SDK's
//! default AWS-LC native crypto provider is deliberately not enabled.

use std::fmt;
use std::time::SystemTime;

mod process;
mod rds;
mod resolve;
mod sso;
pub use rds::{rds_auth_token, rds_unsigned_token};
pub use resolve::{Request, Resolution, ResolveError, resolve, resolve_configuration};

/// Credential material is never included in Debug output.
#[derive(Clone)]
pub struct Credentials {
    pub access_key: String,
    pub secret_key: String,
    pub session_token: Option<String>,
    pub expires_at: Option<SystemTime>,
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credentials")
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

impl From<aws_credential_types::Credentials> for Credentials {
    fn from(credentials: aws_credential_types::Credentials) -> Self {
        Self {
            access_key: credentials.access_key_id().to_owned(),
            secret_key: credentials.secret_access_key().to_owned(),
            session_token: credentials.session_token().map(str::to_owned),
            expires_at: credentials.expiry(),
        }
    }
}

/// Explicit chain components accepted by the pinned extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Environment,
    Config,
    Process,
    WebIdentity,
    Sso,
    Instance,
    Sts,
}

impl Provider {
    pub fn parse(name: &str) -> Result<Self, String> {
        match name {
            "env" => Ok(Self::Environment),
            "config" => Ok(Self::Config),
            "process" => Ok(Self::Process),
            "web_identity" => Ok(Self::WebIdentity),
            "sso" => Ok(Self::Sso),
            "instance" => Ok(Self::Instance),
            "sts" => Ok(Self::Sts),
            _ => Err(format!(
                "Unknown provider found while parsing AWS credential chain string: '{name}'"
            )),
        }
    }
}
