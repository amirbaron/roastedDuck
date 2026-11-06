//! Scaled-integer decimals (`decimal.c`).
//!
//! As in the original, `precision` is the number of digits after the point and `scale`
//! the number of digits before it. (`decimal_t`'s `flags` are never read and are not
//! kept.)

/// `decimal_t`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Decimal {
    pub(crate) precision: i32,
    pub(crate) scale: i32,
    pub(crate) number: i64,
}

/// An operator of `decimal_t_op`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DecimalOp {
    Plus,
    Minus,
    Mult,
    Div,
}

/// C's `atoi`: optional whitespace and sign, then digits up to the first non-digit.
pub(crate) fn atoi(s: &str) -> i32 {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() && matches!(bytes[i], b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c) {
        i += 1;
    }
    let negative = i < bytes.len() && bytes[i] == b'-';
    if i < bytes.len() && matches!(bytes[i], b'-' | b'+') {
        i += 1;
    }
    let mut value: i32 = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        value = value.wrapping_mul(10).wrapping_add(i32::from(bytes[i] - b'0'));
        i += 1;
    }
    if negative { value.wrapping_neg() } else { value }
}

impl Decimal {
    /// `strtodec`.
    pub(crate) fn from_str(s: &str) -> Self {
        // `valbuf[20]`.
        let (int_part, fraction) = match s.find('.') {
            None => (s, None),
            Some(point) => (&s[..point], Some(&s[point + 1..])),
        };
        let mut decimal = Self {
            scale: int_part.len() as i32,
            number: i64::from(atoi(int_part)),
            precision: 0,
        };
        if let Some(fraction) = fraction {
            decimal.precision = fraction.len() as i32;
            for _ in 0..decimal.precision {
                decimal.number *= 10;
            }
            decimal.number += i64::from(atoi(fraction));
        }
        if s.starts_with('-') && decimal.number > 0 {
            decimal.number *= -1;
        }
        decimal
    }

    /// `itodec`.
    pub(crate) fn from_int(src: i32) -> Self {
        let mut scale = 1;
        let mut bound: i32 = 1;
        while bound.wrapping_mul(10) <= src {
            scale += 1;
            bound = bound.wrapping_mul(10);
        }
        Self {
            precision: 0,
            scale,
            number: i64::from(src),
        }
    }

    /// `NegateDecimal`.
    pub(crate) fn negate(&mut self) {
        self.number *= -1;
    }

    /// `decimal_t_op(dest, op, d1, d2)` with a `dest` that is neither operand.
    pub(crate) fn op(op: DecimalOp, d1: &Self, d2: &Self) -> Self {
        let mut dest = Self {
            scale: d1.scale.max(d2.scale),
            precision: if d1.precision > d2.precision {
                d1.precision
            } else {
                d2.precision
            },
            number: 0,
        };
        match op {
            DecimalOp::Plus => dest.number = d1.number + d2.number,
            DecimalOp::Minus => dest.number = d1.number - d2.number,
            DecimalOp::Mult => {
                let mut res = d1.precision + d2.precision;
                dest.number = d1.number.wrapping_mul(d2.number);
                while res > dest.precision {
                    res -= 1;
                    dest.number /= 10;
                }
            }
            DecimalOp::Div => {
                // In `float`, as the original.
                let mut f1 = d1.number as f32;
                let mut np = d1.precision;
                while np < dest.precision {
                    f1 *= 10.0;
                    np += 1;
                }
                np = 0;
                while np < dest.precision {
                    f1 *= 10.0;
                    np += 1;
                }
                let mut f2 = d2.number as f32;
                np = d2.precision;
                while np < dest.precision {
                    f2 *= 10.0;
                    np += 1;
                }
                dest.number = i64::from((f1 / f2) as i32);
            }
        }
        dest
    }

    /// `decimal_t_op(d1, op, d1, d2)`: `dest` is the first operand, whose precision the
    /// original overwrites before reading it.
    pub(crate) fn op_assign(&mut self, op: DecimalOp, d2: &Self) {
        let mut d1 = *self;
        d1.scale = d1.scale.max(d2.scale);
        if d2.precision >= d1.precision {
            d1.precision = d2.precision;
        }
        *self = Self::op(op, &d1, d2);
    }
}
