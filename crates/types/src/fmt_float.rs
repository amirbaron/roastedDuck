//! Floating point digit generation of fmtlib 6.1.2 (`duckdb_fmt`), which DuckDB uses for
//! every float/double to text conversion.
//!
//! DuckDB's output depends on fmt's exact algorithms rather than on the mathematically
//! shortest or exact digits: ties in the shortest representation follow Grisu's
//! `round_weed` or the `(FPP)^2` fallback (round half to even), FLOAT values fall back to a
//! digit generator that uses DOUBLE boundaries, and fixed-precision output (`%f`) of very
//! large values produces Grisu's 64-bit approximation digits. This module ports those
//! algorithms one to one so that the text is byte-identical.

/// Normalized 64-bit significands of `10^k` for `k = -348, -340, ..., 340`, verbatim from
/// fmt (`support/compute-powers.py`).
#[rustfmt::skip]
#[allow(clippy::unreadable_literal)]
const POW10_SIGNIFICANDS: [u64; 87] = [
    0xfa8fd5a0081c0288, 0xbaaee17fa23ebf76, 0x8b16fb203055ac76, 0xcf42894a5dce35ea, 0x9a6bb0aa55653b2d,
    0xe61acf033d1a45df, 0xab70fe17c79ac6ca, 0xff77b1fcbebcdc4f, 0xbe5691ef416bd60c, 0x8dd01fad907ffc3c,
    0xd3515c2831559a83, 0x9d71ac8fada6c9b5, 0xea9c227723ee8bcb, 0xaecc49914078536d, 0x823c12795db6ce57,
    0xc21094364dfb5637, 0x9096ea6f3848984f, 0xd77485cb25823ac7, 0xa086cfcd97bf97f4, 0xef340a98172aace5,
    0xb23867fb2a35b28e, 0x84c8d4dfd2c63f3b, 0xc5dd44271ad3cdba, 0x936b9fcebb25c996, 0xdbac6c247d62a584,
    0xa3ab66580d5fdaf6, 0xf3e2f893dec3f126, 0xb5b5ada8aaff80b8, 0x87625f056c7c4a8b, 0xc9bcff6034c13053,
    0x964e858c91ba2655, 0xdff9772470297ebd, 0xa6dfbd9fb8e5b88f, 0xf8a95fcf88747d94, 0xb94470938fa89bcf,
    0x8a08f0f8bf0f156b, 0xcdb02555653131b6, 0x993fe2c6d07b7fac, 0xe45c10c42a2b3b06, 0xaa242499697392d3,
    0xfd87b5f28300ca0e, 0xbce5086492111aeb, 0x8cbccc096f5088cc, 0xd1b71758e219652c, 0x9c40000000000000,
    0xe8d4a51000000000, 0xad78ebc5ac620000, 0x813f3978f8940984, 0xc097ce7bc90715b3, 0x8f7e32ce7bea5c70,
    0xd5d238a4abe98068, 0x9f4f2726179a2245, 0xed63a231d4c4fb27, 0xb0de65388cc8ada8, 0x83c7088e1aab65db,
    0xc45d1df942711d9a, 0x924d692ca61be758, 0xda01ee641a708dea, 0xa26da3999aef774a, 0xf209787bb47d6b85,
    0xb454e4a179dd1877, 0x865b86925b9bc5c2, 0xc83553c5c8965d3d, 0x952ab45cfa97a0b3, 0xde469fbd99a05fe3,
    0xa59bc234db398c25, 0xf6c69a72a3989f5c, 0xb7dcbf5354e9bece, 0x88fcf317f22241e2, 0xcc20ce9bd35c78a5,
    0x98165af37b2153df, 0xe2a0b5dc971f303a, 0xa8d9d1535ce3b396, 0xfb9b7cd9a4a7443c, 0xbb764c4ca7a44410,
    0x8bab8eefb6409c1a, 0xd01fef10a657842c, 0x9b10a4e5e9913129, 0xe7109bfba19c0c9d, 0xac2820d9623bf429,
    0x80444b5e7aa7cf85, 0xbf21e44003acdd2d, 0x8e679c2f5e44ff8f, 0xd433179d9c8cb841, 0x9e19db92b4e31ba9,
    0xeb96bf6ebadf77d9, 0xaf87023b9bf0ee6b,
];

/// Binary exponents of the significands above.
#[rustfmt::skip]
const POW10_EXPONENTS: [i16; 87] = [
    -1220, -1193, -1166, -1140, -1113, -1087, -1060, -1034, -1007, -980, -954, -927, -901, -874, -847,
    -821, -794, -768, -741, -715, -688, -661, -635, -608, -582, -555, -529, -502, -475, -449, -422,
    -396, -369, -343, -316, -289, -263, -236, -210, -183, -157, -130, -103, -77, -50, -24, 3, 30, 56,
    83, 109, 136, 162, 189, 216, 242, 269, 295, 322, 348, 375, 402, 428, 455, 481, 508, 534, 561,
    588, 614, 641, 667, 694, 720, 747, 774, 800, 827, 853, 880, 907, 933, 960, 986, 1013, 1039, 1066,
];

/// `10^0 .. 10^19`.
const POWERS_OF_10_64: [u64; 20] = {
    let mut table = [1u64; 20];
    let mut i = 1;
    while i < 20 {
        table[i] = table[i - 1] * 10;
        i += 1;
    }
    table
};

const DOUBLE_SIGNIFICAND_SIZE: i32 = 52;
const IMPLICIT_BIT: u64 = 1 << DOUBLE_SIGNIFICAND_SIZE;
const SIGNIFICAND_SIZE: i32 = 64;

/// `f * 2^e` with a 64-bit significand (fmt's `fp`).
#[derive(Clone, Copy, Debug)]
struct Fp {
    f: u64,
    e: i32,
}

impl Fp {
    /// The (unnormalized) significand and exponent of `d`, and whether its predecessor is
    /// closer than its successor (a power of two other than the smallest normal).
    fn assign(d: f64) -> (Self, bool) {
        const EXPONENT_BIAS: i32 = 1023;
        let bits = d.to_bits();
        let mut f = bits & (IMPLICIT_BIT - 1);
        let mut biased_e = ((bits >> DOUBLE_SIGNIFICAND_SIZE) & 0x7ff) as i32;
        let predecessor_closer = f == 0 && biased_e > 1;
        if biased_e == 0 {
            // Subnormals use the minimum exponent.
            biased_e = 1;
        } else {
            f += IMPLICIT_BIT;
        }
        (
            Self {
                f,
                e: biased_e - EXPONENT_BIAS - DOUBLE_SIGNIFICAND_SIZE,
            },
            predecessor_closer,
        )
    }

    /// Shifts the significand so that its top bit is set, for a value multiplied by
    /// `2^shift`.
    fn normalize(self, shift: i32) -> Self {
        let mut value = self;
        let shifted_implicit_bit = IMPLICIT_BIT << shift;
        while value.f & shifted_implicit_bit == 0 {
            value.f <<= 1;
            value.e -= 1;
        }
        let offset = SIGNIFICAND_SIZE - DOUBLE_SIGNIFICAND_SIZE - shift - 1;
        value.f <<= offset;
        value.e -= offset;
        value
    }

    /// `self * other` rounded to 64 bits.
    fn mul(self, other: Self) -> Self {
        Self {
            f: multiply(self.f, other.f),
            e: self.e + other.e + 64,
        }
    }
}

/// `lhs * rhs / 2^64`, rounded half up.
fn multiply(lhs: u64, rhs: u64) -> u64 {
    let product = u128::from(lhs) * u128::from(rhs);
    let high = (product >> 64) as u64;
    if product as u64 & (1 << 63) != 0 {
        high + 1
    } else {
        high
    }
}

/// The value with its rounding boundaries aligned to the normalized upper boundary.
fn boundaries(d: f64) -> (Fp, u64, u64) {
    let (value, lower_closer) = Fp::assign(d);
    let lower = if lower_closer {
        Fp {
            f: (value.f << 2) - 1,
            e: value.e - 2,
        }
    } else {
        Fp {
            f: (value.f << 1) - 1,
            e: value.e - 1,
        }
    };
    let upper = Fp {
        f: (value.f << 1) + 1,
        e: value.e - 1,
    }
    .normalize(1);
    (value, lower.f << (lower.e - upper.e), upper.f)
}

/// Like [`boundaries`] for a FLOAT value widened to DOUBLE.
fn float_boundaries(d: f64) -> (Fp, u64, u64) {
    const MIN_NORMAL_E: i32 = -125 - 53;
    let (value, _) = Fp::assign(d);
    let mut half_ulp: u64 = 1 << (53 - 24 - 1);
    if MIN_NORMAL_E > value.e {
        half_ulp <<= MIN_NORMAL_E - value.e;
    }
    let upper = Fp {
        f: value.f + half_ulp,
        e: value.e,
    }
    .normalize(0);
    let lower_shift = u32::from(value.f == IMPLICIT_BIT && value.e > MIN_NORMAL_E);
    let lower = Fp {
        f: value.f - (half_ulp >> lower_shift),
        e: value.e,
    };
    (value, lower.f << (lower.e - upper.e), upper.f)
}

/// A cached power of ten whose binary exponent lies in `[min_exponent, min_exponent + 28]`,
/// and its decimal exponent.
fn cached_power(min_exponent: i32) -> (Fp, i32) {
    const ONE_OVER_LOG2_10: i64 = 0x4d10_4d42;
    const FIRST_DEC_EXP: i32 = -348;
    const DEC_EXP_STEP: i32 = 8;
    let index =
        ((i64::from(min_exponent + SIGNIFICAND_SIZE - 1) * ONE_OVER_LOG2_10 + ((1 << 32) - 1)) >> 32) as i32;
    let index = (index - FIRST_DEC_EXP - 1) / DEC_EXP_STEP + 1;
    let index = index.clamp(0, POW10_SIGNIFICANDS.len() as i32 - 1) as usize;
    let pow10_exponent = FIRST_DEC_EXP + index as i32 * DEC_EXP_STEP;
    (
        Fp {
            f: POW10_SIGNIFICANDS[index],
            e: i32::from(POW10_EXPONENTS[index]),
        },
        pow10_exponent,
    )
}

fn count_digits(n: u32) -> i32 {
    n.checked_ilog10().map_or(1, |log| log as i32 + 1)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Digits {
    More,
    Done,
    Error,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RoundDirection {
    Unknown,
    Up,
    Down,
}

/// Whether a number with `remainder` below the last digit should round up or down,
/// given the uncertainty `error`.
fn round_direction(divisor: u64, remainder: u64, error: u64) -> RoundDirection {
    if remainder <= divisor.wrapping_sub(remainder)
        && error.wrapping_mul(2) <= divisor.wrapping_sub(remainder.wrapping_mul(2))
    {
        return RoundDirection::Down;
    }
    if remainder >= error && remainder - error >= divisor.wrapping_sub(remainder - error) {
        return RoundDirection::Up;
    }
    RoundDirection::Unknown
}

/// Where generated digits go.
pub(crate) trait DigitBuffer {
    fn push(&mut self, digit: u8);
    fn digits(&self) -> &[u8];
    fn digits_mut(&mut self) -> &mut [u8];
    fn clear(&mut self);
}

impl DigitBuffer for Vec<u8> {
    fn push(&mut self, digit: u8) {
        Vec::push(self, digit);
    }
    fn digits(&self) -> &[u8] {
        self
    }
    fn digits_mut(&mut self) -> &mut [u8] {
        self
    }
    fn clear(&mut self) {
        Vec::clear(self);
    }
}

/// A stack buffer for shortest representations (at most 17 significant digits plus the
/// rounding carry; extra digits are dropped, which cannot happen for finite values).
#[derive(Debug)]
pub(crate) struct ShortDigits {
    buf: [u8; 32],
    len: usize,
}

impl ShortDigits {
    pub(crate) const fn new() -> Self {
        Self { buf: [0; 32], len: 0 }
    }
}

impl DigitBuffer for ShortDigits {
    fn push(&mut self, digit: u8) {
        if let Some(slot) = self.buf.get_mut(self.len) {
            *slot = digit;
            self.len += 1;
        }
    }
    fn digits(&self) -> &[u8] {
        &self.buf[..self.len]
    }
    fn digits_mut(&mut self) -> &mut [u8] {
        &mut self.buf[..self.len]
    }
    fn clear(&mut self) {
        self.len = 0;
    }
}

/// The callbacks of [`gen_digits`].
trait DigitHandler {
    fn on_start(&mut self, divisor: u64, remainder: u64, error: u64, exp: i32) -> Digits;
    fn on_digit(
        &mut self,
        digit: u8,
        divisor: u64,
        remainder: u64,
        error: u64,
        exp: i32,
        integral: bool,
    ) -> Digits;
}

/// Grisu's digit generation: emits the digits of `value` (scaled by a cached power of
/// ten) to `handler`; `exp` ends as the decimal exponent of the next digit.
fn gen_digits(value: Fp, mut error: u64, exp: &mut i32, handler: &mut impl DigitHandler) -> Digits {
    let shift = (-value.e) as u32;
    let one = 1u64 << shift;
    let mut integral = (value.f >> shift) as u32;
    let mut fractional = value.f & (one - 1);
    *exp = count_digits(integral);
    let result = handler.on_start(
        POWERS_OF_10_64[(*exp - 1) as usize] << shift,
        value.f / 10,
        error.wrapping_mul(10),
        *exp,
    );
    if result != Digits::More {
        return result;
    }
    loop {
        let divisor = POWERS_OF_10_64[(*exp - 1) as usize] as u32;
        let digit = integral / divisor;
        integral %= divisor;
        *exp -= 1;
        let remainder = (u64::from(integral) << shift) + fractional;
        let result = handler.on_digit(
            b'0' + digit as u8,
            POWERS_OF_10_64[*exp as usize] << shift,
            remainder,
            error,
            *exp,
            true,
        );
        if result != Digits::More {
            return result;
        }
        if *exp <= 0 {
            break;
        }
    }
    loop {
        fractional = fractional.wrapping_mul(10);
        error = error.wrapping_mul(10);
        let digit = b'0' + (fractional >> shift) as u8;
        fractional &= one - 1;
        *exp -= 1;
        let result = handler.on_digit(digit, one, fractional, error, *exp, false);
        if result != Digits::More {
            return result;
        }
    }
}

/// Digits for a fixed number of decimals (fmt's `fixed_handler`).
struct FixedHandler<'a> {
    buf: &'a mut Vec<u8>,
    precision: i32,
    exp10: i32,
}

impl DigitHandler for FixedHandler<'_> {
    fn on_start(&mut self, divisor: u64, remainder: u64, error: u64, exp: i32) -> Digits {
        // The precision is relative to the decimal point.
        self.precision += exp + self.exp10;
        if self.precision > 0 {
            return Digits::More;
        }
        if self.precision < 0 {
            return Digits::Done;
        }
        match round_direction(divisor, remainder, error) {
            RoundDirection::Unknown => Digits::Error,
            direction => {
                self.buf.push(if direction == RoundDirection::Up {
                    b'1'
                } else {
                    b'0'
                });
                Digits::Done
            }
        }
    }

    fn on_digit(
        &mut self,
        digit: u8,
        divisor: u64,
        remainder: u64,
        error: u64,
        _: i32,
        integral: bool,
    ) -> Digits {
        self.buf.push(digit);
        if (self.buf.len() as i64) < i64::from(self.precision) {
            return Digits::More;
        }
        if !integral && (error >= divisor || error >= divisor.wrapping_sub(error)) {
            return Digits::Error;
        }
        match round_direction(divisor, remainder, error) {
            RoundDirection::Down => return Digits::Done,
            RoundDirection::Unknown => return Digits::Error,
            RoundDirection::Up => {}
        }
        // Round the generated digits up.
        let mut i = self.buf.len() - 1;
        self.buf[i] += 1;
        while i > 0 && self.buf[i] > b'9' {
            self.buf[i] = b'0';
            self.buf[i - 1] += 1;
            i -= 1;
        }
        if self.buf[0] > b'9' {
            self.buf[0] = b'1';
            self.buf.push(b'0');
        }
        Digits::Done
    }
}

/// Digits of the shortest representation (fmt's `grisu_shortest_handler`).
struct ShortestHandler<'a, B: DigitBuffer> {
    buf: &'a mut B,
    /// Distance between the scaled value and the upper bound.
    diff: u64,
}

impl<B: DigitBuffer> ShortestHandler<'_, B> {
    /// Decrements the generated number approaching the value from above.
    fn round(&mut self, d: u64, divisor: u64, remainder: &mut u64, error: u64) {
        while *remainder < d
            && error.wrapping_sub(*remainder) >= divisor
            && (remainder.wrapping_add(divisor) < d
                || d.wrapping_sub(*remainder) >= remainder.wrapping_add(divisor).wrapping_sub(d))
        {
            if let Some(last) = self.buf.digits_mut().last_mut() {
                *last = last.wrapping_sub(1);
            }
            *remainder = remainder.wrapping_add(divisor);
        }
    }
}

impl<B: DigitBuffer> DigitHandler for ShortestHandler<'_, B> {
    fn on_start(&mut self, _: u64, _: u64, _: u64, _: i32) -> Digits {
        Digits::More
    }

    /// Grisu's `round_weed`.
    fn on_digit(
        &mut self,
        digit: u8,
        divisor: u64,
        mut remainder: u64,
        error: u64,
        exp: i32,
        integral: bool,
    ) -> Digits {
        self.buf.push(digit);
        if remainder >= error {
            return Digits::More;
        }
        let unit = if integral {
            1
        } else {
            match POWERS_OF_10_64.get(exp.unsigned_abs() as usize) {
                Some(&unit) => unit,
                None => return Digits::Error,
            }
        };
        let up = self.diff.wrapping_sub(1).wrapping_mul(unit);
        self.round(up, divisor, &mut remainder, error);
        let down = self.diff.wrapping_add(1).wrapping_mul(unit);
        if remainder < down
            && error.wrapping_sub(remainder) >= divisor
            && (remainder.wrapping_add(divisor) < down
                || down.wrapping_sub(remainder) > remainder.wrapping_add(divisor).wrapping_sub(down))
        {
            return Digits::Error;
        }
        if unit.wrapping_mul(2) <= remainder && remainder <= error.wrapping_sub(unit.wrapping_mul(4)) {
            Digits::Done
        } else {
            Digits::Error
        }
    }
}

/// An arbitrary precision unsigned integer `bigits * 2^(32 * exp)` (fmt's `bigint`).
#[derive(Clone, Debug, Default)]
struct BigInt {
    /// Little-endian 32-bit limbs.
    bigits: Vec<u32>,
    exp: i32,
}

impl BigInt {
    fn from_u64(mut n: u64) -> Self {
        let mut bigits = Vec::with_capacity(4);
        loop {
            bigits.push(n as u32);
            n >>= 32;
            if n == 0 {
                break;
            }
        }
        Self { bigits, exp: 0 }
    }

    fn num_bigits(&self) -> i32 {
        self.bigits.len() as i32 + self.exp
    }

    fn remove_leading_zeros(&mut self) {
        while self.bigits.len() > 1 && self.bigits.last() == Some(&0) {
            self.bigits.pop();
        }
    }

    fn shl(&mut self, shift: i32) {
        self.exp += shift / 32;
        let shift = shift % 32;
        if shift == 0 {
            return;
        }
        let mut carry = 0;
        for bigit in &mut self.bigits {
            let next = *bigit >> (32 - shift);
            *bigit = (*bigit << shift).wrapping_add(carry);
            carry = next;
        }
        if carry != 0 {
            self.bigits.push(carry);
        }
    }

    fn mul_u32(&mut self, value: u32) {
        let mut carry = 0u64;
        for bigit in &mut self.bigits {
            let result = u64::from(*bigit) * u64::from(value) + carry;
            *bigit = result as u32;
            carry = result >> 32;
        }
        if carry != 0 {
            self.bigits.push(carry as u32);
        }
    }

    fn mul_u64(&mut self, value: u64) {
        let lower = value & 0xffff_ffff;
        let upper = value >> 32;
        let mut carry = 0u64;
        for bigit in &mut self.bigits {
            let b = u64::from(*bigit);
            let result = b * lower + (carry & 0xffff_ffff);
            carry = b * upper + (result >> 32) + (carry >> 32);
            *bigit = result as u32;
        }
        while carry != 0 {
            self.bigits.push(carry as u32);
            carry >>= 32;
        }
    }

    fn square(&mut self) {
        let n = std::mem::take(&mut self.bigits);
        let num_bigits = n.len();
        let mut result = vec![0u32; 2 * num_bigits];
        let mut sum: u128 = 0;
        for (index, slot) in result.iter_mut().enumerate().take(num_bigits) {
            for i in 0..=index {
                sum += u128::from(n[i]) * u128::from(n[index - i]);
            }
            *slot = sum as u32;
            sum >>= 32;
        }
        for (index, slot) in result.iter_mut().enumerate().skip(num_bigits) {
            for j in (index + 1 - num_bigits..num_bigits).rev() {
                let i = index - j;
                sum += u128::from(n[i]) * u128::from(n[j]);
            }
            *slot = sum as u32;
            sum >>= 32;
        }
        self.bigits = result;
        self.remove_leading_zeros();
        self.exp *= 2;
    }

    fn pow10(exp: i32) -> Self {
        if exp == 0 {
            return Self::from_u64(1);
        }
        let mut bitmask = 1;
        while exp >= bitmask {
            bitmask <<= 1;
        }
        bitmask >>= 1;
        // 10^exp = 5^exp * 2^exp: square-and-multiply for 5^exp, then shift.
        let mut result = Self::from_u64(5);
        bitmask >>= 1;
        while bitmask != 0 {
            result.square();
            if exp & bitmask != 0 {
                result.mul_u32(5);
            }
            bitmask >>= 1;
        }
        result.shl(exp);
        result
    }

    fn bigit_at(&self, index: i32) -> u32 {
        if index >= self.exp && index < self.num_bigits() {
            self.bigits[(index - self.exp) as usize]
        } else {
            0
        }
    }

    fn compare(lhs: &Self, rhs: &Self) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        let (lhs_bigits, rhs_bigits) = (lhs.num_bigits(), rhs.num_bigits());
        if lhs_bigits != rhs_bigits {
            return lhs_bigits.cmp(&rhs_bigits);
        }
        let mut i = lhs.bigits.len() as i32 - 1;
        let mut j = rhs.bigits.len() as i32 - 1;
        let end = (i - j).max(0);
        while i >= end {
            let (l, r) = (lhs.bigits[i as usize], rhs.bigits[j as usize]);
            if l != r {
                return l.cmp(&r);
            }
            i -= 1;
            j -= 1;
        }
        if i != j {
            return if i > j { Ordering::Greater } else { Ordering::Less };
        }
        Ordering::Equal
    }

    /// `compare(lhs1 + lhs2, rhs)` as -1, 0 or 1.
    fn add_compare(lhs1: &Self, lhs2: &Self, rhs: &Self) -> i32 {
        let max_lhs_bigits = lhs1.num_bigits().max(lhs2.num_bigits());
        let num_rhs_bigits = rhs.num_bigits();
        if max_lhs_bigits + 1 < num_rhs_bigits {
            return -1;
        }
        if max_lhs_bigits > num_rhs_bigits {
            return 1;
        }
        let mut borrow: u64 = 0;
        let min_exp = lhs1.exp.min(lhs2.exp).min(rhs.exp);
        let mut i = num_rhs_bigits - 1;
        while i >= min_exp {
            let sum = u64::from(lhs1.bigit_at(i)) + u64::from(lhs2.bigit_at(i));
            let rhs_bigit = u64::from(rhs.bigit_at(i));
            if sum > rhs_bigit + borrow {
                return 1;
            }
            borrow = rhs_bigit + borrow - sum;
            if borrow > 1 {
                return -1;
            }
            borrow <<= 32;
            i -= 1;
        }
        if borrow != 0 { -1 } else { 0 }
    }

    /// `self -= other` for aligned values with `self >= other`.
    fn subtract_aligned(&mut self, other: &Self) {
        let mut borrow = 0u32;
        let mut i = (other.exp - self.exp) as usize;
        let subtract = |bigits: &mut Vec<u32>, index: usize, value: u32, borrow: &mut u32| {
            let result = u64::from(bigits[index])
                .wrapping_sub(u64::from(value))
                .wrapping_sub(u64::from(*borrow));
            bigits[index] = result as u32;
            *borrow = (result >> 63) as u32;
        };
        for &value in &other.bigits {
            subtract(&mut self.bigits, i, value, &mut borrow);
            i += 1;
        }
        // Like fmt, the borrow is applied to the same limb until it is absorbed.
        while borrow > 0 && i < self.bigits.len() {
            subtract(&mut self.bigits, i, 0, &mut borrow);
        }
        self.remove_leading_zeros();
    }

    /// Divides by `divisor`, keeping the remainder and returning the (small) quotient.
    fn divmod_assign(&mut self, divisor: &Self) -> i32 {
        if Self::compare(self, divisor) == std::cmp::Ordering::Less {
            return 0;
        }
        let exp_difference = self.exp - divisor.exp;
        if exp_difference > 0 {
            // Align by adding trailing zero limbs.
            let mut aligned = vec![0u32; exp_difference as usize];
            aligned.extend_from_slice(&self.bigits);
            self.bigits = aligned;
            self.exp -= exp_difference;
        }
        let mut quotient = 0;
        loop {
            self.subtract_aligned(divisor);
            quotient += 1;
            if Self::compare(self, divisor) == std::cmp::Ordering::Less {
                return quotient;
            }
        }
    }
}

/// fmt's `fallback_format`: the shortest digits by the `(FPP)^2` algorithm (with DOUBLE
/// boundaries, even for FLOAT values), rounding half to even. `exp10` is an estimate of the
/// decimal exponent of the first digit on input and the exponent of the last digit on
/// output.
fn fallback_format(d: f64, buf: &mut impl DigitBuffer, exp10: &mut i32) {
    let (value, lower_closer) = Fp::assign(d);
    let shift = if lower_closer { 2 } else { 1 };
    let significand = value.f << shift;
    let mut numerator;
    let mut denominator;
    let mut lower;
    let mut upper: Option<BigInt> = None;
    if value.e >= 0 {
        numerator = BigInt::from_u64(significand);
        numerator.shl(value.e);
        lower = BigInt::from_u64(1);
        lower.shl(value.e);
        if shift != 1 {
            let mut upper_value = BigInt::from_u64(1);
            upper_value.shl(value.e + 1);
            upper = Some(upper_value);
        }
        denominator = BigInt::pow10(*exp10);
        denominator.shl(1);
    } else if *exp10 < 0 {
        numerator = BigInt::pow10(-*exp10);
        lower = numerator.clone();
        if shift != 1 {
            let mut upper_value = numerator.clone();
            upper_value.shl(1);
            upper = Some(upper_value);
        }
        numerator.mul_u64(significand);
        denominator = BigInt::from_u64(1);
        denominator.shl(shift - value.e);
    } else {
        numerator = BigInt::from_u64(significand);
        denominator = BigInt::pow10(*exp10);
        denominator.shl(shift - value.e);
        lower = BigInt::from_u64(1);
        if shift != 1 {
            upper = Some(BigInt::from_u64(2));
        }
    }
    let even = value.f & 1 == 0;
    buf.clear();
    loop {
        let digit = numerator.divmod_assign(&denominator);
        let upper_ref = upper.as_ref().unwrap_or(&lower);
        let low = (BigInt::compare(&numerator, &lower) as i32) - i32::from(even) < 0;
        let high = BigInt::add_compare(&numerator, upper_ref, &denominator) + i32::from(even) > 0;
        buf.push(b'0' + digit as u8);
        if low || high {
            if let Some(last) = buf.digits_mut().last_mut() {
                if !low {
                    *last += 1;
                } else if high {
                    // Round half to even.
                    let result = BigInt::add_compare(&numerator, &numerator, &denominator);
                    if result > 0 || (result == 0 && digit % 2 != 0) {
                        *last += 1;
                    }
                }
            }
            *exp10 -= buf.digits().len() as i32 - 1;
            return;
        }
        numerator.mul_u32(10);
        lower.mul_u32(10);
        if let Some(upper) = &mut upper {
            upper.mul_u32(10);
        }
    }
}

/// Exact fixed formatting, used where fmt falls back to `snprintf("%.*f")`.
fn exact_fixed(value: f64, precision: i32, buf: &mut Vec<u8>) -> i32 {
    use std::io::Write as _;
    buf.clear();
    let _ = write!(buf, "{value:.prec$}", prec = precision.max(0) as usize);
    if precision <= 0 {
        return 0;
    }
    if let Some(point) = buf.iter().position(|&b| b == b'.') {
        buf.remove(point);
    }
    -precision
}

/// fmt's `format_float` with a fixed number of decimals (`%f`) for a positive finite
/// value: fills `buf` with digits `d` and returns `exp` such that the value is
/// `d * 10^exp`.
pub(crate) fn format_fixed(value: f64, precision: i32, buf: &mut Vec<u8>) -> i32 {
    const MIN_EXP: i32 = -60;
    buf.clear();
    if value <= 0.0 {
        if precision <= 0 {
            buf.push(b'0');
            return 0;
        }
        buf.resize(precision as usize, b'0');
        return -precision;
    }
    if precision > 17 {
        return exact_fixed(value, precision, buf);
    }
    let normalized = Fp::assign(value).0.normalize(0);
    let (cached, cached_exp10) = cached_power(MIN_EXP - (normalized.e + SIGNIFICAND_SIZE));
    let scaled = normalized.mul(cached);
    let mut handler = FixedHandler {
        buf,
        precision,
        exp10: -cached_exp10,
    };
    let mut exp = 0;
    if gen_digits(scaled, 1, &mut exp, &mut handler) == Digits::Error {
        return exact_fixed(value, precision, handler.buf);
    }
    exp - cached_exp10
}

/// fmt's `format_float` for the shortest representation of a positive finite value
/// (`binary32` for FLOAT values widened to DOUBLE): fills `buf` with digits `d` and returns
/// `exp` such that the value is `d * 10^exp`.
pub(crate) fn format_shortest(value: f64, binary32: bool, buf: &mut impl DigitBuffer) -> i32 {
    const MIN_EXP: i32 = -60;
    buf.clear();
    if value <= 0.0 {
        buf.push(b'0');
        return 0;
    }
    let (value_fp, lower, upper) = if binary32 {
        float_boundaries(value)
    } else {
        boundaries(value)
    };
    let value_fp = value_fp.normalize(0);
    let (cached, cached_exp10) = cached_power(MIN_EXP - (value_fp.e + SIGNIFICAND_SIZE));
    let scaled = value_fp.mul(cached);
    let lower = multiply(lower, cached.f).wrapping_sub(1);
    let upper = multiply(upper, cached.f).wrapping_add(1);
    let mut handler = ShortestHandler {
        buf,
        diff: upper.wrapping_sub(scaled.f),
    };
    let mut exp = 0;
    let result = gen_digits(
        Fp {
            f: upper,
            e: scaled.e,
        },
        upper.wrapping_sub(lower),
        &mut exp,
        &mut handler,
    );
    if result == Digits::Error {
        exp += handler.buf.digits().len() as i32 - cached_exp10 - 1;
        fallback_format(value, handler.buf, &mut exp);
        return exp;
    }
    exp - cached_exp10
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shortest(value: f64, binary32: bool) -> (String, i32) {
        let mut buf = ShortDigits::new();
        let exp = format_shortest(value, binary32, &mut buf);
        (String::from_utf8(buf.digits().to_vec()).unwrap(), exp)
    }

    #[test]
    fn shortest_digits() {
        assert_eq!(shortest(1.0, false), ("1".to_owned(), 0));
        assert_eq!(shortest(0.1, false), ("1".to_owned(), -1));
        assert_eq!(shortest(123.456, false), ("123456".to_owned(), -3));
        assert_eq!(shortest(1e300, false), ("1".to_owned(), 300));
        // A tie broken to even by the fallback algorithm, unlike Rust's `{:e}`.
        assert_eq!(
            shortest(f64::from(128.6_f32), false),
            ("12860000610351562".to_owned(), -14)
        );
        assert_eq!(shortest(f64::from(0.1f32), true), ("1".to_owned(), -1));
        assert_eq!(shortest(5e-324, false), ("5".to_owned(), -324));
    }

    #[test]
    fn fixed_digits() {
        let fixed = |value: f64| {
            let mut buf = Vec::new();
            let exp = format_fixed(value, 6, &mut buf);
            (String::from_utf8(buf).unwrap(), exp)
        };
        assert_eq!(fixed(1e30).0, "1000000000000000019884624838656000000");
        assert_eq!(fixed(12345.678), ("12345678000".to_owned(), -6));
        // Grisu's 64-bit approximation, not the exact expansion.
        assert!(
            fixed(1e100)
                .0
                .starts_with("100000000000000001586158759891986846923828125")
        );
    }
}
