//! Physical operator implementations.

mod dummy_scan;
mod projection;
mod result_collector;

pub use dummy_scan::PhysicalDummyScan;
pub use projection::PhysicalProjection;
pub use result_collector::{CollectedResult, PhysicalResultCollector};
