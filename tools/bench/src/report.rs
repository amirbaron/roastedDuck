//! `bench compare`: a markdown report of roastedduck against C++ DuckDB, with per-benchmark
//! speedups and memory ratios, per-suite geometric means and individual query outliers.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use crate::measure::geomean;
use crate::record::{BenchmarkRecord, MachineInfo, Results, Status};

/// Suites shown together in the speed–memory comparison.
struct ComparisonSuite {
    label: &'static str,
    selects: fn(&BenchmarkRecord) -> bool,
}

const COMPARISON_SUITES: [ComparisonSuite; 3] = [
    ComparisonSuite {
        label: "TPC-H SF10",
        selects: |r| {
            r.name.starts_with("benchmark/tpch/sf1/") && r.arguments.get("sf").is_some_and(|sf| sf == "10")
        },
    },
    ComparisonSuite {
        label: "ClickBench",
        selects: |r| r.suite == "clickbench",
    },
    ComparisonSuite {
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
    out.push_str("# roastedduck vs C++ DuckDB: benchmark comparison\n\n");
    write_runs(&mut out, cpp, rust, cpp_label, rust_label);
    write_performance_policy(&mut out, &pairs);
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
    let threads = |r: &Results| r.machine.iter().map(|m| m.threads).collect::<BTreeSet<_>>();
    let hosts = |r: &Results| {
        r.machine
            .iter()
            .map(|m| m.hostname.clone())
            .collect::<BTreeSet<_>>()
    };
    if !rust.machine.is_empty() && (threads(cpp) != threads(rust) || hosts(cpp) != hosts(rust)) {
        out.push_str(
            "\n> **Warning:** the two runs used different machines or thread counts; ratios are not meaningful.\n",
        );
    }
    out.push('\n');
}

fn ratio(value: Option<f64>) -> String {
    value.map_or_else(|| "–".to_owned(), |v| format!("{v:.2}×"))
}

fn write_performance_policy(out: &mut String, pairs: &[Pair<'_>]) {
    out.push_str("## Performance acceptance policy (2026-10-23)\n\n");
    out.push_str("Preserve the lower-memory uncached baseline. Roughly 15–20% longer execution time with 60–70% less memory on TPC-H and ClickBench is acceptable. Evaluate speed and memory together; do not increase memory merely to match C++ caching or close a small speed gap.\n\n");
    out.push_str("**Final acceptance: pending baseline comparison and documented review.** Use `bench ratchet check` against the preserved Rust baseline and complete `docs/performance-acceptance.md`. C++ ratios below are measurements; report individual query regressions and outliers separately from suite averages.\n\n");
    out.push_str("Only correct, comparable results count. Result mismatches, errors and missing results remain visible in the checks and coverage below.\n\n");
    out.push_str("| Comparison | Value | Benchmarks |\n|---|---:|---:|\n");
    for suite in &COMPARISON_SUITES {
        let speedups: Vec<f64> = pairs
            .iter()
            .filter(|p| (suite.selects)(p.cpp))
            .filter_map(Pair::speedup)
            .collect();
        let _ = writeln!(
            out,
            "| Speed geomean, {} | {} | {} |",
            suite.label,
            ratio(geomean(speedups.iter().copied())),
            speedups.len()
        );
    }
    let memory: Vec<f64> = pairs.iter().filter_map(Pair::memory_ratio).collect();
    let _ = writeln!(
        out,
        "| Peak memory geomean (all suites) | {} | {} |\n",
        ratio(geomean(memory.iter().copied())),
        memory.len()
    );
    out.push_str("### Individual query comparisons\n\n");
    out.push_str("The ten slowest comparisons and ten largest memory ratios are listed separately; all comparable queries appear in the detailed table. These C++ comparisons supplement the per-query Rust-baseline ratchet.\n\n");
    for memory_order in [false, true] {
        let mut ranked: Vec<(f64, &Pair<'_>)> = pairs
            .iter()
            .filter_map(|pair| {
                let value = if memory_order {
                    pair.memory_ratio()?
                } else {
                    pair.speedup()?
                };
                Some((value, pair))
            })
            .collect();
        ranked.sort_by(|(a, left), (b, right)| {
            let order = if memory_order {
                b.total_cmp(a)
            } else {
                a.total_cmp(b)
            };
            order.then_with(|| left.cpp.key().cmp(&right.cpp.key()))
        });
        out.push_str(if memory_order {
            "Largest memory comparisons:\n\n"
        } else {
            "Slowest comparisons:\n\n"
        });
        out.push_str("| Benchmark | Speed (C++/Rust) | Peak memory (Rust/C++) |\n|---|---:|---:|\n");
        for (_, pair) in ranked.iter().take(10) {
            let _ = writeln!(
                out,
                "| `{}` | {} | {} |",
                pair.cpp.key().trim_start_matches("benchmark/"),
                ratio(pair.speedup()),
                ratio(pair.memory_ratio())
            );
        }
        out.push('\n');
    }
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
    out.push_str("\n\"Rust other\" counts benchmarks that were not run on roastedduck or ended in error, timeout, crash or skip.\n\n");
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
    fn reports_tradeoff_and_individual_outliers() {
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
        // geomean(2.0, 0.5) = 1.0; retain the slow query alongside that average.
        assert!(
            report.contains("| Speed geomean, h2oai | 1.00× | 2 |"),
            "{report}"
        );
        assert!(
            report.contains("| Speed geomean, ClickBench | – | 0 |"),
            "{report}"
        );
        assert!(
            report.contains("| Peak memory geomean (all suites) | 0.50× | 2 |"),
            "{report}"
        );
        assert!(
            report.contains("| `h2oai/q02.benchmark` | 0.50× | 0.50× |"),
            "{report}"
        );
        assert!(
            report.contains("Final acceptance: pending baseline comparison and documented review"),
            "{report}"
        );
        assert!(!report.contains("≥ 1.30×"), "{report}");
        assert!(!report.contains("≤ 0.60×"), "{report}");
        assert!(report.contains("| micro | 1 | 1 | 0 | 1 | 0 | 0 |"), "{report}");
    }

    #[test]
    fn accepted_lower_memory_tradeoff_still_requires_baseline_review() {
        let mut cpp = Results::default();
        let mut rust = Results::default();
        cpp.benchmarks.push(record(
            "cpp",
            "benchmark/clickbench/q01.benchmark",
            Status::Ok,
            100.0,
            1000,
        ));
        rust.benchmarks.push(record(
            "rust",
            "benchmark/clickbench/q01.benchmark",
            Status::Ok,
            120.0,
            300,
        ));
        let report = compare(&cpp, &rust, "cpp.jsonl", "rust.jsonl");
        assert!(
            report.contains("| Speed geomean, ClickBench | 0.83× | 1 |"),
            "{report}"
        );
        assert!(
            report.contains("| Peak memory geomean (all suites) | 0.30× | 1 |"),
            "{report}"
        );
        assert!(
            report.contains("Final acceptance: pending baseline comparison"),
            "{report}"
        );
        assert!(!report.contains("**FAIL**"), "{report}");
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
