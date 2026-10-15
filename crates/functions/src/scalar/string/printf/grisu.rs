//! The fixed-precision Grisu digit generation of `{fmt}` 6 (`format_float` with a
//! precision, `grisu_gen_digits` and `fixed_handler` in `format-inl.h`).
//!
//! Grisu works on a 64-bit approximation of the value. When the rounding of the last
//! digit is uncertain it reports an error and `{fmt}` falls back to `snprintf`; but with
//! many requested digits (fixed notation of large values) it keeps emitting the digits of
//! the approximation, which differ from the exact decimal expansion. Those digits are
//! part of DuckDB's output, so they are reproduced here bit for bit.

/// Normalized 64-bit significands of `10^k` for `k = -348, -340, ..., 340`.
const POW10_SIGNIFICANDS: [u64; 87] = [
    0xfa8f_d5a0_081c_0288,
    0xbaae_e17f_a23e_bf76,
    0x8b16_fb20_3055_ac76,
    0xcf42_894a_5dce_35ea,
    0x9a6b_b0aa_5565_3b2d,
    0xe61a_cf03_3d1a_45df,
    0xab70_fe17_c79a_c6ca,
    0xff77_b1fc_bebc_dc4f,
    0xbe56_91ef_416b_d60c,
    0x8dd0_1fad_907f_fc3c,
    0xd351_5c28_3155_9a83,
    0x9d71_ac8f_ada6_c9b5,
    0xea9c_2277_23ee_8bcb,
    0xaecc_4991_4078_536d,
    0x823c_1279_5db6_ce57,
    0xc210_9436_4dfb_5637,
    0x9096_ea6f_3848_984f,
    0xd774_85cb_2582_3ac7,
    0xa086_cfcd_97bf_97f4,
    0xef34_0a98_172a_ace5,
    0xb238_67fb_2a35_b28e,
    0x84c8_d4df_d2c6_3f3b,
    0xc5dd_4427_1ad3_cdba,
    0x936b_9fce_bb25_c996,
    0xdbac_6c24_7d62_a584,
    0xa3ab_6658_0d5f_daf6,
    0xf3e2_f893_dec3_f126,
    0xb5b5_ada8_aaff_80b8,
    0x8762_5f05_6c7c_4a8b,
    0xc9bc_ff60_34c1_3053,
    0x964e_858c_91ba_2655,
    0xdff9_7724_7029_7ebd,
    0xa6df_bd9f_b8e5_b88f,
    0xf8a9_5fcf_8874_7d94,
    0xb944_7093_8fa8_9bcf,
    0x8a08_f0f8_bf0f_156b,
    0xcdb0_2555_6531_31b6,
    0x993f_e2c6_d07b_7fac,
    0xe45c_10c4_2a2b_3b06,
    0xaa24_2499_6973_92d3,
    0xfd87_b5f2_8300_ca0e,
    0xbce5_0864_9211_1aeb,
    0x8cbc_cc09_6f50_88cc,
    0xd1b7_1758_e219_652c,
    0x9c40_0000_0000_0000,
    0xe8d4_a510_0000_0000,
    0xad78_ebc5_ac62_0000,
    0x813f_3978_f894_0984,
    0xc097_ce7b_c907_15b3,
    0x8f7e_32ce_7bea_5c70,
    0xd5d2_38a4_abe9_8068,
    0x9f4f_2726_179a_2245,
    0xed63_a231_d4c4_fb27,
    0xb0de_6538_8cc8_ada8,
    0x83c7_088e_1aab_65db,
    0xc45d_1df9_4271_1d9a,
    0x924d_692c_a61b_e758,
    0xda01_ee64_1a70_8dea,
    0xa26d_a399_9aef_774a,
    0xf209_787b_b47d_6b85,
    0xb454_e4a1_79dd_1877,
    0x865b_8692_5b9b_c5c2,
    0xc835_53c5_c896_5d3d,
    0x952a_b45c_fa97_a0b3,
    0xde46_9fbd_99a0_5fe3,
    0xa59b_c234_db39_8c25,
    0xf6c6_9a72_a398_9f5c,
    0xb7dc_bf53_54e9_bece,
    0x88fc_f317_f222_41e2,
    0xcc20_ce9b_d35c_78a5,
    0x9816_5af3_7b21_53df,
    0xe2a0_b5dc_971f_303a,
    0xa8d9_d153_5ce3_b396,
    0xfb9b_7cd9_a4a7_443c,
    0xbb76_4c4c_a7a4_4410,
    0x8bab_8eef_b640_9c1a,
    0xd01f_ef10_a657_842c,
    0x9b10_a4e5_e991_3129,
    0xe710_9bfb_a19c_0c9d,
    0xac28_20d9_623b_f429,
    0x8044_4b5e_7aa7_cf85,
    0xbf21_e440_03ac_dd2d,
    0x8e67_9c2f_5e44_ff8f,
    0xd433_179d_9c8c_b841,
    0x9e19_db92_b4e3_1ba9,
    0xeb96_bf6e_badf_77d9,
    0xaf87_023b_9bf0_ee6b,
];

/// Binary exponents of the cached powers of ten above.
const POW10_EXPONENTS: [i32; 87] = [
    -1220, -1193, -1166, -1140, -1113, -1087, -1060, -1034, -1007, -980, -954, -927, -901, -874, -847, -821,
    -794, -768, -741, -715, -688, -661, -635, -608, -582, -555, -529, -502, -475, -449, -422, -396, -369,
    -343, -316, -289, -263, -236, -210, -183, -157, -130, -103, -77, -50, -24, 3, 30, 56, 83, 109, 136, 162,
    189, 216, 242, 269, 295, 322, 348, 375, 402, 428, 455, 481, 508, 534, 561, 588, 614, 641, 667, 694, 720,
    747, 774, 800, 827, 853, 880, 907, 933, 960, 986, 1013, 1039, 1066,
];

/// `10^k` for `k = 0..=19`.
const POWERS_OF_10: [u64; 20] = {
    let mut powers = [1u64; 20];
    let mut i = 1;
    while i < 20 {
        powers[i] = powers[i - 1] * 10;
        i += 1;
    }
    powers
};

/// `f * 2^e`.
#[derive(Clone, Copy, Debug)]
struct Fp {
    f: u64,
    e: i32,
}

impl Fp {
    /// `fp(double)` followed by `normalize`.
    fn normalized(value: f64) -> Self {
        const SIGNIFICAND_BITS: u32 = 52;
        const IMPLICIT_BIT: u64 = 1 << SIGNIFICAND_BITS;
        let bits = value.to_bits();
        let mut f = bits & (IMPLICIT_BIT - 1);
        let mut biased = ((bits >> SIGNIFICAND_BITS) & 0x7FF) as i32;
        if biased != 0 {
            f += IMPLICIT_BIT;
        } else {
            biased = 1;
        }
        let mut e = biased - 1075;
        while f & IMPLICIT_BIT == 0 {
            f <<= 1;
            e -= 1;
        }
        // Shift the implicit bit to bit 63.
        Self {
            f: f << 11,
            e: e - 11,
        }
    }

    /// `operator*`: the product rounded half up to 64 bits.
    fn multiply(self, other: Self) -> Self {
        let product = u128::from(self.f) * u128::from(other.f);
        let high = (product >> 64) as u64;
        let f = if product & (1 << 63) != 0 { high + 1 } else { high };
        Self {
            f,
            e: self.e + other.e + 64,
        }
    }
}

/// `get_cached_power`: a power of ten whose exponent lands in `[min_exponent,
/// min_exponent + 28]`, and its decimal exponent.
fn cached_power(min_exponent: i32) -> (Fp, i32) {
    const ONE_OVER_LOG2_10: i64 = 0x4d10_4d42;
    let index = ((i64::from(min_exponent + 63) * ONE_OVER_LOG2_10 + ((1i64 << 32) - 1)) >> 32) as i32;
    let index = (index + 348 - 1) / 8 + 1;
    let index = usize::try_from(index)
        .unwrap_or(0)
        .min(POW10_SIGNIFICANDS.len() - 1);
    let power = Fp {
        f: POW10_SIGNIFICANDS[index],
        e: POW10_EXPONENTS[index],
    };
    (power, -348 + index as i32 * 8)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Round {
    Unknown,
    Up,
    Down,
}

/// `get_round_direction`.
fn round_direction(divisor: u64, remainder: u64, error: u64) -> Round {
    if remainder <= divisor - remainder && error.wrapping_mul(2) <= divisor - remainder * 2 {
        return Round::Down;
    }
    if remainder >= error && remainder - error >= divisor - (remainder - error) {
        return Round::Up;
    }
    Round::Unknown
}

/// The outcome of a digit callback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    More,
    Done,
    Error,
}

/// `fixed_handler`.
struct FixedHandler {
    digits: Vec<u8>,
    precision: i32,
    exp10: i32,
    fixed: bool,
}

impl FixedHandler {
    fn on_start(&mut self, divisor: u64, remainder: u64, error: u64, exp: i32) -> Step {
        if !self.fixed {
            return Step::More;
        }
        self.precision += exp + self.exp10;
        if self.precision > 0 {
            return Step::More;
        }
        if self.precision < 0 {
            return Step::Done;
        }
        match round_direction(divisor, remainder, error) {
            Round::Unknown => Step::Error,
            direction => {
                self.digits.push(if direction == Round::Up { b'1' } else { b'0' });
                Step::Done
            }
        }
    }

    fn on_digit(&mut self, digit: u8, divisor: u64, remainder: u64, error: u64, integral: bool) -> Step {
        self.digits.push(digit);
        if (self.digits.len() as i64) < i64::from(self.precision) {
            return Step::More;
        }
        if !integral && (error >= divisor || error >= divisor - error) {
            return Step::Error;
        }
        match round_direction(divisor, remainder, error) {
            Round::Down => return Step::Done,
            Round::Unknown => return Step::Error,
            Round::Up => {}
        }
        let size = self.digits.len();
        self.digits[size - 1] += 1;
        let mut i = size - 1;
        while i > 0 && self.digits[i] > b'9' {
            self.digits[i] = b'0';
            self.digits[i - 1] += 1;
            i -= 1;
        }
        if self.digits[0] > b'9' {
            self.digits[0] = b'1';
            self.digits.push(b'0');
        }
        Step::Done
    }
}

/// `grisu_gen_digits` with a [`FixedHandler`]; updates `exp` (kappa).
fn gen_digits(value: Fp, mut error: u64, exp: &mut i32, handler: &mut FixedHandler) -> Step {
    let shift = (-value.e) as u32;
    let one = 1u64 << shift;
    let mut integral = (value.f >> shift) as u32;
    let mut fractional = value.f & (one - 1);
    *exp = integral.checked_ilog10().map_or(1, |log| log as i32 + 1);
    let result = handler.on_start(
        POWERS_OF_10[(*exp - 1) as usize] << shift,
        value.f / 10,
        error.wrapping_mul(10),
        *exp,
    );
    if result != Step::More {
        return result;
    }
    loop {
        let divisor = POWERS_OF_10[(*exp - 1) as usize] as u32;
        let digit = integral / divisor;
        integral %= divisor;
        *exp -= 1;
        let remainder = (u64::from(integral) << shift).wrapping_add(fractional);
        let result = handler.on_digit(
            b'0' + digit as u8,
            POWERS_OF_10[*exp as usize] << shift,
            remainder,
            error,
            true,
        );
        if result != Step::More {
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
        let result = handler.on_digit(digit, one, fractional, error, false);
        if result != Step::More {
            return result;
        }
    }
}

/// `format_float` for a positive finite value and `0 <= precision <= 17`: the digits and
/// decimal exponent, or `None` when `{fmt}` falls back to `snprintf`.
pub(super) fn format_float(value: f64, precision: i32, fixed: bool) -> Option<(Vec<u8>, i32)> {
    const MIN_EXP: i32 = -60;
    let normalized = Fp::normalized(value);
    let (power, cached_exp10) = cached_power(MIN_EXP - (normalized.e + 64));
    let scaled = normalized.multiply(power);
    let mut handler = FixedHandler {
        digits: Vec::with_capacity(32),
        precision,
        exp10: -cached_exp10,
        fixed,
    };
    let mut exp = 0;
    if gen_digits(scaled, 1, &mut exp, &mut handler) == Step::Error {
        return None;
    }
    let mut digits = handler.digits;
    if !fixed {
        while digits.last() == Some(&b'0') {
            digits.pop();
            exp += 1;
        }
    }
    Some((digits, exp - cached_exp10))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grisu_matches_fmt() {
        let (digits, exp) = format_float(1.5, 3, false).unwrap();
        assert_eq!((String::from_utf8(digits).unwrap().as_str(), exp), ("15", -1));
        let (digits, exp) = format_float(0.1, 2, true).unwrap();
        assert_eq!((String::from_utf8(digits).unwrap().as_str(), exp), ("10", -2));
    }
}
