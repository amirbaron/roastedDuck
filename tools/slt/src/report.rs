//! Results files and the reports built from them: the per-directory summary of a run, the
//! C++-vs-Rust conformance dashboard, and the verdict diff used to validate the harness.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::fs;
use std::io::{self, BufRead, BufWriter, Write};
use std::path::Path;

use crate::outcome::{FileResult, Status};
use crate::ratchet;

/// Reads a JSON-lines results file.
pub fn read_results(path: &Path) -> io::Result<Vec<FileResult>> {
    let file = fs::File::open(path)
        .map_err(|error| io::Error::new(error.kind(), format!("{}: {error}", path.display())))?;
    let mut results = Vec::new();
    for (index, line) in io::BufReader::new(file).lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let result = serde_json::from_str(&line).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{}:{}: {error}", path.display(), index + 1),
            )
        })?;
        results.push(result);
    }
    Ok(results)
}

/// Writes results as JSON lines.
pub fn write_results(path: &Path, results: &[FileResult]) -> io::Result<()> {
    if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    let mut out = BufWriter::new(fs::File::create(path)?);
    for result in results {
        serde_json::to_writer(&mut out, result)?;
        out.write_all(b"\n")?;
    }
    out.flush()
}

/// The report group of a test: its directory, cut to three components (`test/sql/<dir>`).
pub fn group_of(file: &str) -> String {
    let directory = file.rsplit_once('/').map_or("", |(directory, _)| directory);
    directory.split('/').take(3).collect::<Vec<_>>().join("/")
}

/// Per-group status counts of one run, as a plain-text table.
pub fn summary_table(results: &[FileResult]) -> String {
    let mut groups: BTreeMap<String, [usize; 5]> = BTreeMap::new();
    for result in results {
        let counts = groups.entry(group_of(&result.file)).or_default();
        counts[status_index(result.status)] += 1;
    }
    let mut total = [0usize; 5];
    for counts in groups.values() {
        for (sum, count) in total.iter_mut().zip(counts) {
            *sum += count;
        }
    }
    let width = groups.keys().map(String::len).max().unwrap_or(0).max(5);
    let mut out = format!("{:<width$} {:>6}", "group", "files");
    for status in Status::ALL {
        let _ = write!(out, " {:>7}", status.as_str());
    }
    out.push('\n');
    for (group, counts) in groups
        .iter()
        .chain(std::iter::once((&"TOTAL".to_owned(), &total)))
    {
        let _ = write!(out, "{group:<width$} {:>6}", counts.iter().sum::<usize>());
        for count in counts {
            let _ = write!(out, " {count:>7}");
        }
        out.push('\n');
    }
    out
}

fn status_index(status: Status) -> usize {
    Status::ALL.iter().position(|s| *s == status).unwrap_or(0)
}

/// Per-group comparison counters for the dashboard.
#[derive(Clone, Copy, Debug, Default)]
struct Row {
    files: usize,
    cpp_pass: usize,
    cpp_skip: usize,
    cpp_fail: usize,
    rust_passing: usize,
    both_pass: usize,
    rust_fail: usize,
    rust_crash: usize,
    rust_skip_only: usize,
}

impl Row {
    fn add(&mut self, cpp: Option<&FileResult>, rust: Option<&FileResult>) {
        self.files += 1;
        let cpp_status = cpp.map(|r| r.status);
        match cpp_status {
            Some(Status::Pass) => self.cpp_pass += 1,
            Some(Status::Skip) => self.cpp_skip += 1,
            _ => self.cpp_fail += 1,
        }
        let Some(rust) = rust else {
            return;
        };
        if ratchet::is_passing(rust, cpp) {
            self.rust_passing += 1;
        }
        if rust.status == Status::Pass && cpp_status == Some(Status::Pass) {
            self.both_pass += 1;
        }
        match rust.status {
            Status::Fail => self.rust_fail += 1,
            Status::Crash | Status::Timeout => self.rust_crash += 1,
            Status::Skip if cpp_status == Some(Status::Pass) => self.rust_skip_only += 1,
            _ => {}
        }
    }

    fn merge(&mut self, other: &Self) {
        self.files += other.files;
        self.cpp_pass += other.cpp_pass;
        self.cpp_skip += other.cpp_skip;
        self.cpp_fail += other.cpp_fail;
        self.rust_passing += other.rust_passing;
        self.both_pass += other.both_pass;
        self.rust_fail += other.rust_fail;
        self.rust_crash += other.rust_crash;
        self.rust_skip_only += other.rust_skip_only;
    }

    fn markdown(&self, name: &str) -> String {
        let percent = if self.files == 0 {
            0.0
        } else {
            100.0 * self.rust_passing as f64 / self.files as f64
        };
        format!(
            "| {name} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {percent:.1}% |\n",
            self.files,
            self.cpp_pass,
            self.cpp_skip,
            self.cpp_fail,
            self.rust_passing,
            self.both_pass,
            self.rust_fail,
            self.rust_crash,
            self.rust_skip_only,
        )
    }
}

/// The conformance dashboard: per `test/sql/<dir>` counts of how the Rust engine does on files,
/// next to the C++ reference verdicts.
pub fn comparison_dashboard(cpp: &[FileResult], rust: &[FileResult]) -> String {
    let cpp_by_file: HashMap<&str, &FileResult> = cpp.iter().map(|r| (r.file.as_str(), r)).collect();
    let rust_by_file: HashMap<&str, &FileResult> = rust.iter().map(|r| (r.file.as_str(), r)).collect();
    let mut files: Vec<&str> = cpp_by_file.keys().chain(rust_by_file.keys()).copied().collect();
    files.sort_unstable();
    files.dedup();
    let mut groups: BTreeMap<String, Row> = BTreeMap::new();
    for file in &files {
        groups
            .entry(group_of(file))
            .or_default()
            .add(cpp_by_file.get(file).copied(), rust_by_file.get(file).copied());
    }
    let mut total = Row::default();
    for row in groups.values() {
        total.merge(row);
    }
    let mut out = String::from("# rustdb conformance dashboard\n\n");
    let _ = writeln!(
        out,
        "Rust passing: **{} / {} files** ({} pass on both engines). \"Passing\" means the Rust run passed, or \
         both engines skipped the file for the same reason.\n",
        total.rust_passing, total.files, total.both_pass
    );
    out.push_str(
        "| group | files | C++ pass | C++ skip | C++ fail | Rust passing | both pass | Rust fail | Rust \
         crash/timeout | Rust skip (C++ pass) | Rust % |\n|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|\n",
    );
    for (group, row) in &groups {
        out.push_str(&row.markdown(group));
    }
    out.push_str(&total.markdown("**total**"));
    out
}

/// A file whose verdict differs between two runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Discrepancy {
    pub file: String,
    pub left: String,
    pub right: String,
}

fn verdict(result: Option<&FileResult>) -> String {
    match result {
        None => "missing".to_owned(),
        Some(result) => match (&result.status, &result.reason) {
            (Status::Skip, Some(reason)) => format!("skip ({reason})"),
            (status, _) => status.to_string(),
        },
    }
}

/// Files whose status (or skip reason) differs between `left` and `right`.
pub fn verdict_diff(left: &[FileResult], right: &[FileResult]) -> Vec<Discrepancy> {
    let left_by_file: HashMap<&str, &FileResult> = left.iter().map(|r| (r.file.as_str(), r)).collect();
    let right_by_file: HashMap<&str, &FileResult> = right.iter().map(|r| (r.file.as_str(), r)).collect();
    let mut files: Vec<&str> = left_by_file.keys().chain(right_by_file.keys()).copied().collect();
    files.sort_unstable();
    files.dedup();
    files
        .into_iter()
        .filter_map(|file| {
            let (left, right) = (
                verdict(left_by_file.get(file).copied()),
                verdict(right_by_file.get(file).copied()),
            );
            (left != right).then(|| Discrepancy {
                file: file.to_owned(),
                left,
                right,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(file: &str, status: Status, reason: Option<&str>) -> FileResult {
        FileResult {
            file: file.to_owned(),
            status,
            reason: reason.map(str::to_owned),
            failure: None,
            duration_ms: 0,
        }
    }

    #[test]
    fn groups() {
        assert_eq!(
            group_of("test/sql/aggregate/aggregates/x.test"),
            "test/sql/aggregate"
        );
        assert_eq!(group_of("test/sqlite/select1.test_slow"), "test/sqlite");
        assert_eq!(group_of("x.test"), "");
    }

    #[test]
    fn dashboard_counts() {
        let cpp = [
            result("test/sql/a/1.test", Status::Pass, None),
            result("test/sql/a/2.test", Status::Skip, Some("require icu")),
        ];
        let rust = [
            result("test/sql/a/1.test", Status::Fail, None),
            result("test/sql/a/2.test", Status::Skip, Some("require icu")),
        ];
        let dashboard = comparison_dashboard(&cpp, &rust);
        assert!(dashboard.contains("| test/sql/a | 2 | 1 | 1 | 0 | 1 | 0 | 1 | 0 | 0 | 50.0% |"));
    }

    #[test]
    fn diffs_compare_status_and_skip_reason() {
        let left = [
            result("a", Status::Pass, None),
            result("b", Status::Skip, Some("require x")),
            result("c", Status::Fail, None),
        ];
        let right = [
            result("a", Status::Pass, None),
            result("b", Status::Skip, Some("require y")),
        ];
        let diff = verdict_diff(&left, &right);
        assert_eq!(diff.len(), 2);
        assert_eq!(diff[1].right, "missing");
    }

    #[test]
    fn results_round_trip() {
        let path = std::env::temp_dir().join(format!("slt-report-test-{}.jsonl", std::process::id()));
        let results = vec![result("a", Status::Skip, Some("require x"))];
        write_results(&path, &results).unwrap();
        assert_eq!(read_results(&path).unwrap(), results);
        let _ = fs::remove_file(path);
    }
}
