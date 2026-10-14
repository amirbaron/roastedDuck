//! DATE values (DuckDB's `date_t` and `Date`).

use crate::{NativeType, PhysicalType};

/// Days since 1970-01-01. `i32::MAX` / `-i32::MAX` encode `infinity` / `-infinity`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct Date(pub i32);

// SAFETY: a transparent `i32`; every bit pattern is a valid value.
unsafe impl NativeType for Date {
    const PHYSICAL: PhysicalType = PhysicalType::Int32;
}

impl Date {
    /// `infinity`.
    pub const INFINITY: Self = Self(i32::MAX);
    /// `-infinity`.
    pub const NEG_INFINITY: Self = Self(-i32::MAX);
    /// 1970-01-01.
    pub const EPOCH: Self = Self(0);

    /// Whether the date is neither `infinity` nor `-infinity`.
    pub const fn is_finite(self) -> bool {
        self.0 != Self::INFINITY.0 && self.0 != Self::NEG_INFINITY.0
    }
}
