//! The value writers of DuckDB's bundled `{fmt}` 6 fork (`basic_writer`,
//! `arg_formatter_base` and `float_writer` in `third_party/fmt/include/fmt/format.h`),
//! shared by `printf` and `format`.
//!
//! Widths and precisions count bytes, as `{fmt}` does for `char` strings. The locale is
//! DuckDB's fixed one: groups of three digits and `.` as the decimal point.

use roastedduck_common::{Error, Result};

/// Text alignment (`fmt::align`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Align {
    None,
    Left,
    Right,
    Center,
    Numeric,
}

/// Sign display (`fmt::sign`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Sign {
    None,
    Minus,
    Plus,
    Space,
}

impl Sign {
    /// `data::signs[sign]`.
    fn byte(self) -> Option<u8> {
        match self {
            Self::None => None,
            Self::Minus => Some(b'-'),
            Self::Plus => Some(b'+'),
            Self::Space => Some(b' '),
        }
    }
}

/// `basic_format_specs<char>`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Specs {
    pub(crate) width: usize,
    /// `-1` when absent.
    pub(crate) precision: i32,
    /// `0` when absent.
    pub(crate) ty: u8,
    pub(crate) align: Align,
    pub(crate) sign: Sign,
    pub(crate) alt: bool,
    pub(crate) fill: u8,
    /// The thousands separator, `0` when absent (a DuckDB extension).
    pub(crate) thousands: u8,
}

impl Default for Specs {
    fn default() -> Self {
        Self {
            width: 0,
            precision: -1,
            ty: 0,
            align: Align::None,
            sign: Sign::None,
            alt: false,
            fill: b' ',
            thousands: 0,
        }
    }
}

/// A formatting failure (DuckDB throws them as `InvalidInputException`).
pub(crate) fn format_error(message: impl Into<String>) -> Error {
    Error::invalid_input(message.into())
}

fn invalid_type(ty: u8, kind: &str) -> Error {
    format_error(format!(
        "Invalid type specifier \"{}\" for formatting a value of type {kind}",
        char::from(ty)
    ))
}

/// `basic_writer::write_padded`: `body` of `size` bytes, padded to the width.
pub(crate) fn write_padded(out: &mut Vec<u8>, specs: &Specs, size: usize, body: impl FnOnce(&mut Vec<u8>)) {
    let width = specs.width;
    if width <= size {
        body(out);
        return;
    }
    let padding = width - size;
    let fill = |out: &mut Vec<u8>, count: usize| out.extend(std::iter::repeat_n(specs.fill, count));
    match specs.align {
        Align::Right => {
            fill(out, padding);
            body(out);
        }
        Align::Center => {
            let left = padding / 2;
            fill(out, left);
            body(out);
            fill(out, padding - left);
        }
        _ => {
            body(out);
            fill(out, padding);
        }
    }
}

/// Writes `bytes` with the string specifiers (`basic_writer::write(basic_string_view, specs)`).
pub(crate) fn write_str(out: &mut Vec<u8>, value: &[u8], specs: &Specs) {
    let size = match usize::try_from(specs.precision) {
        Ok(precision) if precision < value.len() => precision,
        _ => value.len(),
    };
    write_padded(out, specs, size, |out| out.extend_from_slice(&value[..size]));
}

/// A string argument (`arg_formatter_base::operator()(string_view)`).
pub(crate) fn format_string(out: &mut Vec<u8>, value: &[u8], specs: Option<&Specs>) -> Result<()> {
    match specs {
        None => out.extend_from_slice(value),
        Some(specs) => {
            if specs.ty != 0 && specs.ty != b's' {
                return Err(invalid_type(specs.ty, "string"));
            }
            write_str(out, value, specs);
        }
    }
    Ok(())
}

/// A boolean argument (`arg_formatter_base::operator()(bool)`).
pub(crate) fn format_bool(out: &mut Vec<u8>, value: bool, specs: Option<&Specs>) -> Result<()> {
    match specs {
        Some(specs) if specs.ty != 0 => format_int(out, false, u128::from(value), Some(specs)),
        Some(specs) => {
            write_str(out, if value { b"true" } else { b"false" }, specs);
            Ok(())
        }
        None => {
            out.extend_from_slice(if value { b"true" } else { b"false" });
            Ok(())
        }
    }
}

/// A single byte (`arg_formatter_base::write_char`, used for `%c`).
pub(crate) fn format_char(out: &mut Vec<u8>, value: u8, specs: &Specs) {
    write_padded(out, specs, 1, |out| out.push(value));
}

/// Decimal digits of `value`, with `separator` between groups of three.
fn decimal_digits(value: u128, separator: Option<u8>) -> Vec<u8> {
    let digits = value.to_string().into_bytes();
    let Some(separator) = separator else {
        return digits;
    };
    let mut grouped = Vec::with_capacity(digits.len() + digits.len() / 3);
    for (index, &digit) in digits.iter().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(separator);
        }
        grouped.push(digit);
    }
    grouped
}

/// `basic_writer::write_int`: `<left-padding><prefix><numeric-padding><digits><right-padding>`.
fn write_int_body(out: &mut Vec<u8>, prefix: &[u8], digits: &[u8], specs: &Specs) {
    let mut size = prefix.len() + digits.len();
    let mut fill = specs.fill;
    let mut padding = 0;
    if specs.align == Align::Numeric {
        if specs.width > size {
            padding = specs.width - size;
            size = specs.width;
        }
    } else if let Ok(precision) = usize::try_from(specs.precision)
        && precision > digits.len()
    {
        size = prefix.len() + precision;
        padding = precision - digits.len();
        fill = b'0';
    }
    let mut padded = *specs;
    if padded.align == Align::None {
        padded.align = Align::Right;
    }
    write_padded(out, &padded, size, |out| {
        out.extend_from_slice(prefix);
        out.extend(std::iter::repeat_n(fill, padding));
        out.extend_from_slice(digits);
    });
}

/// An integer argument (`int_writer` driven by `handle_int_type_spec`): `negative` and
/// the magnitude.
pub(crate) fn format_int(
    out: &mut Vec<u8>,
    negative: bool,
    magnitude: u128,
    specs: Option<&Specs>,
) -> Result<()> {
    let Some(specs) = specs else {
        if negative {
            out.push(b'-');
        }
        out.extend_from_slice(magnitude.to_string().as_bytes());
        return Ok(());
    };
    let mut prefix = Vec::with_capacity(4);
    if negative {
        prefix.push(b'-');
    } else if let Some(sign) = specs.sign.byte().filter(|_| specs.sign != Sign::Minus) {
        prefix.push(sign);
    }
    if specs.thousands != 0 {
        let digits = decimal_digits(magnitude, Some(specs.thousands));
        write_int_body(out, &prefix, &digits, specs);
        return Ok(());
    }
    let digits = match specs.ty {
        0 | b'd' | b'n' | b'l' | b'L' => magnitude.to_string().into_bytes(),
        b'x' | b'X' => {
            if specs.alt {
                prefix.extend_from_slice(&[b'0', specs.ty]);
            }
            if specs.ty == b'x' {
                format!("{magnitude:x}").into_bytes()
            } else {
                format!("{magnitude:X}").into_bytes()
            }
        }
        b'b' | b'B' => {
            if specs.alt {
                prefix.extend_from_slice(&[b'0', specs.ty]);
            }
            format!("{magnitude:b}").into_bytes()
        }
        b'o' => {
            let digits = format!("{magnitude:o}").into_bytes();
            let precision_covers = usize::try_from(specs.precision).map_or(true, |p| p <= digits.len());
            if specs.alt && precision_covers && magnitude != 0 {
                // The octal prefix '0' counts as a digit.
                prefix.push(b'0');
            }
            digits
        }
        ty => return Err(invalid_type(ty, "int")),
    };
    write_int_body(out, &prefix, &digits, specs);
    Ok(())
}

/// `float_format`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FloatFormat {
    General,
    Exp,
    Fixed,
    Hex,
}

/// `float_specs`.
#[derive(Clone, Copy, Debug)]
struct FloatSpecs {
    precision: i32,
    format: FloatFormat,
    sign: Sign,
    thousand_sep: u8,
    upper: bool,
    trailing_zeros: bool,
}

/// `parse_float_type_spec`.
fn parse_float_type_spec(specs: &Specs) -> Result<(FloatSpecs, bool)> {
    let mut result = FloatSpecs {
        precision: 0,
        format: FloatFormat::General,
        sign: Sign::None,
        thousand_sep: specs.thousands,
        upper: false,
        trailing_zeros: specs.alt,
    };
    let mut locale = false;
    match specs.ty {
        0 => result.trailing_zeros |= specs.precision != 0,
        b'G' | b'g' => result.upper = specs.ty == b'G',
        b'E' | b'e' => {
            result.upper = specs.ty == b'E';
            result.format = FloatFormat::Exp;
            result.trailing_zeros |= specs.precision != 0;
        }
        b'F' | b'f' => {
            result.upper = specs.ty == b'F';
            result.format = FloatFormat::Fixed;
            result.trailing_zeros |= specs.precision != 0;
        }
        b'A' | b'a' => {
            result.upper = specs.ty == b'A';
            result.format = FloatFormat::Hex;
        }
        b'n' | b'l' | b'L' => locale = true,
        ty => return Err(invalid_type(ty, "float")),
    }
    Ok((result, locale))
}

/// `snprintf_float` for the decimal formats: the digits and exponent of `%.*f` or
/// `%.*e`, which round exactly (ties to even) like Rust's formatting.
fn snprintf_digits(value: f64, precision: i32, fixed: bool) -> (Vec<u8>, i32) {
    if fixed {
        let text = format!("{value:.*}", usize::try_from(precision).unwrap_or(6));
        if precision == 0 {
            return (text.into_bytes(), 0);
        }
        let fraction = text.len() - text.find('.').map_or(text.len(), |dot| dot + 1);
        let digits: Vec<u8> = text.bytes().filter(|&b| b != b'.').collect();
        return (digits, -(fraction as i32));
    }
    // `%e` with one digit fewer than the precision; a negative precision means `%e`.
    let decimals = usize::try_from(precision - 1).unwrap_or(6);
    let text = format!("{value:.decimals$e}");
    let (mantissa, exponent) = text.split_once('e').unwrap_or((&text, "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let mut digits: Vec<u8> = mantissa.bytes().filter(|&b| b != b'.').collect();
    while digits.len() > 1 && digits.last() == Some(&b'0') {
        digits.pop();
    }
    let fraction = digits.len() as i32 - 1;
    (digits, exponent - fraction)
}

/// `format_float`: the decimal digits and exponent of a non-negative finite `value`
/// (`value = digits * 10^exp`): the shortest round-trip digits for a negative precision,
/// otherwise Grisu's fixed-precision digits with `{fmt}`'s `snprintf` fallback.
fn float_digits(value: f64, precision: i32, fixed: bool) -> (Vec<u8>, i32) {
    if value <= 0.0 {
        return match usize::try_from(precision) {
            Ok(precision) if precision > 0 && fixed => (vec![b'0'; precision], -(precision as i32)),
            _ => (vec![b'0'], 0),
        };
    }
    if precision >= 0 {
        if precision <= 17
            && let Some(result) = super::grisu::format_float(value, precision, fixed)
        {
            return result;
        }
        return snprintf_digits(value, precision, fixed);
    }
    // The shortest representation (Grisu with a Dragon4 fallback in `{fmt}`).
    let text = format!("{value:e}");
    let (mantissa, exponent) = text.split_once('e').unwrap_or((&text, "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let digits: Vec<u8> = mantissa.bytes().filter(|&b| b != b'.').collect();
    let exp = exponent - (digits.len() as i32 - 1);
    (digits, exp)
}

/// `write_exponent`: `[+-]d{2,3}`.
fn write_exponent(out: &mut Vec<u8>, exp: i32) {
    out.push(if exp < 0 { b'-' } else { b'+' });
    let exp = exp.unsigned_abs();
    if exp < 10 {
        out.push(b'0');
    }
    out.extend_from_slice(exp.to_string().as_bytes());
}

/// `float_writer::prettify`.
fn prettify(out: &mut Vec<u8>, digits: &[u8], exp: i32, specs: &FloatSpecs, point: u8) {
    let num_digits = digits.len() as i32;
    let full_exp = num_digits + exp;
    let zeros = |out: &mut Vec<u8>, count: i32| out.extend(std::iter::repeat_n(b'0', count.max(0) as usize));
    if specs.format == FloatFormat::Exp {
        out.push(digits[0]);
        let num_zeros = specs.precision - num_digits;
        let trailing_zeros = num_zeros > 0 && specs.trailing_zeros;
        if num_digits > 1 || trailing_zeros {
            out.push(point);
        }
        out.extend_from_slice(&digits[1..]);
        if trailing_zeros {
            zeros(out, num_zeros);
        }
        out.push(if specs.upper { b'E' } else { b'e' });
        write_exponent(out, full_exp - 1);
        return;
    }
    let grouped = |out: &mut Vec<u8>, count: i32, digit_at: &dyn Fn(i32) -> u8| {
        let mut group = if full_exp % 3 == 0 { 3 } else { full_exp % 3 };
        let mut i = 0;
        while i < count {
            if i > 0 {
                out.push(specs.thousand_sep);
            }
            for j in i..i + group {
                out.push(digit_at(j));
            }
            i += group;
            group = 3;
        }
    };
    let digit_or_zero = |index: i32| digits.get(index as usize).copied().unwrap_or(b'0');
    if num_digits <= full_exp {
        // 1234e7 -> 12340000000[.0+]
        if specs.thousand_sep != 0 && full_exp > 3 {
            grouped(out, full_exp, &digit_or_zero);
        } else {
            out.extend_from_slice(digits);
            zeros(out, full_exp - num_digits);
        }
        if specs.trailing_zeros {
            out.push(point);
            let num_zeros = specs.precision - full_exp;
            if num_zeros <= 0 {
                if specs.format != FloatFormat::Fixed {
                    out.push(b'0');
                }
                return;
            }
            zeros(out, num_zeros);
        }
    } else if full_exp > 0 {
        // 1234e-2 -> 12.34[0+]
        if specs.thousand_sep != 0 && full_exp > 3 {
            grouped(out, full_exp, &digit_or_zero);
        } else {
            out.extend_from_slice(&digits[..full_exp as usize]);
        }
        if !specs.trailing_zeros {
            let mut end = digits.len();
            while end > full_exp as usize && digits[end - 1] == b'0' {
                end -= 1;
            }
            if end != full_exp as usize {
                out.push(point);
            }
            out.extend_from_slice(&digits[full_exp as usize..end]);
            return;
        }
        out.push(point);
        out.extend_from_slice(&digits[full_exp as usize..]);
        if specs.precision > num_digits {
            zeros(out, specs.precision - num_digits);
        }
    } else {
        // 1234e-6 -> 0.001234
        out.push(b'0');
        let mut num_zeros = -full_exp;
        if num_digits == 0 && specs.precision >= 0 && specs.precision < num_zeros {
            num_zeros = specs.precision;
        }
        let mut end = digits.len();
        if !specs.trailing_zeros {
            while end > 0 && digits[end - 1] == b'0' {
                end -= 1;
            }
        }
        if num_zeros != 0 || end != 0 {
            out.push(point);
            zeros(out, num_zeros);
            out.extend_from_slice(&digits[..end]);
        }
    }
}

/// C's `%a`/`%A` of a non-negative finite value, with an optional precision (hex digits
/// after the point) and `#` (keep the point).
fn hex_float(value: f64, precision: i32, alt: bool, upper: bool) -> Vec<u8> {
    let bits = value.to_bits();
    let biased = ((bits >> 52) & 0x7FF) as i32;
    let mut mantissa = bits & ((1u64 << 52) - 1);
    let (mut lead, exp) = if biased == 0 {
        if mantissa == 0 { (0u64, 0) } else { (0u64, -1022) }
    } else {
        (1u64, biased - 1023)
    };
    let mut hex_digits = 13usize;
    if let Ok(precision) = usize::try_from(precision)
        && precision < 13
    {
        // Round the mantissa to `precision` hex digits, ties to even.
        let drop = (13 - precision) * 4;
        let full = (lead << 52) | mantissa;
        let mask = (1u64 << drop) - 1;
        let remainder = full & mask;
        let half = 1u64 << (drop - 1);
        let mut kept = full >> drop;
        if remainder > half || (remainder == half && kept & 1 == 1) {
            kept += 1;
        }
        let rounded = kept << drop;
        lead = rounded >> 52;
        mantissa = rounded & ((1u64 << 52) - 1);
        hex_digits = precision;
    }
    let mut out = Vec::new();
    out.extend_from_slice(if upper { b"0X" } else { b"0x" });
    out.extend_from_slice(lead.to_string().as_bytes());
    let mut fraction = format!("{mantissa:013x}");
    fraction.truncate(hex_digits);
    if precision < 0 {
        while fraction.ends_with('0') {
            fraction.pop();
        }
    } else {
        while fraction.len() < precision as usize {
            fraction.push('0');
        }
    }
    if !fraction.is_empty() || alt {
        out.push(b'.');
    }
    out.extend_from_slice(fraction.as_bytes());
    out.push(if upper { b'P' } else { b'p' });
    out.push(if exp < 0 { b'-' } else { b'+' });
    out.extend_from_slice(exp.unsigned_abs().to_string().as_bytes());
    if upper {
        out.make_ascii_uppercase();
    }
    out
}

/// A floating-point argument (`basic_writer::write(T value, format_specs)`).
pub(crate) fn format_double(out: &mut Vec<u8>, mut value: f64, specs: Option<&Specs>) -> Result<()> {
    let mut specs = specs.copied().unwrap_or_default();
    let (mut fspecs, locale) = parse_float_type_spec(&specs)?;
    fspecs.sign = specs.sign;
    if value.is_sign_negative() {
        fspecs.sign = Sign::Minus;
        value = -value;
    } else if fspecs.sign == Sign::Minus {
        fspecs.sign = Sign::None;
    }
    if !value.is_finite() {
        let text: &[u8] = match (value.is_infinite(), fspecs.upper) {
            (true, false) => b"inf",
            (true, true) => b"INF",
            (false, false) => b"nan",
            (false, true) => b"NAN",
        };
        let sign = fspecs.sign.byte();
        write_padded(out, &specs, 3 + usize::from(sign.is_some()), |out| {
            out.extend(sign);
            out.extend_from_slice(text);
        });
        return Ok(());
    }
    if specs.align == Align::None {
        specs.align = Align::Right;
    } else if specs.align == Align::Numeric {
        if let Some(sign) = fspecs.sign.byte() {
            out.push(sign);
            fspecs.sign = Sign::None;
            specs.width = specs.width.saturating_sub(1);
        }
        specs.align = Align::Right;
    }
    let mut buffer = Vec::new();
    if fspecs.format == FloatFormat::Hex {
        buffer.extend(fspecs.sign.byte());
        buffer.extend(hex_float(
            value,
            specs.precision,
            fspecs.trailing_zeros,
            fspecs.upper,
        ));
        write_padded(out, &specs, buffer.len(), |out| out.extend_from_slice(&buffer));
        return Ok(());
    }
    let mut precision = if specs.precision >= 0 || specs.ty == 0 {
        specs.precision
    } else {
        6
    };
    if fspecs.format == FloatFormat::Exp {
        precision += 1;
    }
    let (digits, exp) = float_digits(value, precision, fspecs.format == FloatFormat::Fixed);
    fspecs.precision = precision;
    let point = if !locale && fspecs.thousand_sep == b'.' {
        b','
    } else {
        b'.'
    };
    // `float_writer`: the general format switches to exponent notation outside
    // [1e-4, 1e(precision)).
    let full_exp = digits.len() as i32 + exp - 1;
    let general_precision = if fspecs.precision > 0 {
        fspecs.precision
    } else {
        16
    };
    if fspecs.format == FloatFormat::General && !(-4..general_precision).contains(&full_exp) {
        fspecs.format = FloatFormat::Exp;
    }
    buffer.extend(fspecs.sign.byte());
    prettify(&mut buffer, &digits, exp, &fspecs, point);
    write_padded(out, &specs, buffer.len(), |out| out.extend_from_slice(&buffer));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn double(value: f64, specs: Option<Specs>) -> String {
        let mut out = Vec::new();
        format_double(&mut out, value, specs.as_ref()).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn default_float_format_matches_fmt() {
        assert_eq!(double(1.0, None), "1.0");
        assert_eq!(double(0.1, None), "0.1");
        assert_eq!(double(1e20, None), "1e+20");
        assert_eq!(double(1.5e-5, None), "1.5e-05");
        assert_eq!(double(123_456.789, None), "123456.789");
    }

    #[test]
    fn hex_float_matches_c() {
        assert_eq!(
            String::from_utf8(hex_float(1.0, -1, false, false)).unwrap(),
            "0x1p+0"
        );
        assert_eq!(
            String::from_utf8(hex_float(3.0, -1, false, true)).unwrap(),
            "0X1.8P+1"
        );
        assert_eq!(
            String::from_utf8(hex_float(0.0, -1, false, false)).unwrap(),
            "0x0p+0"
        );
    }
}
