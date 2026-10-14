//! Text to number parsing (DuckDB's `integer_cast_operator.hpp`, the HUGEINT string cast in
//! `cast_operators.cpp`, `TryDecimalStringCast` and `TryDoubleCast`).
//!
//! DuckDB parses every integer-like target with one scanner (`TryIntegerCast`) that calls
//! back into a target-specific *state* for digits, decimals and exponents. The same split is
//! kept here: [`scan`] is the scanner and [`NumberState`] the callbacks, so that integers,
//! integers with decimals/exponents, HUGEINTs and DECIMALs accept exactly the same syntax
//! as DuckDB (underscores between digits, `0x`/`0b` prefixes, `.5`, `1.`, `1e3`, trailing
//! whitespace) and round exactly like it.

use rustdb_types::format::is_space;

/// The per-target callbacks of the number scanner (DuckDB's `*CastOperation` structs).
///
/// `NEG` tells whether the number started with `-`; negative numbers accumulate downwards
/// so that the most negative value of each type parses without overflow.
pub(super) trait NumberState {
    fn digit<const NEG: bool>(&mut self, digit: u8) -> bool;
    fn hex_digit(&mut self, _digit: u8) -> bool {
        false
    }
    fn binary_digit(&mut self, _digit: u8) -> bool {
        false
    }
    fn exponent<const NEG: bool>(&mut self, _exponent: i16) -> bool {
        false
    }
    fn decimal<const NEG: bool>(&mut self, _digit: u8) -> bool {
        false
    }
    fn finalize<const NEG: bool>(&mut self) -> bool {
        true
    }
}

/// Integers that the scanner accumulates into.
pub(super) trait ScanInt: Copy + Default + PartialEq + PartialOrd {
    const ZERO: Self;
    fn mul_add(self, base: u8, digit: u8) -> Option<Self>;
    fn mul_sub(self, base: u8, digit: u8) -> Option<Self>;
}

macro_rules! scan_int {
    ($($ty:ty),*) => {$(
        impl ScanInt for $ty {
            const ZERO: Self = 0;
            #[inline]
            fn mul_add(self, base: u8, digit: u8) -> Option<Self> {
                self.checked_mul(base as Self)?.checked_add(digit as Self)
            }
            #[inline]
            fn mul_sub(self, base: u8, digit: u8) -> Option<Self> {
                self.checked_mul(base as Self)?.checked_sub(digit as Self)
            }
        }
    )*};
}
scan_int!(i8, i16, i32, i64, i128, u8, u16, u32, u64, u128);

/// Plain integers (DuckDB's `IntegerCastData` + `IntegerCastOperation`).
#[derive(Default)]
pub(super) struct IntegerState<T> {
    pub(super) result: T,
}

impl<T: ScanInt> NumberState for IntegerState<T> {
    #[inline]
    fn digit<const NEG: bool>(&mut self, digit: u8) -> bool {
        let next = if NEG {
            self.result.mul_sub(10, digit)
        } else {
            self.result.mul_add(10, digit)
        };
        next.map(|next| self.result = next).is_some()
    }

    fn hex_digit(&mut self, digit: u8) -> bool {
        self.result
            .mul_add(16, digit)
            .map(|next| self.result = next)
            .is_some()
    }

    fn binary_digit(&mut self, digit: u8) -> bool {
        self.result
            .mul_add(2, digit)
            .map(|next| self.result = next)
            .is_some()
    }
}

/// Integers written with decimals or an exponent, rounded half away from zero
/// (DuckDB's `IntegerDecimalCastData` + `IntegerDecimalCastOperation`). Accumulates in
/// `i64` (`u64` for UBIGINT) and narrows in `finalize`.
pub(super) struct IntegerDecimalState<S, T> {
    result: S,
    decimal: S,
    decimal_digits: u16,
    pub(super) value: T,
}

impl<S: ScanInt, T: Default> Default for IntegerDecimalState<S, T> {
    fn default() -> Self {
        Self {
            result: S::ZERO,
            decimal: S::ZERO,
            decimal_digits: 0,
            value: T::default(),
        }
    }
}

/// The 64-bit store of [`IntegerDecimalState`].
pub(super) trait DecimalStore: ScanInt {
    const MAX_DIGITS: u16;
    fn rem10(self) -> Self;
    fn div10(self) -> Self;
    fn abs(self) -> Self;
    fn pow10(exponent: u32) -> Self;
    fn split_div(self, divisor: Self) -> (Self, Self);
    fn add(self, other: Self) -> Option<Self>;
    fn sub(self, other: Self) -> Option<Self>;
    fn ge_five(self) -> bool;
    fn gt_ten(self) -> bool;
}

macro_rules! decimal_store {
    ($($ty:ty => $digits:expr),*) => {$(
        impl DecimalStore for $ty {
            const MAX_DIGITS: u16 = $digits;
            fn rem10(self) -> Self { self % 10 }
            fn div10(self) -> Self { self / 10 }
            #[allow(clippy::cast_lossless, unused_comparisons)]
            fn abs(self) -> Self { if self < 0 { (0 as $ty).wrapping_sub(self) } else { self } }
            fn pow10(exponent: u32) -> Self { (10 as $ty).wrapping_pow(exponent) }
            fn split_div(self, divisor: Self) -> (Self, Self) { (self / divisor, self % divisor) }
            fn add(self, other: Self) -> Option<Self> { self.checked_add(other) }
            fn sub(self, other: Self) -> Option<Self> { self.checked_sub(other) }
            fn ge_five(self) -> bool { self >= 5 }
            fn gt_ten(self) -> bool { self > 10 }
        }
    )*};
}
decimal_store!(i64 => 19, u64 => 20);

impl<S, T> NumberState for IntegerDecimalState<S, T>
where
    S: DecimalStore,
    T: Copy + Default + TryFrom<S> + ScanInt,
{
    #[inline]
    fn digit<const NEG: bool>(&mut self, digit: u8) -> bool {
        let next = if NEG {
            self.result.mul_sub(10, digit)
        } else {
            self.result.mul_add(10, digit)
        };
        next.map(|next| self.result = next).is_some()
    }

    fn exponent<const NEG: bool>(&mut self, exponent: i16) -> bool {
        let mut e = exponent;
        if e < 0 {
            while e < 0 {
                e += 1;
                self.decimal = self.result.rem10();
                self.result = self.result.div10();
                if self.result == S::ZERO && self.decimal == S::ZERO {
                    break;
                }
            }
            self.decimal = self.decimal.abs();
            self.decimal_digits = 1;
            return self.finalize::<NEG>();
        }
        while self.result != S::ZERO && e > 0 {
            e -= 1;
            match self.result.mul_add(10, 0) {
                Some(next) => self.result = next,
                None => return false,
            }
        }
        if self.decimal == S::ZERO {
            return self.finalize::<NEG>();
        }
        let shift = i32::from(exponent) - i32::from(self.decimal_digits);
        let mut remainder = S::ZERO;
        if shift < 0 {
            if shift.unsigned_abs() <= u32::from(S::MAX_DIGITS) {
                let (quotient, rest) = self.decimal.split_div(S::pow10(shift.unsigned_abs()));
                self.decimal = quotient;
                remainder = rest;
            } else {
                self.decimal = S::ZERO;
            }
        } else {
            for _ in 0..shift {
                match self.decimal.mul_add(10, 0) {
                    Some(next) => self.decimal = next,
                    None => return false,
                }
            }
        }
        self.decimal_digits = self.decimal_digits.wrapping_sub(exponent as u16);
        let combined = if NEG {
            self.result.sub(self.decimal)
        } else {
            self.result.add(self.decimal)
        };
        match combined {
            Some(next) => self.result = next,
            None => return false,
        }
        self.decimal = remainder;
        self.finalize::<NEG>()
    }

    fn decimal<const NEG: bool>(&mut self, digit: u8) -> bool {
        // Digits that no longer fit are ignored.
        if let Some(next) = self.decimal.mul_add(10, digit) {
            self.decimal_digits += 1;
            self.decimal = next;
        }
        true
    }

    fn finalize<const NEG: bool>(&mut self) -> bool {
        let Ok(mut value) = T::try_from(self.result) else {
            return false;
        };
        while self.decimal.gt_ten() {
            self.decimal = self.decimal.div10();
            self.decimal_digits = self.decimal_digits.wrapping_sub(1);
        }
        if self.decimal_digits == 1 && self.decimal.ge_five() {
            let rounded = if NEG {
                value.mul_sub(1, 1)
            } else {
                value.mul_add(1, 1)
            };
            match rounded {
                Some(next) => value = next,
                None => return false,
            }
        }
        self.value = value;
        true
    }
}

/// 128-bit integers (DuckDB's `HugeIntCastData` + `HugeIntegerCastOperation`): digits are
/// gathered in a 64-bit intermediate and flushed with one 128-bit multiplication per 18
/// digits; decimals and exponents round half away from zero.
pub(super) struct WideIntState<W, I> {
    pub(super) result: W,
    intermediate: I,
    digits: u32,
    decimal: W,
    decimal_total_digits: u32,
    decimal_intermediate: W,
    decimal_intermediate_digits: u32,
}

/// The 128-bit result types of [`WideIntState`] with their 64-bit intermediates.
pub(super) trait WideInt: ScanInt {
    type Intermediate: ScanInt + Into<Self>;
    const I64_MAX: Self;
    fn pow10(exponent: u32) -> Self;
    fn checked_mul_w(self, other: Self) -> Option<Self>;
    fn checked_add_w(self, other: Self) -> Option<Self>;
    fn checked_sub_w(self, other: Self) -> Option<Self>;
    fn div_rem(self, divisor: Self) -> (Self, Self);
    fn checked_neg_w(self) -> Option<Self>;
    fn is_negative(self) -> bool;
    fn from_u8(value: u8) -> Self;
}

macro_rules! wide_int {
    ($($ty:ty: $inter:ty),*) => {$(
        impl WideInt for $ty {
            type Intermediate = $inter;
            const I64_MAX: Self = i64::MAX as $ty;
            fn pow10(exponent: u32) -> Self { (10 as $ty).pow(exponent) }
            fn checked_mul_w(self, other: Self) -> Option<Self> { self.checked_mul(other) }
            fn checked_add_w(self, other: Self) -> Option<Self> { self.checked_add(other) }
            fn checked_sub_w(self, other: Self) -> Option<Self> { self.checked_sub(other) }
            fn div_rem(self, divisor: Self) -> (Self, Self) { (self / divisor, self % divisor) }
            fn checked_neg_w(self) -> Option<Self> { (0 as $ty).checked_sub(self) }
            #[allow(unused_comparisons)]
            fn is_negative(self) -> bool { self < 0 }
            fn from_u8(value: u8) -> Self { value.into() }
        }
    )*};
}
wide_int!(i128: i64, u128: u64);

impl<W: WideInt> Default for WideIntState<W, W::Intermediate> {
    fn default() -> Self {
        Self {
            result: W::ZERO,
            intermediate: W::Intermediate::ZERO,
            digits: 0,
            decimal: W::ZERO,
            decimal_total_digits: 0,
            decimal_intermediate: W::ZERO,
            decimal_intermediate_digits: 0,
        }
    }
}

impl<W: WideInt> WideIntState<W, W::Intermediate> {
    /// The largest power of ten that is cached (`Hugeint::POWERS_OF_TEN`).
    const MAX_DIGITS: u32 = 38;

    fn flush(&mut self) -> bool {
        if self.digits == 0 && self.intermediate == W::Intermediate::ZERO {
            return true;
        }
        if self.result != W::ZERO {
            if self.digits > Self::MAX_DIGITS {
                return false;
            }
            match self.result.checked_mul_w(W::pow10(self.digits)) {
                Some(next) => self.result = next,
                None => return false,
            }
        }
        match self.result.checked_add_w(self.intermediate.into()) {
            Some(next) => self.result = next,
            None => return false,
        }
        self.digits = 0;
        self.intermediate = W::Intermediate::ZERO;
        true
    }

    fn flush_decimal(&mut self) -> bool {
        if self.decimal_intermediate_digits == 0 && self.decimal_intermediate == W::ZERO {
            return true;
        }
        if self.decimal != W::ZERO {
            if self.decimal_intermediate_digits > Self::MAX_DIGITS {
                return false;
            }
            match self
                .decimal
                .checked_mul_w(W::pow10(self.decimal_intermediate_digits))
            {
                Some(next) => self.decimal = next,
                None => return false,
            }
        }
        match self.decimal.checked_add_w(self.decimal_intermediate) {
            Some(next) => self.decimal = next,
            None => return false,
        }
        self.decimal_total_digits += self.decimal_intermediate_digits;
        self.decimal_intermediate_digits = 0;
        self.decimal_intermediate = W::ZERO;
        true
    }

    fn round_away<const NEG: bool>(&mut self) -> bool {
        let next = if NEG {
            self.result.checked_sub_w(W::from_u8(1))
        } else {
            self.result.checked_add_w(W::from_u8(1))
        };
        next.map(|next| self.result = next).is_some()
    }
}

impl<W: WideInt> NumberState for WideIntState<W, W::Intermediate> {
    #[inline]
    fn digit<const NEG: bool>(&mut self, digit: u8) -> bool {
        let next = if NEG {
            self.intermediate.mul_sub(10, digit)
        } else {
            self.intermediate.mul_add(10, digit)
        };
        if let Some(next) = next {
            self.intermediate = next;
        } else {
            // The intermediate is full: flush it into the result first.
            if !self.flush() {
                return false;
            }
            self.intermediate = if NEG {
                W::Intermediate::ZERO.mul_sub(1, digit)
            } else {
                W::Intermediate::ZERO.mul_add(1, digit)
            }
            .unwrap_or(W::Intermediate::ZERO);
        }
        self.digits += 1;
        true
    }

    fn exponent<const NEG: bool>(&mut self, exponent: i16) -> bool {
        if !self.flush() {
            return false;
        }
        let e = i32::from(exponent);
        if e < -38 {
            self.result = W::ZERO;
            return true;
        }
        let mut remainder = W::ZERO;
        if e < 0 {
            let (quotient, rest) = self.result.div_rem(W::pow10(e.unsigned_abs()));
            self.result = quotient;
            remainder = rest;
            if remainder.is_negative() {
                match remainder.checked_neg_w() {
                    Some(positive) => remainder = positive,
                    None => return false,
                }
            }
            self.decimal = remainder;
            self.decimal_total_digits = e.unsigned_abs();
            self.decimal_intermediate = W::ZERO;
            self.decimal_intermediate_digits = 0;
            return self.finalize::<NEG>();
        }
        if self.result != W::ZERO {
            if e > 38 {
                return false;
            }
            match self.result.checked_mul_w(W::pow10(e.unsigned_abs())) {
                Some(next) => self.result = next,
                None => return false,
            }
        }
        if !self.flush_decimal() {
            return false;
        }
        if self.decimal == W::ZERO {
            return self.finalize::<NEG>();
        }
        let shift = e - self.decimal_total_digits as i32;
        if shift < 0 {
            if shift < -38 {
                return false;
            }
            let (quotient, rest) = self.decimal.div_rem(W::pow10(shift.unsigned_abs()));
            self.decimal = quotient;
            remainder = rest;
            self.decimal_total_digits = self.decimal_total_digits.wrapping_sub(e.unsigned_abs());
        } else {
            if shift > 38 {
                return false;
            }
            match self.decimal.checked_mul_w(W::pow10(shift.unsigned_abs())) {
                Some(next) => self.decimal = next,
                None => return false,
            }
        }
        let combined = if NEG {
            self.result.checked_sub_w(self.decimal)
        } else {
            self.result.checked_add_w(self.decimal)
        };
        match combined {
            Some(next) => self.result = next,
            None => return false,
        }
        self.decimal = remainder;
        self.finalize::<NEG>()
    }

    fn decimal<const NEG: bool>(&mut self, digit: u8) -> bool {
        if !self.flush() {
            return false;
        }
        // DuckDB flushes once `decimal_intermediate > (INT64_MAX - digit) / 10`, for both
        // HUGEINT and UHUGEINT.
        let limit = W::I64_MAX;
        let fits = self
            .decimal_intermediate
            .mul_add(10, digit)
            .is_some_and(|next| next <= limit);
        if !fits && !self.flush_decimal() {
            return false;
        }
        match self.decimal_intermediate.mul_add(10, digit) {
            Some(next) => self.decimal_intermediate = next,
            None => return false,
        }
        self.decimal_intermediate_digits += 1;
        true
    }

    fn finalize<const NEG: bool>(&mut self) -> bool {
        if !self.flush() || !self.flush_decimal() {
            return false;
        }
        if self.decimal == W::ZERO || self.decimal_total_digits == 0 {
            return true;
        }
        // Keep only the first (left-most) decimal digit.
        while self.decimal_total_digits > Self::MAX_DIGITS {
            self.decimal = self.decimal.div_rem(W::pow10(Self::MAX_DIGITS)).0;
            self.decimal_total_digits -= Self::MAX_DIGITS;
        }
        self.decimal = self.decimal.div_rem(W::pow10(self.decimal_total_digits - 1)).0;
        if self.decimal >= W::from_u8(5) {
            return self.round_away::<NEG>();
        }
        true
    }
}

/// How an exponent appeared in a DECIMAL literal.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ExponentKind {
    None,
    Positive,
    Negative,
}

/// DECIMAL(width, scale) values (DuckDB's `DecimalCastData` + `DecimalCastOperation`),
/// accumulated as unscaled `i128`; `max_digits` is the precision of the physical storage.
pub(super) struct DecimalState {
    pub(super) result: i128,
    width: u8,
    scale: u8,
    max_digits: u8,
    digit_count: u8,
    decimal_count: u8,
    round_set: bool,
    should_round: bool,
    excessive_decimals: u8,
    exponent_kind: ExponentKind,
    limit: i128,
    /// The largest value of the physical storage (INT16 .. INT128).
    store_max: i128,
}

impl DecimalState {
    pub(super) fn new(width: u8, scale: u8) -> Self {
        let (max_digits, store_max) = match width {
            0..=4 => (4, i128::from(i16::MAX)),
            5..=9 => (9, i128::from(i32::MAX)),
            10..=18 => (18, i128::from(i64::MAX)),
            _ => (38, i128::MAX),
        };
        Self {
            result: 0,
            width,
            scale,
            max_digits,
            digit_count: 0,
            decimal_count: 0,
            round_set: false,
            should_round: false,
            excessive_decimals: 0,
            exponent_kind: ExponentKind::None,
            limit: 10i128.pow(u32::from(width)),
            store_max,
        }
    }

    fn round_up<const NEG: bool>(&mut self) {
        self.result += if NEG { -1 } else { 1 };
    }

    fn truncate_excessive_decimals<const NEG: bool>(&mut self) {
        let mut round_up = false;
        for _ in 0..self.excessive_decimals {
            let digit = self.result % 10;
            round_up = if NEG { digit <= -5 } else { digit >= 5 };
            self.result /= 10;
        }
        // Only round up when exponents are involved.
        if self.exponent_kind == ExponentKind::Positive && round_up {
            self.round_up::<NEG>();
        }
        self.decimal_count = self.scale;
    }
}

impl NumberState for DecimalState {
    fn digit<const NEG: bool>(&mut self, digit: u8) -> bool {
        if self.result == 0 && digit == 0 {
            // Leading zeros do not count towards the width.
            return true;
        }
        if self.digit_count == self.width - self.scale {
            return false;
        }
        // The storage type must not overflow (it can once an exponent appends zeros).
        let store_min = -self.store_max - 1;
        if (NEG && self.result < store_min / 10) || (!NEG && self.result > self.store_max / 10) {
            return false;
        }
        self.digit_count += 1;
        self.result = self.result * 10
            + if NEG {
                -i128::from(digit)
            } else {
                i128::from(digit)
            };
        true
    }

    fn exponent<const NEG: bool>(&mut self, exponent: i16) -> bool {
        let mut exponent = i32::from(exponent);
        let decimal_excess = i32::from(self.decimal_count.saturating_sub(self.scale));
        if exponent > 0 {
            self.exponent_kind = ExponentKind::Positive;
            // Positive exponents consume the extra decimals first.
            if decimal_excess > exponent {
                self.excessive_decimals = (decimal_excess - exponent) as u8;
                exponent = 0;
            } else {
                exponent -= decimal_excess;
            }
        } else if exponent < 0 {
            self.exponent_kind = ExponentKind::Negative;
        }
        if !self.finalize::<NEG>() {
            return false;
        }
        if exponent < 0 {
            let mut round_up = false;
            for _ in 0..exponent.unsigned_abs() {
                let digit = self.result % 10;
                round_up = if NEG { digit <= -5 } else { digit >= 5 };
                self.result /= 10;
                if self.result == 0 {
                    break;
                }
            }
            if round_up {
                self.round_up::<NEG>();
            }
            true
        } else {
            (0..exponent).all(|_| self.digit::<NEG>(0))
        }
    }

    fn decimal<const NEG: bool>(&mut self, digit: u8) -> bool {
        if self.decimal_count == self.scale && !self.round_set {
            self.round_set = true;
            self.should_round = digit >= 5;
        }
        // Keep the decimals for a possible exponent, but never overflow the storage.
        if self.digit_count + self.decimal_count >= self.max_digits {
            return true;
        }
        self.decimal_count += 1;
        self.result = self.result * 10
            + if NEG {
                -i128::from(digit)
            } else {
                i128::from(digit)
            };
        true
    }

    fn finalize<const NEG: bool>(&mut self) -> bool {
        if self.exponent_kind != ExponentKind::Positive && self.decimal_count > self.scale {
            self.excessive_decimals = self.decimal_count - self.scale;
        }
        if self.excessive_decimals > 0 {
            self.truncate_excessive_decimals::<NEG>();
        }
        if self.exponent_kind == ExponentKind::None && self.round_set && self.should_round {
            self.round_up::<NEG>();
        }
        for _ in self.decimal_count..self.scale {
            self.result *= 10;
        }
        if NEG {
            self.result > -self.limit
        } else {
            self.result < self.limit
        }
    }
}

const fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Parses `buf` (already trimmed on the left) into `state` (DuckDB's `IntegerCastLoop`).
fn scan_decimal_digits<S: NumberState, const NEG: bool, const ALLOW_EXPONENT: bool>(
    buf: &[u8],
    state: &mut S,
    strict: bool,
) -> bool {
    let start = if NEG {
        1
    } else if buf.first() == Some(&b'+') {
        if strict {
            return false;
        }
        1
    } else {
        0
    };
    let len = buf.len();
    let mut pos = start;
    while pos < len {
        let byte = buf[pos];
        if !byte.is_ascii_digit() {
            let mut byte = byte;
            if byte == b'.' {
                if strict {
                    return false;
                }
                let number_before_period = pos > start;
                pos += 1;
                let start_digit = pos;
                while pos < len && buf[pos].is_ascii_digit() {
                    if !state.decimal::<NEG>(buf[pos] - b'0') {
                        return false;
                    }
                    pos += 1;
                    if pos != len && buf[pos] == b'_' {
                        pos += 1;
                        if pos == len || !buf[pos].is_ascii_digit() {
                            return false;
                        }
                    }
                }
                // Accept "1." and ".1", but not ".".
                if !(number_before_period || pos > start_digit) {
                    return false;
                }
                if pos >= len {
                    break;
                }
                byte = buf[pos];
            }
            if is_space(byte) {
                // Only whitespace may follow; like DuckDB, a lone sign followed by spaces
                // counts as a number (zero).
                return buf[pos..].iter().all(|&b| is_space(b)) && state.finalize::<NEG>() && len > start;
            }
            if ALLOW_EXPONENT && (byte == b'e' || byte == b'E') {
                if strict || pos == start {
                    return false;
                }
                pos += 1;
                if pos >= len {
                    return false;
                }
                let mut exponent = IntegerState::<i16>::default();
                let rest = &buf[pos..];
                let parsed = if rest[0] == b'-' {
                    scan_decimal_digits::<_, true, false>(rest, &mut exponent, strict)
                } else {
                    scan_decimal_digits::<_, false, false>(rest, &mut exponent, strict)
                };
                return parsed && state.exponent::<NEG>(exponent.result);
            }
            return false;
        }
        if !state.digit::<NEG>(byte - b'0') {
            return false;
        }
        pos += 1;
        if pos != len && buf[pos] == b'_' && !strict {
            // Skip one underscore between digits.
            pos += 1;
            if pos == len || !buf[pos].is_ascii_digit() {
                return false;
            }
        }
    }
    state.finalize::<NEG>() && pos > start
}

/// Parses the digits after a `0x`/`0b` prefix (DuckDB's `IntegerHexCastLoop` /
/// `IntegerBinaryCastLoop`); `digits` excludes the prefix.
fn scan_radix_digits<S: NumberState>(digits: &[u8], state: &mut S, hex: bool) -> bool {
    let valid = |byte: u8| {
        if hex {
            hex_value(byte)
        } else {
            matches!(byte, b'0' | b'1').then(|| byte - b'0')
        }
    };
    let mut pos = 0;
    while pos < digits.len() {
        let Some(digit) = valid(digits[pos]) else {
            return false;
        };
        pos += 1;
        if pos != digits.len() && digits[pos] == b'_' {
            pos += 1;
            if pos == digits.len() || valid(digits[pos]).is_none() {
                return false;
            }
        }
        let accepted = if hex {
            state.hex_digit(digit)
        } else {
            state.binary_digit(digit)
        };
        if !accepted {
            return false;
        }
    }
    state.finalize::<false>() && !digits.is_empty()
}

/// DuckDB's `TryIntegerCast`: leading whitespace, sign handling (unsigned targets only
/// accept `-0`), `0x`/`0b` prefixes and strict-mode restrictions.
pub(super) fn scan<S: NumberState, const SIGNED: bool, const ALLOW_EXPONENT: bool>(
    input: &[u8],
    state: &mut S,
    strict: bool,
) -> bool {
    let start = input.iter().position(|&b| !is_space(b)).unwrap_or(input.len());
    let buf = &input[start..];
    let Some(&first) = buf.first() else {
        return false;
    };
    if first == b'-' {
        if !SIGNED && buf[1..].iter().any(|&b| b != b'0') {
            return false;
        }
        return scan_decimal_digits::<S, true, ALLOW_EXPONENT>(buf, state, strict);
    }
    if buf.len() > 1 && first == b'0' {
        match buf[1] {
            b'x' | b'X' => return scan_radix_digits(&buf[2..], state, true),
            b'b' | b'B' => return scan_radix_digits(&buf[2..], state, false),
            next if strict && next.is_ascii_digit() => return false,
            _ => {}
        }
    }
    scan_decimal_digits::<S, false, ALLOW_EXPONENT>(buf, state, strict)
}

/// Integer types parsed from text (DuckDB's `TrySimpleIntegerCast`).
pub(super) trait ParseInteger: Sized {
    fn parse_text(input: &[u8], strict: bool) -> Option<Self>;
}

macro_rules! parse_integer {
    ($($ty:ty: $signed:expr, $store:ty),*) => {$(
        impl ParseInteger for $ty {
            #[inline]
            fn parse_text(input: &[u8], strict: bool) -> Option<Self> {
                let mut simple = IntegerState::<$ty>::default();
                if scan::<_, $signed, false>(input, &mut simple, strict) {
                    return Some(simple.result);
                }
                // Retry, allowing decimals and exponents.
                let mut state = IntegerDecimalState::<$store, $ty>::default();
                scan::<_, $signed, true>(input, &mut state, strict).then_some(state.value)
            }
        }
    )*};
}
parse_integer!(
    i8: true, i64, i16: true, i64, i32: true, i64, i64: true, i64,
    u8: false, i64, u16: false, i64, u32: false, i64, u64: false, u64
);

impl ParseInteger for i128 {
    fn parse_text(input: &[u8], strict: bool) -> Option<Self> {
        let mut state = WideIntState::<i128, i64>::default();
        scan::<_, true, true>(input, &mut state, strict).then_some(state.result)
    }
}

impl ParseInteger for u128 {
    fn parse_text(input: &[u8], strict: bool) -> Option<Self> {
        let mut state = WideIntState::<u128, u64>::default();
        scan::<_, false, true>(input, &mut state, strict).then_some(state.result)
    }
}

/// Parses a DECIMAL(width, scale) literal into its unscaled value
/// (DuckDB's `TryDecimalStringCast`).
pub(super) fn parse_decimal(input: &[u8], width: u8, scale: u8) -> Option<i128> {
    let mut state = DecimalState::new(width, scale);
    scan::<_, true, true>(input, &mut state, false).then_some(state.result)
}

/// DuckDB's `TryCastStringBool`.
pub(super) fn parse_bool(input: &[u8], strict: bool) -> Option<bool> {
    let lower = |i: usize| input[i].to_ascii_lowercase();
    match input.len() {
        1 => match lower(0) {
            b't' => Some(true),
            b'y' | b'1' if !strict => Some(true),
            b'f' => Some(false),
            b'n' | b'0' if !strict => Some(false),
            _ => None,
        },
        2 => input.eq_ignore_ascii_case(b"no").then_some(false),
        3 => input.eq_ignore_ascii_case(b"yes").then_some(true),
        4 => input.eq_ignore_ascii_case(b"true").then_some(true),
        5 => input.eq_ignore_ascii_case(b"false").then_some(false),
        _ => None,
    }
}

/// Floating point types parsed from text.
pub(super) trait ParseFloat: Sized + std::str::FromStr {
    const INFINITY: Self;
    const NEG_INFINITY: Self;
    const NAN: Self;
    const NEG_NAN: Self;
}

impl ParseFloat for f32 {
    const INFINITY: Self = f32::INFINITY;
    const NEG_INFINITY: Self = f32::NEG_INFINITY;
    const NAN: Self = f32::NAN;
    const NEG_NAN: Self = -f32::NAN;
}

impl ParseFloat for f64 {
    const INFINITY: Self = f64::INFINITY;
    const NEG_INFINITY: Self = f64::NEG_INFINITY;
    const NAN: Self = f64::NAN;
    const NEG_NAN: Self = -f64::NAN;
}

/// A stack buffer for the canonical text handed to the standard float parser, spilling to
/// the heap for unusually long inputs.
struct FloatText {
    stack: [u8; 64],
    len: usize,
    spill: Vec<u8>,
}

impl FloatText {
    fn new() -> Self {
        Self {
            stack: [0; 64],
            len: 0,
            spill: Vec::new(),
        }
    }

    fn push(&mut self, byte: u8) {
        if self.len < self.stack.len() {
            self.stack[self.len] = byte;
        } else {
            if self.spill.is_empty() {
                self.spill.extend_from_slice(&self.stack);
            }
            self.spill.push(byte);
        }
        self.len += 1;
    }

    fn as_str(&self) -> Option<&str> {
        let bytes = if self.spill.is_empty() {
            &self.stack[..self.len]
        } else {
            &self.spill
        };
        std::str::from_utf8(bytes).ok()
    }
}

/// Parses a floating point number like DuckDB's `TryDoubleCast` (`fast_float` with DuckDB's
/// underscore and strict-mode extensions). Values are correctly rounded.
pub(super) fn parse_float<T: ParseFloat>(input: &[u8], strict: bool) -> Option<T> {
    let start = input.iter().position(|&b| !is_space(b))?;
    let mut buf = &input[start..];
    if buf[0] == b'+' {
        if strict {
            return None;
        }
        buf = &buf[1..];
    }
    if strict && buf.len() >= 2 && buf[0] == b'0' && buf[1].is_ascii_digit() {
        return None;
    }
    let (value, consumed) = match scan_float_number(buf, strict) {
        Some(number) => number,
        None => parse_inf_nan::<T>(buf)?,
    };
    let rest = &buf[consumed..];
    let trailing_ok = if strict {
        rest.is_empty()
    } else {
        rest.iter().all(|&b| is_space(b))
    };
    trailing_ok.then_some(value)
}

/// Consumes digits starting at `pos`, allowing single underscores between digits unless
/// `strict`; returns the number of digits, or `None` for a misplaced underscore.
fn float_digits(buf: &[u8], pos: &mut usize, strict: bool, mut sink: impl FnMut(u8)) -> Option<usize> {
    let mut count = 0;
    while *pos < buf.len() && buf[*pos].is_ascii_digit() {
        sink(buf[*pos]);
        count += 1;
        *pos += 1;
        if *pos < buf.len() && buf[*pos] == b'_' {
            if strict {
                return None;
            }
            *pos += 1;
            if *pos == buf.len() || !buf[*pos].is_ascii_digit() {
                return None;
            }
        }
    }
    Some(count)
}

/// `fast_float`'s `parse_number_string`: the value and the number of bytes consumed.
fn scan_float_number<T: ParseFloat>(buf: &[u8], strict: bool) -> Option<(T, usize)> {
    let mut text = FloatText::new();
    let mut pos = 0;
    if buf.first() == Some(&b'-') {
        text.push(b'-');
        pos += 1;
        if pos == buf.len() || !(buf[pos].is_ascii_digit() || buf[pos] == b'.') {
            return None;
        }
    }
    let mut digit_count = float_digits(buf, &mut pos, strict, |d| text.push(d))?;
    if buf.get(pos) == Some(&b'.') {
        text.push(b'.');
        pos += 1;
        digit_count += float_digits(buf, &mut pos, strict, |d| text.push(d))?;
    }
    if digit_count == 0 {
        return None;
    }
    if matches!(buf.get(pos), Some(b'e' | b'E')) {
        let mut exp_pos = pos + 1;
        let negative = buf.get(exp_pos) == Some(&b'-');
        if negative || buf.get(exp_pos) == Some(&b'+') {
            exp_pos += 1;
        }
        // Without digits the `e` is not part of the number.
        if buf.get(exp_pos).is_some_and(u8::is_ascii_digit) {
            pos = exp_pos;
            // fast_float stops accumulating once the exponent exceeds 0x10000.
            let mut exponent: u32 = 0;
            float_digits(buf, &mut pos, strict, |d| {
                if exponent < 0x10000 {
                    exponent = exponent * 10 + u32::from(d - b'0');
                }
            })?;
            text.push(b'e');
            if negative {
                text.push(b'-');
            }
            // At most six digits (the accumulation stops past 0x10000).
            let mut divisor = 100_000;
            let mut started = false;
            while divisor > 0 {
                let digit = (exponent / divisor % 10) as u8;
                if digit != 0 || started || divisor == 1 {
                    text.push(b'0' + digit);
                    started = true;
                }
                divisor /= 10;
            }
        }
    }
    let value = text.as_str()?.parse::<T>().ok()?;
    Some((value, pos))
}

/// `fast_float`'s `parse_infnan`: `nan`, `nan(...)`, `inf` and `infinity`, case-insensitive,
/// with an optional `-`.
fn parse_inf_nan<T: ParseFloat>(buf: &[u8]) -> Option<(T, usize)> {
    let (negative, rest, offset) = match buf.first() {
        Some(b'-') => (true, &buf[1..], 1),
        _ => (false, buf, 0),
    };
    let starts_with =
        |prefix: &[u8]| rest.len() >= prefix.len() && rest[..prefix.len()].eq_ignore_ascii_case(prefix);
    if starts_with(b"nan") {
        let mut consumed = 3;
        if rest.get(3) == Some(&b'(') {
            for (i, &byte) in rest.iter().enumerate().skip(4) {
                if byte == b')' {
                    consumed = i + 1;
                    break;
                }
                if !(byte.is_ascii_alphanumeric() || byte == b'_') {
                    break;
                }
            }
        }
        let value = if negative { T::NEG_NAN } else { T::NAN };
        return Some((value, offset + consumed));
    }
    if starts_with(b"inf") {
        let consumed = if starts_with(b"infinity") { 8 } else { 3 };
        let value = if negative { T::NEG_INFINITY } else { T::INFINITY };
        return Some((value, offset + consumed));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integers_parse_like_duckdb() {
        let int = |s: &str| i32::parse_text(s.as_bytes(), false);
        assert_eq!(int("42"), Some(42));
        assert_eq!(int("  -42  "), Some(-42));
        assert_eq!(int("+7"), Some(7));
        assert_eq!(int("1_000"), Some(1000));
        assert_eq!(int("1__000"), None);
        assert_eq!(int("1_"), None);
        assert_eq!(int("0x1F"), Some(31));
        assert_eq!(int("0b101"), Some(5));
        assert_eq!(int("0x"), None);
        assert_eq!(int("1.5"), Some(2));
        assert_eq!(int("2.5"), Some(3));
        assert_eq!(int("-2.5"), Some(-3));
        assert_eq!(int("1.49"), Some(1));
        assert_eq!(int("1e2"), Some(100));
        assert_eq!(int("1.5e1"), Some(15));
        assert_eq!(int("15e-1"), Some(2));
        assert_eq!(int("."), None);
        assert_eq!(int("1."), Some(1));
        assert_eq!(int(".5"), Some(1));
        assert_eq!(int("2147483647"), Some(i32::MAX));
        assert_eq!(int("-2147483648"), Some(i32::MIN));
        assert_eq!(int("2147483648"), None);
        assert_eq!(int(""), None);
        assert_eq!(int("1 2"), None);
        assert_eq!(u8::parse_text(b"-0", false), Some(0));
        assert_eq!(u8::parse_text(b"-1", false), None);
        assert_eq!(i8::parse_text(b"0x7F", false), Some(127));
        assert_eq!(i8::parse_text(b"0x80", false), None);
        assert_eq!(i32::parse_text(b"007", true), None);
        assert_eq!(i32::parse_text(b"+7", true), None);
    }

    #[test]
    fn wide_integers_round_decimals() {
        let huge = |s: &str| i128::parse_text(s.as_bytes(), false);
        assert_eq!(huge("170141183460469231731687303715884105727"), Some(i128::MAX));
        assert_eq!(huge("-170141183460469231731687303715884105728"), Some(i128::MIN));
        assert_eq!(huge("170141183460469231731687303715884105728"), None);
        assert_eq!(huge("1.5"), Some(2));
        assert_eq!(huge("-1.5"), Some(-2));
        assert_eq!(huge("1e30"), Some(10i128.pow(30)));
        assert_eq!(huge("12345e-2"), Some(123));
        assert_eq!(
            u128::parse_text(b"340282366920938463463374607431768211455", false),
            Some(u128::MAX)
        );
    }

    #[test]
    fn decimals_round_half_away() {
        let dec = |s: &str, w, sc| parse_decimal(s.as_bytes(), w, sc);
        assert_eq!(dec("1.25", 4, 1), Some(13));
        assert_eq!(dec("-1.25", 4, 1), Some(-13));
        assert_eq!(dec("1.2", 4, 3), Some(1200));
        assert_eq!(dec("99.99", 4, 2), Some(9999));
        assert_eq!(dec("100", 4, 2), None);
        assert_eq!(dec("1e2", 5, 2), Some(10000));
        assert_eq!(dec("0001.5", 2, 1), Some(15));
    }

    #[test]
    fn floats_parse_like_fast_float() {
        let double = |s: &str| parse_float::<f64>(s.as_bytes(), false);
        assert_eq!(double("1.5"), Some(1.5));
        assert_eq!(double(" -1e3 "), Some(-1000.0));
        assert_eq!(double("1_000.5"), Some(1000.5));
        assert_eq!(double(".5"), Some(0.5));
        assert_eq!(double("5."), Some(5.0));
        assert_eq!(double("."), None);
        assert_eq!(double("1e"), None);
        assert_eq!(double("inf"), Some(f64::INFINITY));
        assert_eq!(double("-Infinity"), Some(f64::NEG_INFINITY));
        assert!(double("NaN").is_some_and(f64::is_nan));
        assert_eq!(double("1e400"), Some(f64::INFINITY));
        assert_eq!(double("0.1"), Some(0.1));
        assert_eq!(double("abc"), None);
        assert_eq!(parse_float::<f32>(b"3.4028236e38", false), Some(f32::INFINITY));
        assert_eq!(parse_bool(b"YES", false), Some(true));
        assert_eq!(parse_bool(b"1", true), None);
    }
}
