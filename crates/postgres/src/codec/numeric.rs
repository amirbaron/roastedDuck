use super::{Reader, invalid};
use roastedduck_common::Result;
use roastedduck_types::{LogicalType, Value};

pub(super) fn decode(reader: &mut Reader<'_>, ty: &LogicalType, nan_as_null: bool) -> Result<Value> {
    let count = usize::from(reader.u16()?);
    let weight = i32::from(reader.i16()?);
    let sign = reader.u16()?;
    let scale = u32::from(reader.u16()?);
    if matches!(sign, 0xc000 | 0xd000 | 0xf000) {
        if ty.decimal_properties().is_some() {
            if nan_as_null {
                return Ok(Value::null(ty.clone()));
            }
            let name = match sign {
                0xc000 => "NaN",
                0xd000 => "Infinity",
                _ => "-Infinity",
            };
            return Err(invalid(format!("Unsupported NUMERIC value: {name}")));
        }
        return Ok(Value::double(match sign {
            0xc000 => f64::NAN,
            0xd000 => f64::INFINITY,
            _ => f64::NEG_INFINITY,
        }));
    }
    if sign != 0 && sign != 0x4000 {
        return Err(invalid("Postgres numeric NA/Inf"));
    }
    if count > reader.0.len() / 2 {
        return Err(invalid("Truncated PostgreSQL numeric"));
    }
    let digits = (0..count)
        .map(|_| {
            let digit = reader.u16()?;
            if digit >= 10_000 {
                return Err(invalid("Invalid PostgreSQL numeric digit"));
            }
            Ok(digit)
        })
        .collect::<Result<Vec<_>>>()?;
    if let Some((width, target_scale)) = ty.decimal_properties() {
        let mut result = 0i128;
        for (i, digit) in digits.into_iter().enumerate() {
            if digit == 0 {
                continue;
            }
            let exponent = (weight - i as i32) * 4 + i32::from(target_scale);
            let term = if exponent >= 0 {
                10i128
                    .checked_pow(exponent as u32)
                    .and_then(|power| i128::from(digit).checked_mul(power))
                    .ok_or_else(|| invalid("PostgreSQL numeric overflow"))?
            } else {
                10i128
                    .checked_pow((-exponent) as u32)
                    .map_or(0, |power| i128::from(digit) / power)
            };
            result = result
                .checked_add(term)
                .ok_or_else(|| invalid("PostgreSQL numeric overflow"))?;
        }
        if result >= 10i128.pow(u32::from(width)) {
            return Err(invalid("PostgreSQL numeric exceeds declared precision"));
        }
        return Ok(Value::decimal(
            if sign == 0x4000 { -result } else { result },
            width,
            target_scale,
        ));
    }
    // Preserve the pinned extension's evaluation order (not a decimal-string parse),
    // including its floating-point rounding for unconstrained NUMERIC.
    if count == 0 {
        return Ok(Value::double(0.0));
    }
    let power = 10.0f64.powf(f64::from(scale));
    let mut integral = 0.0;
    if weight >= 0 {
        integral = f64::from(digits[0]);
        for i in 1..=weight as usize {
            integral *= 10_000.0;
            if let Some(digit) = digits.get(i) {
                integral += f64::from(*digit);
            }
        }
        integral *= power;
    }
    let mut fraction = 0.0;
    if count as i32 > weight + 1 {
        let correction = (count as i32 - weight - 1) * 4 - scale as i32;
        for (i, digit) in digits.iter().enumerate().skip((weight + 1).max(0) as usize) {
            if i + 1 < count {
                fraction = fraction * 10_000.0 + f64::from(*digit);
            } else {
                let compensation = 10.0f64.powf(f64::from(correction.unsigned_abs()));
                let (base, digit) = if correction >= 0 {
                    (10_000.0 / compensation, f64::from(*digit) / compensation)
                } else {
                    (10_000.0 * compensation, f64::from(*digit) * compensation)
                };
                fraction = fraction * base + digit;
            }
        }
    }
    let result = (integral + fraction) / power;
    Ok(Value::double(if sign == 0x4000 { -result } else { result }))
}

pub(super) fn encode(value: i128, scale: u8) -> Vec<u8> {
    // Pad to base-10000 groups without overflowing an i128 at DECIMAL(38,38).
    let mut decimal = value.unsigned_abs().to_string();
    let scale = usize::from(scale);
    if decimal.len() <= scale {
        decimal.insert_str(0, &"0".repeat(scale + 1 - decimal.len()));
    }
    let integral_digits = decimal.len() - scale;
    let left_pad = (4 - integral_digits % 4) % 4;
    decimal.insert_str(0, &"0".repeat(left_pad));
    decimal.push_str(&"0".repeat((4 - scale % 4) % 4));
    let mut weight = ((integral_digits + left_pad) / 4) as i16 - 1;
    let mut digits: Vec<u16> = decimal
        .as_bytes()
        .as_chunks::<4>()
        .0
        .iter()
        .map(|group| group.iter().fold(0, |n, b| n * 10 + u16::from(b - b'0')))
        .collect();
    let first = digits.iter().position(|d| *d != 0).unwrap_or(digits.len());
    weight -= first as i16;
    digits.drain(..first);
    while digits.last() == Some(&0) {
        digits.pop();
    }
    if digits.is_empty() {
        weight = 0;
    }
    let mut bytes = Vec::with_capacity(8 + digits.len() * 2);
    bytes.extend_from_slice(&(digits.len() as u16).to_be_bytes());
    bytes.extend_from_slice(&weight.to_be_bytes());
    bytes.extend_from_slice(&(if value < 0 { 0x4000u16 } else { 0 }).to_be_bytes());
    bytes.extend_from_slice(&(scale as u16).to_be_bytes());
    for digit in digits {
        bytes.extend_from_slice(&digit.to_be_bytes());
    }
    bytes
}
