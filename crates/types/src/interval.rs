//! INTERVAL values (DuckDB's `interval_t` and `Interval`).

use crate::{NativeType, PhysicalType};

/// A duration in months, days and microseconds; the parts are independent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(C)]
pub struct Interval {
    pub months: i32,
    pub days: i32,
    pub micros: i64,
}

const _: () = assert!(size_of::<Interval>() == 16);

// SAFETY: `repr(C)` of two `i32`s and an `i64` without padding; every bit pattern is valid.
unsafe impl NativeType for Interval {
    const PHYSICAL: PhysicalType = PhysicalType::Interval;
}

impl Interval {
    pub const MONTHS_PER_YEAR: i64 = 12;
    pub const DAYS_PER_MONTH: i64 = 30;
    pub const MICROS_PER_MSEC: i64 = 1_000;
    pub const MICROS_PER_SEC: i64 = 1_000_000;
    pub const MICROS_PER_MINUTE: i64 = 60 * Self::MICROS_PER_SEC;
    pub const MICROS_PER_HOUR: i64 = 60 * Self::MICROS_PER_MINUTE;
    pub const MICROS_PER_DAY: i64 = 24 * Self::MICROS_PER_HOUR;
    pub const NANOS_PER_MICRO: i64 = 1_000;
    pub const NANOS_PER_MSEC: i64 = 1_000_000;
    pub const NANOS_PER_SEC: i64 = 1_000_000_000;
    pub const SECS_PER_MINUTE: i64 = 60;
    pub const SECS_PER_HOUR: i64 = 3_600;
    pub const SECS_PER_DAY: i64 = 86_400;

    pub const fn new(months: i32, days: i32, micros: i64) -> Self {
        Self { months, days, micros }
    }
}
