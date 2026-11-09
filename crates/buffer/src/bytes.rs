//! Byte-size formatting and parsing (DuckDB's `StringUtil::BytesToHumanReadableString`,
//! `StringUtil::TryParseFormattedBytes` and `DBConfig::ParseMemoryLimit`).

use roastedduck_common::{Error, Result};

/// Formats `bytes` with binary units, e.g. `256.0 KiB` (DuckDB's
/// `BytesToHumanReadableString(bytes, 1024)`). Used in settings and error messages.
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["bytes", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut parts = [0u64; 6];
    parts[0] = bytes;
    for i in 1..6 {
        parts[i] = parts[i - 1] / 1024;
        parts[i - 1] %= 1024;
    }
    for i in (1..6).rev() {
        if parts[i] != 0 {
            // Maps 0 to 0 and 1023 to 9.
            let fraction = parts[i - 1] * 10 / 1024;
            return format!("{}.{} {}", parts[i], fraction, UNITS[i]);
        }
    }
    if bytes == 1 {
        "1 byte".to_owned()
    } else {
        format!("{bytes} bytes")
    }
}

/// Parses a size such as `1GB`, `512 MiB` or `1000 bytes`. Errors are returned as DuckDB's
/// message text (`TryParseFormattedBytes`).
pub fn try_parse_formatted_bytes(arg: &str) -> std::result::Result<u64, String> {
    let bytes = arg.as_bytes();
    let mut idx = 0;
    while idx < bytes.len() && bytes[idx].is_ascii_whitespace() {
        idx += 1;
    }
    let num_start = idx;
    while idx < bytes.len()
        && (bytes[idx].is_ascii_digit() || matches!(bytes[idx], b'.' | b'e' | b'E' | b'-'))
    {
        idx += 1;
    }
    if idx == num_start {
        return Err("Memory must have a number (e.g. 1GB)".to_owned());
    }
    let number = &arg[num_start..idx];
    let Ok(limit) = number.parse::<f64>() else {
        return Err(format!("Invalid memory limit: '{number}'"));
    };
    while idx < bytes.len() && bytes[idx].is_ascii_whitespace() {
        idx += 1;
    }
    let unit_start = idx;
    while idx < bytes.len() && !bytes[idx].is_ascii_whitespace() {
        idx += 1;
    }
    if limit < 0.0 {
        return Err("Memory cannot be negative".to_owned());
    }
    let unit = arg[unit_start..idx].to_lowercase();
    let multiplier: u64 = match unit.as_str() {
        "byte" | "bytes" | "b" => 1,
        "kilobyte" | "kilobytes" | "kb" | "k" => 1000,
        "megabyte" | "megabytes" | "mb" | "m" => 1000 * 1000,
        "gigabyte" | "gigabytes" | "gb" | "g" => 1000 * 1000 * 1000,
        "terabyte" | "terabytes" | "tb" | "t" => 1000 * 1000 * 1000 * 1000,
        "kib" => 1024,
        "mib" => 1024 * 1024,
        "gib" => 1024 * 1024 * 1024,
        "tib" => 1024 * 1024 * 1024 * 1024,
        _ => {
            return Err(format!(
                "Unknown unit for memory: '{unit}' (expected: KB, MB, GB, TB for 1000^i units or KiB, MiB, GiB, TiB \
                 for 1024^i units)"
            ));
        }
    };
    let max_value = u64::MAX as f64;
    if limit > max_value / multiplier as f64 {
        return Err("Memory value out of range: value is too large".to_owned());
    }
    Ok((multiplier as f64 * limit) as u64)
}

/// Parses a memory limit setting (`DBConfig::ParseMemoryLimit`).
///
/// A leading `-`, `null` or `none` means "unlimited" (`u64::MAX`); a negative number also
/// yields `u64::MAX`. Malformed input is a Parser Error, like in DuckDB.
pub fn parse_memory_limit(arg: &str) -> Result<u64> {
    if arg.starts_with('-') || arg == "null" || arg == "none" {
        return Ok(u64::MAX);
    }
    match try_parse_formatted_bytes(arg) {
        Ok(bytes) => Ok(bytes),
        Err(message) if message == "Memory cannot be negative" => Ok(u64::MAX),
        Err(message) => Err(Error::parser(message)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_like_duckdb() {
        assert_eq!(format_bytes(0), "0 bytes");
        assert_eq!(format_bytes(1), "1 byte");
        assert_eq!(format_bytes(1023), "1023 bytes");
        assert_eq!(format_bytes(262_144), "256.0 KiB");
        assert_eq!(format_bytes(2_000_000), "1.9 MiB");
        assert_eq!(format_bytes(41_231_686_041), "38.3 GiB");
        assert_eq!(format_bytes(u64::MAX), "16383.9 PiB");
    }

    #[test]
    fn parses_like_duckdb() {
        assert_eq!(try_parse_formatted_bytes("1GB"), Ok(1_000_000_000));
        assert_eq!(try_parse_formatted_bytes(" 512 MiB"), Ok(512 * 1024 * 1024));
        assert_eq!(try_parse_formatted_bytes("1.5kb"), Ok(1500));
        assert_eq!(try_parse_formatted_bytes("10 bytes"), Ok(10));
        assert_eq!(
            try_parse_formatted_bytes("GB").unwrap_err(),
            "Memory must have a number (e.g. 1GB)"
        );
        assert_eq!(
            parse_memory_limit("1foo").unwrap_err().to_string(),
            "Parser Error: Unknown unit for memory: 'foo' (expected: KB, MB, GB, TB for 1000^i units or KiB, MiB, \
             GiB, TiB for 1024^i units)"
        );
        assert_eq!(parse_memory_limit("-1").unwrap(), u64::MAX);
        assert_eq!(parse_memory_limit("none").unwrap(), u64::MAX);
        assert_eq!(parse_memory_limit(" -5GB").unwrap(), u64::MAX);
        assert_eq!(
            parse_memory_limit("1-2GB").unwrap_err().to_string(),
            "Parser Error: Invalid memory limit: '1-2'"
        );
    }
}
