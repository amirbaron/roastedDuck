//! The performance ratchet (`bench/baseline.json`): for every benchmark that already runs
//! correctly on rustdb, its median time and peak RSS may not regress beyond noise.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::record::{BenchmarkRecord, Results, Status};
use crate::report::{format_bytes, format_ms};

/// Allowed noise before a change counts as a regression.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Noise {
    /// Relative slack on the median time (0.10 = 10%).
    pub time_relative: f64,
    /// Absolute slack on the median time, for very short benchmarks.
    pub time_absolute_ms: f64,
    /// Relative slack on peak RSS.
    pub rss_relative: f64,
    /// Absolute slack on peak RSS.
    pub rss_absolute_bytes: u64,
}

impl Default for Noise {
    fn default() -> Self {
        Self {
            time_relative: 0.10,
            time_absolute_ms: 0.5,
            rss_relative: 0.05,
            rss_absolute_bytes: 4 * 1024 * 1024,
        }
    }
}

/// The recorded numbers of one benchmark.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub rust_median_ms: f64,
    pub rust_peak_rss_bytes: u64,
    /// The C++ reference numbers at the time of recording, for context.
    pub cpp_median_ms: Option<f64>,
    pub cpp_peak_rss_bytes: Option<u64>,
}

/// `bench/baseline.json`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Baseline {
    pub schema: u32,
    pub noise: Noise,
    /// Where the numbers were recorded (`hostname / cpu / threads`).
    pub machine: Option<String>,
    /// By benchmark key (name plus arguments).
    pub benchmarks: BTreeMap<String, Entry>,
}

impl Baseline {
    pub fn read(path: &Path) -> std::io::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        serde_json::from_str(&text).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }

    pub fn write(&self, path: &Path) -> std::io::Result<()> {
        let mut json = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        json.push('\n');
        std::fs::write(path, json)
    }
}

/// Findings of [`check`].
#[derive(Debug, Default, PartialEq)]
pub struct Check {
    /// Benchmarks in the baseline that got slower or bigger beyond noise.
    pub regressions: Vec<String>,
    /// Benchmarks in the baseline that no longer run correctly (or were not run).
    pub lost: Vec<String>,
    /// Benchmarks that improved beyond noise (run `ratchet update` to lock them in).
    pub improvements: Vec<String>,
    /// Benchmarks that run correctly but are not in the baseline yet.
    pub new: Vec<String>,
}

impl Check {
    pub fn passed(&self) -> bool {
        self.regressions.is_empty() && self.lost.is_empty()
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        for (title, items) in [
            ("Regressions", &self.regressions),
            ("No longer passing", &self.lost),
            ("Improvements (run `bench ratchet update`)", &self.improvements),
            (
                "Newly passing, not in the baseline (run `bench ratchet update`)",
                &self.new,
            ),
        ] {
            if items.is_empty() {
                continue;
            }
            let _ = writeln!(out, "{title} ({}):", items.len());
            for item in items {
                let _ = writeln!(out, "  {item}");
            }
        }
        let _ = writeln!(out, "ratchet: {}", if self.passed() { "ok" } else { "FAILED" });
        out
    }
}

fn ok_records(results: &Results) -> BTreeMap<String, &BenchmarkRecord> {
    results
        .benchmarks
        .iter()
        .filter(|r| r.status == Status::Ok && r.median_ms.is_some() && r.peak_rss_bytes.is_some())
        .map(|r| (r.key(), r))
        .collect()
}

/// Compares a rustdb run against the baseline. With `allow_missing`, baseline
/// benchmarks absent from the run (a partial run) are not failures.
pub fn check(baseline: &Baseline, rust: &Results, allow_missing: bool) -> Check {
    let noise = baseline.noise;
    let ok = ok_records(rust);
    let ran: BTreeMap<String, &BenchmarkRecord> = rust.benchmarks.iter().map(|r| (r.key(), r)).collect();
    let mut result = Check::default();
    for (key, entry) in &baseline.benchmarks {
        let Some(record) = ok.get(key) else {
            match ran.get(key) {
                Some(record) => result.lost.push(format!("{key}: now {}", record.status)),
                None if !allow_missing => result.lost.push(format!("{key}: not run")),
                None => {}
            }
            continue;
        };
        let (Some(median), Some(rss)) = (record.median_ms, record.peak_rss_bytes) else {
            continue;
        };
        let time_limit = entry.rust_median_ms * (1.0 + noise.time_relative) + noise.time_absolute_ms;
        let rss_limit =
            entry.rust_peak_rss_bytes as f64 * (1.0 + noise.rss_relative) + noise.rss_absolute_bytes as f64;
        if median > time_limit {
            result.regressions.push(format!(
                "{key}: median {} > baseline {} (+{:.0}%)",
                format_ms(median),
                format_ms(entry.rust_median_ms),
                (median / entry.rust_median_ms - 1.0) * 100.0
            ));
        } else if median * (1.0 + noise.time_relative) + noise.time_absolute_ms < entry.rust_median_ms {
            result.improvements.push(format!(
                "{key}: median {} < baseline {}",
                format_ms(median),
                format_ms(entry.rust_median_ms)
            ));
        }
        if rss as f64 > rss_limit {
            result.regressions.push(format!(
                "{key}: peak RSS {} > baseline {}",
                format_bytes(rss),
                format_bytes(entry.rust_peak_rss_bytes)
            ));
        }
    }
    result.new = ok
        .keys()
        .filter(|key| !baseline.benchmarks.contains_key(*key))
        .cloned()
        .collect();
    result
}

/// Records the current rustdb numbers (with C++ numbers for context). Refuses to drop
/// benchmarks that no longer pass unless `force`.
pub fn update(
    baseline: Option<Baseline>,
    rust: &Results,
    cpp: Option<&Results>,
    force: bool,
) -> Result<Baseline, String> {
    let mut baseline = baseline.unwrap_or_default();
    baseline.schema = 1;
    let ok = ok_records(rust);
    let cpp_ok = cpp.map(ok_records).unwrap_or_default();
    let lost: Vec<&String> = baseline
        .benchmarks
        .keys()
        .filter(|key| !ok.contains_key(*key))
        .collect();
    if !lost.is_empty() && !force {
        return Err(format!(
            "{} baseline benchmark(s) no longer pass (e.g. {}); fix them or pass --force",
            lost.len(),
            lost[0]
        ));
    }
    baseline.benchmarks = ok
        .iter()
        .filter_map(|(key, record)| {
            let cpp = cpp_ok.get(key);
            Some((
                key.clone(),
                Entry {
                    rust_median_ms: record.median_ms?,
                    rust_peak_rss_bytes: record.peak_rss_bytes?,
                    cpp_median_ms: cpp.and_then(|c| c.median_ms),
                    cpp_peak_rss_bytes: cpp.and_then(|c| c.peak_rss_bytes),
                },
            ))
        })
        .collect();
    if let Some(machine) = rust.machine.last() {
        baseline.machine = Some(format!(
            "{} / {} / threads={}",
            machine.hostname, machine.cpu, machine.threads
        ));
    }
    Ok(baseline)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn results(records: &[(&str, Status, f64, u64)]) -> Results {
        Results {
            machine: vec![],
            benchmarks: records
                .iter()
                .map(|&(name, status, median, rss)| BenchmarkRecord {
                    name: name.into(),
                    status,
                    median_ms: Some(median),
                    peak_rss_bytes: Some(rss),
                    ..BenchmarkRecord::default()
                })
                .collect(),
        }
    }

    const MIB: u64 = 1024 * 1024;

    #[test]
    fn update_then_check() {
        let first = results(&[
            ("a", Status::Ok, 100.0, 100 * MIB),
            ("b", Status::Unsupported, 0.0, MIB),
        ]);
        let baseline = update(None, &first, None, false).unwrap();
        assert_eq!(baseline.benchmarks.len(), 1);
        assert!(check(&baseline, &first, false).passed());

        let noisy = results(&[("a", Status::Ok, 105.0, 102 * MIB)]);
        assert!(check(&baseline, &noisy, false).passed());

        let slower = results(&[("a", Status::Ok, 150.0, 100 * MIB), ("b", Status::Ok, 1.0, MIB)]);
        let found = check(&baseline, &slower, false);
        assert_eq!(found.regressions.len(), 1);
        assert_eq!(found.new, vec!["b"]);
        assert!(!found.passed());

        let bigger = results(&[("a", Status::Ok, 100.0, 200 * MIB)]);
        assert_eq!(check(&baseline, &bigger, false).regressions.len(), 1);

        let broken = results(&[("a", Status::Error, 0.0, 0)]);
        let found = check(&baseline, &broken, false);
        assert_eq!(found.lost, vec!["a: now error"]);
        assert!(update(Some(baseline.clone()), &broken, None, false).is_err());
        assert!(
            update(Some(baseline.clone()), &broken, None, true)
                .unwrap()
                .benchmarks
                .is_empty()
        );

        let partial = results(&[]);
        assert!(!check(&baseline, &partial, false).passed());
        assert!(check(&baseline, &partial, true).passed());

        let faster = results(&[("a", Status::Ok, 50.0, 100 * MIB)]);
        assert_eq!(check(&baseline, &faster, false).improvements.len(), 1);
    }
}
