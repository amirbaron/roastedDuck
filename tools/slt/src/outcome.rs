//! Test outcomes: the in-process failure type and the per-file verdict records written as JSON.

use std::fmt;

use serde::{Deserialize, Serialize};

/// A test failure, the equivalent of Catch's `FAIL`/`FAIL_LINE`: it aborts the current file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Failure {
    /// 1-based line in the test file, when the failure is tied to a record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    pub message: String,
}

impl Failure {
    /// A failure not tied to a particular line.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            line: None,
            message: message.into(),
        }
    }

    /// A failure at `line` (1-based).
    pub fn at(line: usize, message: impl Into<String>) -> Self {
        Self {
            line: Some(line),
            message: message.into(),
        }
    }

    /// Attaches `line` unless the failure already has one.
    #[must_use]
    pub fn or_at(mut self, line: Option<usize>) -> Self {
        if self.line.is_none() {
            self.line = line;
        }
        self
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.line {
            Some(line) => write!(f, "line {line}: {}", self.message),
            None => f.write_str(&self.message),
        }
    }
}

/// Result type of everything that can fail a test file.
pub type TestResult<T = ()> = Result<T, Failure>;

/// The verdict for one test file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Pass,
    Fail,
    Skip,
    /// The process running the file died (signal, abort) or produced no verdict.
    Crash,
    Timeout,
}

impl Status {
    /// All statuses, in report column order.
    pub const ALL: [Self; 5] = [Self::Pass, Self::Fail, Self::Skip, Self::Crash, Self::Timeout];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::Skip => "skip",
            Self::Crash => "crash",
            Self::Timeout => "timeout",
        }
    }
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One line of a results file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileResult {
    /// Test path relative to the DuckDB source root (e.g. `test/sql/select/x.test`).
    pub file: String,
    pub status: Status,
    /// Skip reason (DuckDB's `SKIP_TEST` text), or what went wrong for crashes and timeouts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<Failure>,
    #[serde(default)]
    pub duration_ms: u64,
}

impl FileResult {
    /// Builds the record for an in-process run.
    pub fn from_run(
        file: String,
        outcome: TestResult,
        skip_reason: Option<String>,
        duration_ms: u64,
    ) -> Self {
        let (status, reason, failure) = match (outcome, skip_reason) {
            (Err(failure), _) => (Status::Fail, None, Some(failure)),
            (Ok(()), Some(reason)) => (Status::Skip, Some(reason), None),
            (Ok(()), None) => (Status::Pass, None, None),
        };
        Self {
            file,
            status,
            reason,
            failure,
            duration_ms,
        }
    }
}
