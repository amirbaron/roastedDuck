//! The conformance ratchet: `conformance/passing.txt` lists every test file that passes on the
//! Rust engine, and the list may only grow.

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::io;
use std::path::Path;

use crate::outcome::{FileResult, Status};

/// Whether a Rust result counts as passing: it passed, or it was skipped for the same reason the
/// C++ reference skipped it (e.g. both lack the same optional extension).
pub fn is_passing(rust: &FileResult, cpp: Option<&FileResult>) -> bool {
    match rust.status {
        Status::Pass => true,
        Status::Skip => cpp.is_some_and(|cpp| cpp.status == Status::Skip && cpp.reason == rust.reason),
        _ => false,
    }
}

/// All files that count as passing in `rust`, given the C++ reference results.
pub fn passing_files(rust: &[FileResult], cpp: &[FileResult]) -> BTreeSet<String> {
    let cpp_by_file: HashMap<&str, &FileResult> = cpp.iter().map(|r| (r.file.as_str(), r)).collect();
    rust.iter()
        .filter(|result| is_passing(result, cpp_by_file.get(result.file.as_str()).copied()))
        .map(|result| result.file.clone())
        .collect()
}

/// Reads a ratchet list (one file per line; `#` comments and blank lines ignored). A missing file
/// is an empty list.
pub fn read_list(path: &Path) -> io::Result<BTreeSet<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(str::to_owned)
            .collect()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(BTreeSet::new()),
        Err(error) => Err(error),
    }
}

/// Writes a ratchet list, sorted, with a short header.
pub fn write_list(path: &Path, files: &BTreeSet<String>) -> io::Result<()> {
    let mut text = String::from(
        "# Test files that pass on the Rust engine (see tools/slt). CI fails if any of them stops passing.\n\
         # Update with: slt ratchet update <rust-results.jsonl>\n",
    );
    for file in files {
        text.push_str(file);
        text.push('\n');
    }
    if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, text)
}

/// The outcome of checking results against the ratchet list.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct RatchetReport {
    /// Listed files that no longer pass, with their current verdict.
    pub regressions: Vec<(String, String)>,
    /// Passing files that are not listed yet.
    pub newly_passing: Vec<String>,
}

/// Compares the current passing set against the ratchet list.
pub fn check(listed: &BTreeSet<String>, rust: &[FileResult], cpp: &[FileResult]) -> RatchetReport {
    let passing = passing_files(rust, cpp);
    let rust_by_file: HashMap<&str, &FileResult> = rust.iter().map(|r| (r.file.as_str(), r)).collect();
    let regressions = listed
        .iter()
        .filter(|file| !passing.contains(*file))
        .map(|file| {
            let verdict = rust_by_file.get(file.as_str()).map_or_else(
                || "missing from results".to_owned(),
                |result| result.status.to_string(),
            );
            (file.clone(), verdict)
        })
        .collect();
    let newly_passing = passing.difference(listed).cloned().collect();
    RatchetReport {
        regressions,
        newly_passing,
    }
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
    fn skips_pass_only_with_the_same_reason() {
        let rust = result("a", Status::Skip, Some("require icu"));
        assert!(is_passing(
            &rust,
            Some(&result("a", Status::Skip, Some("require icu")))
        ));
        assert!(!is_passing(
            &rust,
            Some(&result("a", Status::Skip, Some("require tpch")))
        ));
        assert!(!is_passing(&rust, Some(&result("a", Status::Pass, None))));
        assert!(!is_passing(&rust, None));
        assert!(is_passing(&result("a", Status::Pass, None), None));
    }

    #[test]
    fn regressions_and_new_files() {
        let listed: BTreeSet<String> = ["a", "b"].map(str::to_owned).into();
        let rust = [
            result("a", Status::Pass, None),
            result("b", Status::Fail, None),
            result("c", Status::Pass, None),
        ];
        let report = check(&listed, &rust, &[]);
        assert_eq!(report.regressions, [("b".to_owned(), "fail".to_owned())]);
        assert_eq!(report.newly_passing, ["c"]);
    }

    #[test]
    fn list_round_trip() {
        let path = std::env::temp_dir().join(format!("slt-ratchet-test-{}.txt", std::process::id()));
        let files: BTreeSet<String> = ["test/b.test", "test/a.test"].map(str::to_owned).into();
        write_list(&path, &files).unwrap();
        assert_eq!(read_list(&path).unwrap(), files);
        let _ = fs::remove_file(path);
    }
}
