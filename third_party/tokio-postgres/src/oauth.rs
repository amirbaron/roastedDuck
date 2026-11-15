//! Internal adapter for roastedDuck's supplied-token OAUTHBEARER flow.
use std::{error::Error, fmt, sync::Arc};

#[doc(hidden)]
pub trait OAuthHandler: Send + Sync {
    fn initial_response(&self) -> Result<Vec<u8>, Box<dyn Error + Send + Sync>>;
    /// Validate the challenge and request one reconnect after discovery.
    fn challenge(&self, data: &[u8]) -> Result<bool, Box<dyn Error + Send + Sync>>;
    fn authenticated(&self) -> bool;
}
#[derive(Clone)]
pub(crate) struct Handler(pub Arc<dyn OAuthHandler>);
impl fmt::Debug for Handler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("OAuthHandler([REDACTED])")
    }
}
impl PartialEq for Handler {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for Handler {}
#[derive(Debug)]
pub(crate) struct Reconnect;
impl fmt::Display for Reconnect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("OAuth discovery completed")
    }
}
impl Error for Reconnect {}
