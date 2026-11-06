//! Observable map order follows the platform C++ reference library.
#[cfg(not(target_os = "linux"))]
pub use crate::libcxx::{UnorderedOrder, string_hash};
#[cfg(target_os = "linux")]
pub use crate::libstdcxx::{UnorderedOrder, string_hash};
