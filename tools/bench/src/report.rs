//! `bench compare`: a markdown report of rustdb against C++ DuckDB, with per-benchmark
//! speedups and memory ratios, per-suite geometric means and the plan's final gates.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use crate::measure::geomean;
use crate::record::{BenchmarkRecord, MachineInfo, Results, Status};

/// The plan's speed target: geometric-mean speedup on each of these suites.
pub const SPEED_GEOMEAN_TARGET: f64 = 1.3;
/// No benchmark may be slower than this speedup.
pub const SPEED_FLOOR: f64 = 0.99;
/// Geometric-mean peak memory ratio (Rust / C++) across all suites.
pub const MEMORY_GEOMEAN_TARGET: f64 = 0.6;
/// No benchmark may use more peak memory than C++.
pub const MEMORY_CEILING: f64 = 1.0;

/// A suite the speed gate is evaluated on.
struct GateSuite {
    label: &'static str,
    selects: fn(&BenchmarkRecord) -> bool,
}

const SPEED_GATE_SUITES: [GateSuite; 3] = [
    GateSuite {
        label: "TPC-H SF10",
        selects: |r| {
            r.name.starts_with("benchmark/tpch/sf1/") && r.arguments.get("sf").is_some_and(|sf| sf == "10")
        },
    },
    GateSuite {
        label: "ClickBench",
        selects: |r| r.suite == "clickbench",
    },
    GateSuite {
        label: "h2oai",
        selects: |r| r.suite == "h2oai",
    },
];

/// One benchmark measured on both engines.
#[derive(Debug)]
struct Pair<'a> {
    cpp: &'a BenchmarkRecord,
    rust: Option<&'a BenchmarkRecord>,
}

impl Pair<'_> {
    /// Both engines produced correct, cross-checked results with timings.
    fn comparable(&self) -> bool {
        let Some(rust) = self.rust else { return false };
        self.cpp.status == Status::Ok
            && rust.status == Status::Ok
            && self.cpp.median_ms.is_some()
            && rust.median_ms.is_some()
            && !self.results_differ()
    }

    /// Both results were fingerprinted and the fingerprints differ.
    fn results_differ(&self) -> bool {
        match (
            self.rust.and_then(|r| r.result_fingerprint.as_ref()),
            &self.cpp.result_fingerprint,
        ) {
            (Some(rust), Some(cpp)) => rust != cpp,
            _ => false,
        }
    }

    /// C++ time / Rust time.
    fn speedup(&self) -> Option<f64> {
        if !self.comparable() {
            return None;
        }
        Some(self.cpp.median_ms? / self.rust?.median_ms?)
    }

    /// Rust peak RSS / C++ peak RSS.
    fn memory_ratio(&self) -> Option<f64> {
        if !self.comparable() {
            return None;
        }
        Some(self.rust?.peak_rss_bytes? as f64 / self.cpp.peak_rss_bytes? as f64)
    }
}

/// Renders the comparison report.
pub fn compare(cpp: &Results, rust: &Results, cpp_label: &str, rust_label: &str) -> String {
    let rust_by_key: BTreeMap<String, &BenchmarkRecord> =
        rust.benchmarks.iter().map(|r| (r.key(), r)).collect();
    let pairs: Vec<Pair<'_>> = cpp
        .benchmarks
        .iter()
        .map(|cpp| Pair {
            cpp,
            rust: rust_by_key.get(&cpp.key()).copied(),
        })
        .collect();
    let mut out = String::new();
    out.push_str("# rustdb vs C++ DuckDB: benchmark comparison\n\n");
    write_runs(&mut out, cpp, rust, cpp_label, rust_label);
    write_gates(&mut out, &pairs);
    write_suites(&mut out, &pairs);
    write_cross_checks(&mut out, &pairs);
    write_benchmarks(&mut out, &pairs);
    write_cpp_failures(&mut out, &pairs);
    out
}

fn describe_machine(m: &MachineInfo) -> String {
    format!(
        "{} on {} ({} {}, {} {} cores, {}), threads={}, memory_limit={}, warm-up={}, runs={}, timeout={}, \
         started {}, load average {}",
        m.engine_version,
        m.hostname,
        m.os,
        m.os_version,
        m.cpu,
        m.logical_cores,
        m.memory_bytes.map_or_else(|| "?".to_owned(), format_bytes),
        m.threads,
        m.memory_limit.as_deref().unwrap_or("default"),
        m.warmup,
        m.runs,
        m.timeout_secs
            .map_or_else(|| "none".to_owned(), |t| format!("{t}s")),
        m.started_utc,
        m.load_average.map_or_else(
            || "?".to_owned(),
            |[one, five, _]| format!("{one:.1} (1 min) / {five:.1} (5 min)")
        ),
    )
}

fn write_runs(out: &mut String, cpp: &Results, rust: &Results, cpp_label: &str, rust_label: &str) {
    out.push_str("## Runs\n\n");
    for (label, results) in [(cpp_label, cpp), (rust_label, rust)] {
        let machines: Vec<String> = results.machine.iter().map(describe_machine).collect();
        let _ = writeln!(
            out,
            "- `{label}`: {} benchmark(s); {}",
            results.benchmarks.len(),
            if machines.is_empty() {
                "no machine header".to_owned()
            } else {
                machines.join("; ")
            }
        );
    }
    let threads = |r: &Results| r.machine.iter().map(|m| m.threads).collect::<Vec<_>>();
    let hosts = |r: &Results| r.machine.iter().map(|m| m.hostname.clone()).collect::<Vec<_>>();
    if !rust.machine.is_empty() && (threads(cpp) != threads(rust) || hosts(cpp) != hosts(rust)) {
        out.push_str(
            "\n> **Warning:** the two runs used different machines or thread counts; ratios are not meaningful.\n",
        );
    }
    out.push('\n');
}

fn verdict(value: Option<f64>, pass: impl Fn(f64) -> bool) -> &'static str {
    match value {
        None => "not evaluated (no comparable benchmarks)",
        Some(v) if pass(v) => "pass",
        Some(_) => "**FAIL**",
    }
}

fn ratio(value: Option<f64>) -> String {
    value.map_or_else(|| "–".to_owned(), |v| format!("{v:.2}×"))
}

fn write_gates(out: &mut String, pairs: &[Pair<'_>]) {
    out.push_str("## Final targets (plan, M9)\n\n");
    out.push_str(
        "Only benchmarks that are correct on both engines (and whose result fingerprints agree) count.\n\n",
    );
    out.push_str("| Gate | Target | Value | Benchmarks | Verdict |\n|---|---|---:|---:|---|\n");
    for suite in &SPEED_GATE_SUITES {
        let speedups: Vec<f64> = pairs
            .iter()
            .filter(|p| (suite.selects)(p.cpp))
            .filter_map(Pair::speedup)
            .collect();
        let value = geomean(speedups.iter().copied());
        let _ = writeln!(
            out,
            "| Speed geomean, {} | ≥ {SPEED_GEOMEAN_TARGET:.2}× | {} | {} | {} |",
            suite.label,
            ratio(value),
            speedups.len(),
            verdict(value, |v| v >= SPEED_GEOMEAN_TARGET)
        );
    }
    let speedups: Vec<(f64, &str)> = pairs
        .iter()
        .filter_map(|p| Some((p.speedup()?, p.cpp.name.as_str())))
        .collect();
    let slowest = speedups.iter().copied().min_by(|a, b| a.0.total_cmp(&b.0));
    let below = speedups.iter().filter(|(s, _)| *s < SPEED_FLOOR).count();
    let _ = writeln!(
        out,
        "| Slowest benchmark (all suites) | ≥ {SPEED_FLOOR:.2}× | {}{} | {} | {} |",
        ratio(slowest.map(|s| s.0)),
        slowest.map_or_else(String::new, |s| format!(" (`{}`)", s.1)),
        speedups.len(),
        if below > 0 {
            format!("**FAIL** ({below} below)")
        } else {
            verdict(slowest.map(|s| s.0), |v| v >= SPEED_FLOOR).to_owned()
        }
    );
    let memory: Vec<(f64, &str)> = pairs
        .iter()
        .filter_map(|p| Some((p.memory_ratio()?, p.cpp.name.as_str())))
        .collect();
    let memory_geomean = geomean(memory.iter().map(|m| m.0));
    let _ = writeln!(
        out,
        "| Peak memory geomean (all suites) | ≤ {MEMORY_GEOMEAN_TARGET:.2}× | {} | {} | {} |",
        ratio(memory_geomean),
        memory.len(),
        verdict(memory_geomean, |v| v <= MEMORY_GEOMEAN_TARGET)
    );
    let largest = memory.iter().copied().max_by(|a, b| a.0.total_cmp(&b.0));
    let above = memory.iter().filter(|(m, _)| *m > MEMORY_CEILING).count();
    let _ = writeln!(
        out,
        "| Largest peak memory ratio | ≤ {MEMORY_CEILING:.2}× | {}{} | {} | {} |\n",
        ratio(largest.map(|m| m.0)),
        largest.map_or_else(String::new, |m| format!(" (`{}`)", m.1)),
        memory.len(),
        if above > 0 {
            format!("**FAIL** ({above} above)")
        } else {
            verdict(largest.map(|m| m.0), |v| v <= MEMORY_CEILING).to_owned()
        }
    );
}

/// Per-suite label: the suite plus non-default arguments (e.g. `tpch [sf=10]`).
fn suite_label(record: &BenchmarkRecord) -> String {
    let arguments: Vec<String> = record
        .arguments
        .iter()
        .filter(|(k, v)| !(k.as_str() == "sf" && v.as_str() == "1"))
        .map(|(k, v)| format!("{k}={v}"))
        .collect();
    if arguments.is_empty() {
        record.suite.clone()
    } else {
        format!("{} [{}]", record.suite, arguments.join(","))
    }
}

fn write_suites(out: &mut String, pairs: &[Pair<'_>]) {
    let mut suites: BTreeMap<String, Vec<&Pair<'_>>> = BTreeMap::new();
    for pair in pairs {
        suites.entry(suite_label(pair.cpp)).or_default().push(pair);
    }
    out.push_str("## Suites\n\n");
    out.push_str(
        "| Suite | Benchmarks | C++ ok | Rust ok | Rust unsupported | Rust other | Compared | Speedup geomean | \
         Memory ratio geomean | C++ median geomean | C++ peak RSS geomean |\n\
         |---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|\n",
    );
    for (suite, pairs) in &suites {
        let rust_count = |s: Status| {
            pairs
                .iter()
                .filter(|p| p.rust.is_some_and(|r| r.status == s))
                .count()
        };
        let cpp_ok: Vec<&BenchmarkRecord> = pairs
            .iter()
            .map(|p| p.cpp)
            .filter(|c| c.status == Status::Ok)
            .collect();
        let rust_ok = rust_count(Status::Ok);
        let unsupported = rust_count(Status::Unsupported);
        let _ = writeln!(
            out,
            "| {suite} | {} | {} | {rust_ok} | {unsupported} | {} | {} | {} | {} | {} | {} |",
            pairs.len(),
            cpp_ok.len(),
            pairs.len() - rust_ok - unsupported,
            pairs.iter().filter(|p| p.comparable()).count(),
            ratio(geomean(pairs.iter().filter_map(|p| p.speedup()))),
            ratio(geomean(pairs.iter().filter_map(|p| p.memory_ratio()))),
            geomean(cpp_ok.iter().filter_map(|c| c.median_ms)).map_or_else(|| "–".to_owned(), format_ms),
            geomean(cpp_ok.iter().filter_map(|c| c.peak_rss_bytes.map(|b| b as f64)))
                .map_or_else(|| "–".to_owned(), |b| format_bytes(b as u64)),
        );
    }
    out.push_str("\n\"Rust other\" counts benchmarks that were not run on rustdb or ended in error, timeout, crash or skip.\n\n");
}

fn write_cross_checks(out: &mut String, pairs: &[Pair<'_>]) {
    let differing: Vec<&Pair<'_>> = pairs.iter().filter(|p| p.results_differ()).collect();
    out.push_str("## Result cross-checks\n\n");
    let checked = pairs
        .iter()
        .filter(|p| {
            p.rust.is_some_and(|r| r.result_fingerprint.is_some()) && p.cpp.result_fingerprint.is_some()
        })
        .count();
    let _ = writeln!(
        out,
        "{checked} benchmark(s) have result fingerprints on both engines; {} differ.\n",
        differing.len()
    );
    for pair in differing {
        let _ = writeln!(out, "- `{}`: results differ from C++", pair.cpp.key());
    }
    if checked > 0 {
        out.push('\n');
    }
}

fn status_cell(record: Option<&BenchmarkRecord>) -> String {
    match record {
        None => "not run".to_owned(),
        Some(r) => r.status.to_string(),
    }
}

fn write_benchmarks(out: &mut String, pairs: &[Pair<'_>]) {
    let mut suites: BTreeMap<String, Vec<&Pair<'_>>> = BTreeMap::new();
    for pair in pairs {
        suites.entry(suite_label(pair.cpp)).or_default().push(pair);
    }
    out.push_str("## Benchmarks\n\n");
    for (suite, pairs) in suites {
        let _ = writeln!(
            out,
            "<details><summary>{suite} ({} benchmarks)</summary>\n\n\
             | Benchmark | C++ status | C++ median | C++ min | C++ peak RSS | Rust status | Rust median | \
             Rust peak RSS | Speedup | Memory ratio |\n|---|---|---:|---:|---:|---|---:|---:|---:|---:|",
            pairs.len()
        );
        for pair in pairs {
            let (cpp, rust) = (pair.cpp, pair.rust);
            let _ = writeln!(
                out,
                "| `{}` | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
                cpp.key().trim_start_matches("benchmark/"),
                cpp.status,
                cpp.median_ms.map_or_else(|| "–".to_owned(), format_ms),
                cpp.min_ms.map_or_else(|| "–".to_owned(), format_ms),
                cpp.peak_rss_bytes.map_or_else(|| "–".to_owned(), format_bytes),
                status_cell(rust),
                rust.and_then(|r| r.median_ms)
                    .map_or_else(|| "–".to_owned(), format_ms),
                rust.and_then(|r| r.peak_rss_bytes)
                    .map_or_else(|| "–".to_owned(), format_bytes),
                ratio(pair.speedup()),
                ratio(pair.memory_ratio()),
            );
        }
        out.push_str("\n</details>\n\n");
    }
}

fn write_cpp_failures(out: &mut String, pairs: &[Pair<'_>]) {
    let failures: Vec<&BenchmarkRecord> = pairs
        .iter()
        .map(|p| p.cpp)
        .filter(|c| c.status != Status::Ok)
        .collect();
    if failures.is_empty() {
        return;
    }
    out.push_str("## C++ benchmarks without a baseline\n\n| Benchmark | Status | Reason |\n|---|---|---|\n");
    for record in failures {
        let reason = record.error.as_deref().unwrap_or("").lines().next().unwrap_or("");
        let reason: String = reason.chars().take(160).collect();
        let _ = writeln!(
            out,
            "| `{}` | {} | {} |",
            record.key().trim_start_matches("benchmark/"),
            record.status,
            reason.replace('|', "\\|")
        );
    }
    out.push('\n');
}

/// `12.3 ms`, `1.23 s`.
pub fn format_ms(ms: f64) -> String {
    if ms >= 1000.0 {
        format!("{:.2} s", ms / 1000.0)
    } else if ms >= 10.0 {
        format!("{ms:.1} ms")
    } else {
        format!("{ms:.2} ms")
    }
}

/// `412.3 MiB`, `1.20 GiB`.
pub fn format_bytes(bytes: u64) -> String {
    const MIB: f64 = 1024.0 * 1024.0;
    let mib = bytes as f64 / MIB;
    if mib >= 1024.0 {
        format!("{:.2} GiB", mib / 1024.0)
    } else {
        format!("{mib:.1} MiB")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(engine: &str, name: &str, status: Status, median: f64, rss: u64) -> BenchmarkRecord {
        BenchmarkRecord {
            engine: engine.into(),
            name: name.into(),
            suite: crate::format::suite_of(name).into(),
            status,
            median_ms: (status == Status::Ok).then_some(median),
            peak_rss_bytes: Some(rss),
            ..BenchmarkRecord::default()
        }
    }

    #[test]
    fn computes_ratios_and_gates() {
        let mut cpp = Results::default();
        let mut rust = Results::default();
        for (name, cpp_ms, rust_ms) in [
            ("benchmark/h2oai/q01.benchmark", 100.0, 50.0),
            ("benchmark/h2oai/q02.benchmark", 100.0, 200.0),
        ] {
            cpp.benchmarks.push(record("cpp", name, Status::Ok, cpp_ms, 1000));
            rust.benchmarks
                .push(record("rust", name, Status::Ok, rust_ms, 500));
        }
        cpp.benchmarks
            .push(record("cpp", "benchmark/micro/a.benchmark", Status::Ok, 1.0, 10));
        rust.benchmarks.push(record(
            "rust",
            "benchmark/micro/a.benchmark",
            Status::Unsupported,
            0.0,
            10,
        ));

        let report = compare(&cpp, &rust, "cpp.jsonl", "rust.jsonl");
        // geomean(2.0, 0.5) = 1.0 < 1.3; slowest 0.5 < 0.99; memory 0.5 <= 0.6.
        assert!(
            report.contains("| Speed geomean, h2oai | ≥ 1.30× | 1.00× | 2 | **FAIL** |"),
            "{report}"
        );
        assert!(
            report.contains("| Speed geomean, ClickBench | ≥ 1.30× | – | 0 | not evaluated"),
            "{report}"
        );
        assert!(report.contains("**FAIL** (1 below)"), "{report}");
        assert!(
            report.contains("| Peak memory geomean (all suites) | ≤ 0.60× | 0.50× | 2 | pass |"),
            "{report}"
        );
        assert!(report.contains("| micro | 1 | 1 | 0 | 1 | 0 | 0 |"), "{report}");
    }

    #[test]
    fn differing_fingerprints_exclude_a_benchmark() {
        let mut cpp = record("cpp", "benchmark/h2oai/q01.benchmark", Status::Ok, 10.0, 10);
        let mut rust = record("rust", "benchmark/h2oai/q01.benchmark", Status::Ok, 5.0, 5);
        cpp.result_fingerprint = Some("a".into());
        rust.result_fingerprint = Some("b".into());
        let pair = Pair {
            cpp: &cpp,
            rust: Some(&rust),
        };
        assert!(pair.results_differ());
        assert_eq!(pair.speedup(), None);
        rust.result_fingerprint = Some("a".into());
        let pair = Pair {
            cpp: &cpp,
            rust: Some(&rust),
        };
        assert_eq!(pair.speedup(), Some(2.0));
        assert_eq!(pair.memory_ratio(), Some(0.5));
    }

    #[test]
    fn formats_units() {
        assert_eq!(format_ms(1.234), "1.23 ms");
        assert_eq!(format_ms(12.34), "12.3 ms");
        assert_eq!(format_ms(1234.0), "1.23 s");
        assert_eq!(format_bytes(1024 * 1024 * 3), "3.0 MiB");
        assert_eq!(format_bytes(1024 * 1024 * 1024 * 2), "2.00 GiB");
    }
}
