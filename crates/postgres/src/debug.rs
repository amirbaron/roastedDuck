//! Pinned process-wide query debugging; pool maintenance is not a query event.
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
static ENABLED: AtomicBool = AtomicBool::new(false);
pub fn set_enabled(enabled: bool) {
    ENABLED.store(enabled, Ordering::SeqCst);
}
pub(crate) fn query(sql: &str) {
    if ENABLED.load(Ordering::SeqCst) {
        let mut stderr = std::io::stderr().lock();
        let _ = writeln!(stderr, "{}\n", sql.split('\0').next().unwrap_or_default());
    }
}
pub(crate) struct Quiet<'a> {
    client: &'a tokio_postgres::Client,
    previous: Option<fn(&str)>,
}
pub(crate) fn quiet(client: &tokio_postgres::Client) -> Quiet<'_> {
    Quiet {
        client,
        previous: client.set_query_observer(None),
    }
}
impl Drop for Quiet<'_> {
    fn drop(&mut self) {
        self.client.set_query_observer(self.previous);
    }
}
