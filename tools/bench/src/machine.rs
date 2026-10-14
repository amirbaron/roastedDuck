//! Describing the machine a run happens on.

use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::record::{MachineInfo, SCHEMA_VERSION};

/// Collects machine information; run settings are filled in by the caller.
pub fn collect(repo: &Path) -> MachineInfo {
    let started_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    MachineInfo {
        schema: SCHEMA_VERSION,
        started_unix,
        started_utc: iso8601_utc(started_unix),
        hostname: command_output("hostname", &[]).unwrap_or_default(),
        os: std::env::consts::OS.to_owned(),
        os_version: os_version().unwrap_or_default(),
        arch: std::env::consts::ARCH.to_owned(),
        cpu: cpu_name().unwrap_or_default(),
        logical_cores: std::thread::available_parallelism().map_or(1, |n| n.get() as u32),
        physical_cores: physical_cores(),
        memory_bytes: memory_bytes(),
        load_average: load_average(),
        git_commit: command_output("git", &["-C", &repo.to_string_lossy(), "rev-parse", "HEAD"]),
        ..MachineInfo::default()
    }
}

fn command_output(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!text.is_empty()).then_some(text)
}

fn sysctl(name: &str) -> Option<String> {
    command_output("sysctl", &["-n", name])
}

fn os_version() -> Option<String> {
    if cfg!(target_os = "macos") {
        command_output("sw_vers", &["-productVersion"])
    } else {
        command_output("uname", &["-r"])
    }
}

fn cpu_name() -> Option<String> {
    if cfg!(target_os = "macos") {
        return sysctl("machdep.cpu.brand_string");
    }
    let cpuinfo = std::fs::read_to_string("/proc/cpuinfo").ok()?;
    cpuinfo
        .lines()
        .find(|l| l.starts_with("model name"))
        .and_then(|l| l.split_once(':'))
        .map(|(_, name)| name.trim().to_owned())
}

fn physical_cores() -> Option<u32> {
    if cfg!(target_os = "macos") {
        return sysctl("hw.physicalcpu")?.parse().ok();
    }
    let cpuinfo = std::fs::read_to_string("/proc/cpuinfo").ok()?;
    let mut cores: Vec<(String, String)> = Vec::new();
    let (mut physical, mut core) = (String::new(), String::new());
    for line in cpuinfo.lines() {
        if let Some((key, value)) = line.split_once(':') {
            match key.trim() {
                "physical id" => value.trim().clone_into(&mut physical),
                "core id" => value.trim().clone_into(&mut core),
                _ => {}
            }
        } else if !core.is_empty() {
            cores.push((std::mem::take(&mut physical), std::mem::take(&mut core)));
        }
    }
    cores.sort();
    cores.dedup();
    u32::try_from(cores.len()).ok().filter(|&n| n > 0)
}

/// The current 1/5/15-minute load averages.
pub fn load_average() -> Option<[f64; 3]> {
    let mut loads = [0.0_f64; 3];
    // SAFETY: `loads` has room for the 3 values requested.
    let n = unsafe { libc::getloadavg(loads.as_mut_ptr(), 3) };
    (n == 3).then_some(loads)
}

fn memory_bytes() -> Option<u64> {
    if cfg!(target_os = "macos") {
        return sysctl("hw.memsize")?.parse().ok();
    }
    let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
    let kib: u64 = meminfo
        .lines()
        .find(|l| l.starts_with("MemTotal:"))?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()?;
    Some(kib * 1024)
}

/// Formats Unix seconds as `YYYY-MM-DDTHH:MM:SSZ`.
pub fn iso8601_utc(unix: u64) -> String {
    let (days, secs) = (unix / 86_400, unix % 86_400);
    let (year, month, day) = civil_from_days(days as i64);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        secs / 3600,
        secs / 60 % 60,
        secs % 60
    )
}

/// Howard Hinnant's `civil_from_days`.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_utc_timestamps() {
        assert_eq!(iso8601_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso8601_utc(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(iso8601_utc(1_792_015_424), "2026-10-14T22:03:44Z");
    }

    #[test]
    fn collects_basic_facts() {
        let info = collect(Path::new("."));
        assert!(info.logical_cores >= 1);
        assert_ne!(info.os, "");
    }
}
