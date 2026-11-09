//! Native long-double rounding for wide-integer AVG finalization.
//!
//! C++ divides in binary128 on Linux arm64 and extended80 on Unix `x86_64`,
//! then rounds to DOUBLE. Preserve binary64 finalization on the other targets.
//! Fixed-size integer arithmetic keeps the aggregate state and row update path unchanged.

use roastedduck_function::cast::numeric::i128_to_f64;

pub(super) fn average(sum: i128, count: u64, scale: Option<f64>) -> f64 {
    if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        average_at_precision(sum, count, scale.unwrap_or(1.0), 113)
    } else if cfg!(all(
        target_arch = "x86_64",
        any(target_os = "linux", target_vendor = "apple")
    )) {
        average_at_precision(sum, count, scale.unwrap_or(1.0), 64)
    } else {
        let count = count as f64;
        let denominator = match scale {
            Some(scale) => count * scale,
            None => count,
        };
        i128_to_f64(sum) / denominator
    }
}

#[derive(Clone, Copy)]
struct Binary {
    significand: u128,
    exponent: i32,
}

fn round_shift(value: u128, shift: u32) -> u128 {
    if shift == 0 {
        return value;
    }
    let quotient = value >> shift;
    let remainder = value & ((1u128 << shift) - 1);
    let half = 1u128 << (shift - 1);
    quotient + u128::from(remainder > half || (remainder == half && quotient & 1 != 0))
}

fn from_unsigned(value: u128, precision: u32) -> Binary {
    debug_assert!(value != 0);
    let bits = 128 - value.leading_zeros();
    if bits <= precision {
        Binary {
            significand: value << (precision - bits),
            exponent: bits as i32 - precision as i32,
        }
    } else {
        let shift = bits - precision;
        let mut significand = round_shift(value, shift);
        let mut exponent = shift as i32;
        if significand == 1u128 << precision {
            significand >>= 1;
            exponent += 1;
        }
        Binary {
            significand,
            exponent,
        }
    }
}

fn divide(numerator: Binary, denominator: Binary, precision: u32) -> Binary {
    // Common singleton/power-of-two counts need only an exponent adjustment.
    if denominator.significand.is_power_of_two() {
        return Binary {
            significand: numerator.significand,
            exponent: numerator.exponent - denominator.exponent - (precision as i32 - 1),
        };
    }
    let mut remainder = numerator.significand;
    let mut exponent = numerator.exponent - denominator.exponent;
    if remainder < denominator.significand {
        remainder <<= 1;
        exponent -= 1;
    }
    remainder -= denominator.significand;
    let mut significand = 1u128;
    for _ in 1..precision {
        remainder <<= 1;
        significand <<= 1;
        if remainder >= denominator.significand {
            significand |= 1;
            remainder -= denominator.significand;
        }
    }
    let twice = remainder << 1;
    if twice > denominator.significand || (twice == denominator.significand && significand & 1 != 0) {
        significand += 1;
    }
    if significand == 1u128 << precision {
        significand >>= 1;
        exponent += 1;
    }
    Binary {
        significand,
        exponent: exponent - (precision as i32 - 1),
    }
}

fn average_at_precision(sum: i128, count: u64, scale: f64, precision: u32) -> f64 {
    debug_assert!(matches!(precision, 64 | 113));
    debug_assert!(count != 0 && scale >= 1.0 && scale.is_finite());
    if sum == 0 {
        return 0.0;
    }
    let numerator = from_unsigned(sum.unsigned_abs(), precision);
    let scale_bits = scale.to_bits();
    let scale_significand = (1u128 << 52) | u128::from(scale_bits & ((1u64 << 52) - 1));
    let scale_exponent = ((scale_bits >> 52) & 0x7ff) as i32 - 1023 - 52;
    let mut denominator = from_unsigned(u128::from(count) * scale_significand, precision);
    denominator.exponent += scale_exponent;
    let result = divide(numerator, denominator, precision);
    let shift = precision - 53;
    let mut significand = round_shift(result.significand, shift);
    let mut exponent = result.exponent + shift as i32;
    if significand == 1u128 << 53 {
        significand >>= 1;
        exponent += 1;
    }
    let unbiased = exponent + 52;
    debug_assert!((-1022..=1023).contains(&unbiased));
    let bits = (u64::from(sum < 0) << 63)
        | (((unbiased + 1023) as u64) << 52)
        | (significand as u64 & ((1u64 << 52) - 1));
    f64::from_bits(bits)
}

#[cfg(test)]
mod tests {
    use super::{average, average_at_precision};

    #[test]
    fn native_average_uses_the_reference_precision() {
        let wide = cfg!(all(target_os = "linux", target_arch = "aarch64"))
            || cfg!(all(
                target_arch = "x86_64",
                any(target_os = "linux", target_vendor = "apple")
            ));
        let expected: f64 = if wide {
            3_002_399_751_580_331.0
        } else {
            3_002_399_751_580_330.5
        };
        assert_eq!(
            average(9_007_199_254_740_993, 3, None).to_bits(),
            expected.to_bits()
        );
    }

    #[test]
    fn extended_results_match_native_cpp_bit_controls() {
        // Frozen C++ binary128 and native Intel extended80 expression results.
        let cases: [(i128, u64, f64, u64, u64); 7] = [
            (
                9_007_199_254_740_993,
                3,
                1.0,
                0x4325_5555_5555_5556,
                0x4325_5555_5555_5556,
            ),
            (
                -9_007_199_254_740_993,
                3,
                1.0,
                0xc325_5555_5555_5556,
                0xc325_5555_5555_5556,
            ),
            (
                18_446_744_073_709_553_665,
                1,
                1.0,
                0x43f0_0000_0000_0000,
                0x43f0_0000_0000_0001,
            ),
            (
                10_384_593_717_069_656_409_982_497_265_287_169,
                1,
                1.0,
                0x4700_0000_0000_0000,
                0x4700_0000_0000_0000,
            ),
            (
                i128::MAX,
                u64::MAX,
                1e38,
                0x3bfb_38fb_9daa_78e4,
                0x3bfb_38fb_9daa_78e4,
            ),
            (i128::MIN, 3, 1.0, 0xc7c5_5555_5555_5555, 0xc7c5_5555_5555_5555),
            (-1, 7, 1e23, 0xbafb_a1f1_6dcc_3982, 0xbafb_a1f1_6dcc_3982),
        ];
        for (sum, count, scale, extended80, binary128) in cases {
            assert_eq!(average_at_precision(sum, count, scale, 64).to_bits(), extended80);
            assert_eq!(average_at_precision(sum, count, scale, 113).to_bits(), binary128);
        }
    }

    #[test]
    fn zero_sum_preserves_positive_zero() {
        for precision in [64, 113] {
            assert_eq!(average_at_precision(0, u64::MAX, 1e38, precision).to_bits(), 0);
        }
    }
}
