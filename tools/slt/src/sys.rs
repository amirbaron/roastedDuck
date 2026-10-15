//! Host queries used by `require ram` / `require disk_space` (`FileSystem::GetAvailableMemory`,
//! `FileSystem::GetAvailableDiskSpace` on POSIX).

use std::ffi::CString;

/// Physical memory in bytes (`_SC_PHYS_PAGES * _SC_PAGESIZE`), if the system reports it.
pub fn available_memory() -> Option<u64> {
    // SAFETY: `sysconf` has no preconditions.
    let pages = unsafe { libc::sysconf(libc::_SC_PHYS_PAGES) };
    // SAFETY: as above.
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    let pages = u64::try_from(pages).ok()?;
    let page_size = u64::try_from(page_size).ok()?;
    pages.checked_mul(page_size)
}

/// Bytes available for new data on the file system containing `path` (`f_frsize * f_bfree`).
pub fn available_disk_space(path: &str) -> Option<u64> {
    let path = CString::new(path).ok()?;
    // SAFETY: `statvfs` is plain old data; all-zero is a valid value to be overwritten.
    let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `path` is NUL-terminated and `stats` is a valid out-pointer.
    if unsafe { libc::statvfs(path.as_ptr(), &raw mut stats) } != 0 {
        return None;
    }
    #[allow(clippy::useless_conversion, reason = "field widths differ between platforms")]
    u64::from(stats.f_frsize).checked_mul(u64::from(stats.f_bfree))
}

/// `DBConfig::ParseMemoryLimit` (via `StringUtil::TryParseFormattedBytes`).
pub fn parse_memory_limit(text: &str) -> Result<u64, String> {
    if text.starts_with('-') || text == "null" || text == "none" {
        return Ok(u64::MAX);
    }
    let bytes = text.as_bytes();
    let mut index = bytes.iter().take_while(|&&b| crate::text::is_space(b)).count();
    let number_start = index;
    while index < bytes.len()
        && (bytes[index].is_ascii_digit() || matches!(bytes[index], b'.' | b'e' | b'E' | b'-'))
    {
        index += 1;
    }
    if index == number_start {
        return Err("Memory must have a number (e.g. 1GB)".to_owned());
    }
    let number = &text[number_start..index];
    let limit: f64 = number
        .parse()
        .map_err(|_| format!("Invalid memory limit: '{number}'"))?;
    while index < bytes.len() && crate::text::is_space(bytes[index]) {
        index += 1;
    }
    let unit_start = index;
    while index < bytes.len() && !crate::text::is_space(bytes[index]) {
        index += 1;
    }
    if limit < 0.0 {
        return Ok(u64::MAX);
    }
    let unit = text[unit_start..index].to_ascii_lowercase();
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
                "Unknown unit for memory: '{unit}' (expected: KB, MB, GB, TB for 1000^i units or KiB, MiB, GiB, \
                 TiB for 1024^i units)"
            ));
        }
    };
    Ok((multiplier as f64 * limit) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_limits() {
        assert_eq!(parse_memory_limit("16gb"), Ok(16_000_000_000));
        assert_eq!(parse_memory_limit("1.5 MiB"), Ok(1_572_864));
        assert_eq!(parse_memory_limit("none"), Ok(u64::MAX));
        assert!(parse_memory_limit("gb").is_err());
        assert!(parse_memory_limit("1 parsecs").is_err());
    }

    #[test]
    fn host_queries_succeed() {
        assert!(available_memory().is_some_and(|bytes| bytes > 0));
        assert!(available_disk_space(".").is_some());
    }
}
