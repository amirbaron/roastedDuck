//! `TIME`, `TIME_NS` and `TIME WITH TIME ZONE` values (DuckDB's `dtime_t`, `dtime_ns_t`,
//! `dtime_tz_t` and `Time`).

use crate::{NativeType, PhysicalType};

/// Microseconds since midnight (`TIME`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct Time(pub i64);

/// Nanoseconds since midnight (`TIME_NS`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct TimeNs(pub i64);

/// A time of day plus a UTC offset in seconds (`TIME WITH TIME ZONE`).
///
/// Packed like DuckDB: the upper 40 bits hold the microseconds, the lower 24 bits the
/// biased offset `MAX_OFFSET - offset`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct TimeTz(pub u64);

// SAFETY: transparent 64-bit integers; every bit pattern is a valid value.
unsafe impl NativeType for Time {
    const PHYSICAL: PhysicalType = PhysicalType::Int64;
}
// SAFETY: as above.
unsafe impl NativeType for TimeNs {
    const PHYSICAL: PhysicalType = PhysicalType::Int64;
}
// SAFETY: as above.
unsafe impl NativeType for TimeTz {
    const PHYSICAL: PhysicalType = PhysicalType::Int64;
}

impl TimeTz {
    const OFFSET_BITS: u32 = 24;
    const OFFSET_MASK: u64 = (1 << Self::OFFSET_BITS) - 1;
    /// Largest supported offset: +15:59:59.
    pub const MAX_OFFSET: i32 = 16 * 60 * 60 - 1;
    /// Smallest supported offset: -15:59:59.
    pub const MIN_OFFSET: i32 = -Self::MAX_OFFSET;

    /// Packs a time and an offset (in seconds east of UTC).
    pub const fn new(time: Time, offset: i32) -> Self {
        Self(((time.0 as u64) << Self::OFFSET_BITS) | (Self::MAX_OFFSET - offset) as u64)
    }

    /// The local time of day.
    pub const fn time(self) -> Time {
        Time((self.0 >> Self::OFFSET_BITS) as i64)
    }

    /// The UTC offset in seconds.
    pub const fn offset(self) -> i32 {
        Self::MAX_OFFSET - (self.0 & Self::OFFSET_MASK) as i32
    }

    /// The key DuckDB orders by: the time normalised to UTC, biased to stay unsigned.
    pub const fn sort_key(self) -> u64 {
        self.0 + (((self.0 & Self::OFFSET_MASK) * 1_000_000) << Self::OFFSET_BITS)
    }
}
