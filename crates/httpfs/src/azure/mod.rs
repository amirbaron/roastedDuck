//! roastedDuck's compatible Azure filesystem implementation.
//! Uses the shared Rust transport; no Azure C++ SDK or extension binary is loaded.
use roastedduck_common::{Error, Result};
use roastedduck_types::Value;
use std::collections::BTreeMap;
use std::sync::Arc;

mod blob;
mod client;
mod config;
mod url;
use super::io_error;
pub use blob::AzureBlobFileSystem;
pub use config::options;

pub const SECRET_RESOLVER_KEY: &str = "roastedduck.azure_secret_resolver";

#[derive(Clone)]
pub struct AzureSecret {
    pub name: String,
    pub provider: String,
    pub values: BTreeMap<String, Value>,
}
impl std::fmt::Debug for AzureSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AzureSecret")
            .field("provider", &self.provider)
            .finish_non_exhaustive()
    }
}
pub trait ResolveAzureSecret: std::fmt::Debug + Send + Sync {
    fn resolve(&self, path: &str) -> Result<Option<AzureSecret>>;
    fn user_agent(&self) -> String;
}
#[derive(Debug)]
pub struct SecretResolver(pub Arc<dyn ResolveAzureSecret>);

fn setting(name: &str, fallback: &str) -> String {
    roastedduck_function::file_system::opener()
        .and_then(|c| c.settings())
        .and_then(|s| s.current_setting(name).ok())
        .filter(|v| !v.is_null())
        .and_then(|v| v.to_varchar().ok())
        .unwrap_or_else(|| fallback.into())
}

fn check_interrupt() -> Result<()> {
    if roastedduck_function::file_system::opener()
        .and_then(|c| c.interrupt_flag())
        .is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Relaxed))
    {
        return Err(Error::new(
            roastedduck_common::ErrorKind::Interrupt,
            "Interrupted!",
        ));
    }
    Ok(())
}
