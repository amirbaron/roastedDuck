//! TIMESTAMP values (DuckDB's `timestamp_t` and `Timestamp`).
//!
//! `TIMESTAMP` and `TIMESTAMP WITH TIME ZONE` store microseconds since the epoch;
//! `TIMESTAMP_S`, `TIMESTAMP_MS` and `TIMESTAMP_NS` store seconds, milliseconds and
//! nanoseconds as plain `i64`s with the same infinity encoding.

use crate::{NativeType, PhysicalType};

/// Microseconds since 1970-01-01 00:00:00 (UTC for `TIMESTAMP WITH TIME ZONE`).
/// `i64::MAX` / `-i64::MAX` encode `infinity` / `-infinity`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct Timestamp(pub i64);

// SAFETY: a transparent `i64`; every bit pattern is a valid value.
unsafe impl NativeType for Timestamp {
    const PHYSICAL: PhysicalType = PhysicalType::Int64;
}

impl Timestamp {
    /// `infinity`.
    pub const INFINITY: Self = Self(i64::MAX);
    /// `-infinity`.
    pub const NEG_INFINITY: Self = Self(-i64::MAX);
    /// 1970-01-01 00:00:00.
    pub const EPOCH: Self = Self(0);

    /// Whether the timestamp is neither `infinity` nor `-infinity`.
    pub const fn is_finite(self) -> bool {
        self.0 != Self::INFINITY.0 && self.0 != Self::NEG_INFINITY.0
    }
}
