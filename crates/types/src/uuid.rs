//! UUID values (DuckDB's `UUID`).

use crate::{NativeType, PhysicalType};

/// A UUID stored like DuckDB: as a signed 128-bit integer whose top bit is flipped, so
/// that signed comparison orders UUIDs like their text form.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct Uuid(pub i128);

// SAFETY: a transparent `i128`; every bit pattern is a valid value.
unsafe impl NativeType for Uuid {
    const PHYSICAL: PhysicalType = PhysicalType::Int128;
}
