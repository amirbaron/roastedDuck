//! Running many test files in parallel with crash isolation.
//!
//! Every file runs in its own worker process (`slt exec`, or DuckDB's `unittest` binary for the
//! reference verdicts), so a crash, abort or hang only affects that file. The parent enforces the
//! timeout, cleans up the worker's temporary directory and turns the worker's output into a
//! [`FileResult`].

use std::collections::VecDeque;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use crate::config::TESTING_DIRECTORY_NAME;
use crate::outcome::{Failure, FileResult, Status};

/// Prefix of the line on which `slt exec` reports its verdict.
pub const RESULT_MARKER: &str = "@@slt-result@@ ";

/// Longest failure message kept in a results file.
const MAX_MESSAGE_LEN: usize = 4000;

/// Which engine runs the files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Worker {
    /// `slt exec` with the given engine arguments (`--engine rust|cpp ...`).
    Slt { executable: PathBuf, args: Vec<String> },
    /// DuckDB's own `unittest` binary.
    Unittest { executable: PathBuf },
}

/// Settings for [`run_files`].
#[derive(Clone, Debug)]
pub struct RunOptions {
    pub worker: Worker,
    /// The DuckDB source root; workers run with it as their working directory.
    pub root: PathBuf,
    pub jobs: usize,
    pub timeout: Duration,
    /// Keep each worker's output in this directory.
    pub log_dir: Option<PathBuf>,
}

/// Expands files and directories into test names relative to `root` (`test/...`), sorted.
/// Directories contribute their `.test` and `.test_slow` files recursively.
pub fn collect_test_files(root: &Path, inputs: &[PathBuf]) -> io::Result<Vec<String>> {
    let root = root.canonicalize()?;
    let mut files = Vec::new();
    for input in inputs {
        let path = if input.exists() {
            input.clone()
        } else {
            root.join(input)
        };
        let path = path
            .canonicalize()
            .map_err(|error| io::Error::new(error.kind(), format!("{}: {error}", input.display())))?;
        if path.is_dir() {
            walk(&path, &mut files)?;
        } else {
            files.push(path);
        }
    }
    let mut names: Vec<String> = files
        .into_iter()
        .map(|file| {
            file.strip_prefix(&root).map_or_else(
                |_| file.to_string_lossy().into_owned(),
                |relative| relative.to_string_lossy().replace('\\', "/"),
            )
        })
        .collect();
    names.sort();
    names.dedup();
    Ok(names)
}

fn walk(dir: &Path, files: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            walk(&path, files)?;
        } else if path
            .extension()
            .is_some_and(|extension| extension == "test" || extension == "test_slow")
        {
            files.push(path);
        }
    }
    Ok(())
}

/// Runs every file on `options.jobs` parallel workers. `on_result` sees results as they finish.
pub fn run_files(
    options: &RunOptions,
    files: &[String],
    on_result: &(dyn Fn(&FileResult) + Sync),
) -> Vec<FileResult> {
    let queue = Mutex::new(files.iter().cloned().collect::<VecDeque<_>>());
    let results = Mutex::new(Vec::with_capacity(files.len()));
    let scratch = std::env::temp_dir().join(format!("slt-{}", std::process::id()));
    thread::scope(|scope| {
        for _ in 0..options.jobs.max(1) {
            scope.spawn(|| {
                loop {
                    // Take the next file in its own statement so the queue lock is released
                    // before the (long) run.
                    let next = queue.lock().unwrap_or_else(PoisonError::into_inner).pop_front();
                    let Some(file) = next else {
                        break;
                    };
                    let result = run_one(options, &scratch, &file);
                    on_result(&result);
                    results
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(result);
                }
            });
        }
    });
    let _ = fs::remove_dir_all(&scratch);
    let mut results = results.into_inner().unwrap_or_else(PoisonError::into_inner);
    results.sort_by(|a, b| a.file.cmp(&b.file));
    results
}

/// Runs one file in a worker process and interprets the outcome.
fn run_one(options: &RunOptions, scratch: &Path, file: &str) -> FileResult {
    let started = Instant::now();
    let log_path = match &options.log_dir {
        Some(dir) => dir.join(format!("{}.log", file.replace('/', "__"))),
        None => scratch.join(format!("{}.log", file.replace('/', "__"))),
    };
    let outcome = spawn_and_wait(options, file, &log_path);
    let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let output = fs::read(&log_path).map(|bytes| String::from_utf8_lossy(&bytes).into_owned());
    if options.log_dir.is_none() {
        let _ = fs::remove_file(&log_path);
    }
    let mut result = match (outcome, output) {
        (Err(error), _) | (Ok(_), Err(error)) => crash(file, format!("cannot run worker: {error}")),
        (Ok(None), _) => FileResult {
            file: file.to_owned(),
            status: Status::Timeout,
            reason: Some(format!("no verdict after {} s", options.timeout.as_secs())),
            failure: None,
            duration_ms: 0,
        },
        (Ok(Some(status)), Ok(output)) => match &options.worker {
            Worker::Slt { .. } => parse_slt_output(file, status, &output),
            Worker::Unittest { .. } => parse_unittest_output(file, status, &output),
        },
    };
    result.duration_ms = duration_ms;
    if let Some(failure) = &mut result.failure {
        truncate(&mut failure.message, MAX_MESSAGE_LEN);
    }
    result
}

/// Spawns the worker with stdout and stderr in `log_path`; `Ok(None)` means it timed out.
fn spawn_and_wait(options: &RunOptions, file: &str, log_path: &Path) -> io::Result<Option<ExitStatus>> {
    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let log = File::create(log_path)?;
    let mut command = match &options.worker {
        Worker::Slt { executable, args } => {
            let mut command = Command::new(executable);
            command
                .arg("exec")
                .args(args)
                .arg("--root")
                .arg(&options.root)
                .arg(file);
            command
        }
        Worker::Unittest { executable } => {
            let mut command = Command::new(executable);
            // The upstream binary otherwise changes cwd back to its compiled
            // source root, ignoring the root selected by this harness.
            command.arg("--test-dir").arg(&options.root).arg(file);
            command
        }
    };
    let child = command
        .current_dir(&options.root)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .spawn()?;
    let pid = child.id();
    let status = wait_with_timeout(child, options.timeout);
    // A worker that crashed or was killed cannot clean up its temporary directory.
    let _ = fs::remove_dir_all(options.root.join(TESTING_DIRECTORY_NAME).join(pid.to_string()));
    status
}

fn wait_with_timeout(mut child: Child, timeout: Duration) -> io::Result<Option<ExitStatus>> {
    let deadline = Instant::now() + timeout;
    let mut pause = Duration::from_millis(2);
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(None);
        }
        thread::sleep(pause);
        pause = (pause * 2).min(Duration::from_millis(50));
    }
}

fn crash(file: &str, reason: String) -> FileResult {
    FileResult {
        file: file.to_owned(),
        status: Status::Crash,
        reason: Some(reason),
        failure: None,
        duration_ms: 0,
    }
}

/// Describes an abnormal exit (signal or unexpected exit code).
fn exit_description(status: ExitStatus) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return format!("killed by signal {signal}");
        }
    }
    match status.code() {
        Some(code) => format!("exited with code {code}"),
        None => "exited abnormally".to_owned(),
    }
}

fn killed_by_signal(status: ExitStatus) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        status.signal().is_some()
    }
    #[cfg(not(unix))]
    {
        let _ = status;
        false
    }
}

/// Reads the verdict line an `slt exec` worker printed.
fn parse_slt_output(file: &str, status: ExitStatus, output: &str) -> FileResult {
    let verdict = output
        .lines()
        .rev()
        .find_map(|line| line.strip_prefix(RESULT_MARKER))
        .and_then(|json| serde_json::from_str::<FileResult>(json).ok());
    match verdict {
        Some(result) if !killed_by_signal(status) => result,
        _ => crash(
            file,
            format!(
                "worker {} without a verdict{}",
                exit_description(status),
                tail(output)
            ),
        ),
    }
}

/// Derives a verdict from the console output of `unittest <file>`.
fn parse_unittest_output(file: &str, status: ExitStatus, output: &str) -> FileResult {
    if killed_by_signal(status) {
        return crash(
            file,
            format!("unittest {}{}", exit_description(status), tail(output)),
        );
    }
    let result = |status, reason, failure| FileResult {
        file: file.to_owned(),
        status,
        reason,
        failure,
        duration_ms: 0,
    };
    if output.contains("No test cases matched") {
        return result(
            Status::Fail,
            None,
            Some(Failure::new("unittest: no test case matched the file")),
        );
    }
    if status.code() != Some(0) {
        return result(Status::Fail, None, Some(unittest_failure(file, output)));
    }
    let skip_reason = output
        .lines()
        .skip_while(|line| !line.contains("Skipped tests for the following reasons:"))
        .nth(1)
        .and_then(|line| line.rsplit_once(": "))
        .map(|(reason, _count)| strip_ansi(reason));
    match skip_reason {
        Some(reason) => result(Status::Skip, Some(reason), None),
        None => result(Status::Pass, None, None),
    }
}

/// The first Catch failure report: `<location>: FAILED:` and the lines after it.
fn unittest_failure(file: &str, output: &str) -> Failure {
    let lines: Vec<&str> = output.lines().collect();
    let Some(start) = lines.iter().position(|line| line.contains(": FAILED:")) else {
        return Failure::new(format!("unittest failed{}", tail(output)));
    };
    let location = lines[start].trim_end_matches(": FAILED:");
    let line = location
        .strip_prefix(file)
        .and_then(|rest| rest.strip_prefix(':'))
        .and_then(|number| number.parse().ok());
    let message = lines[start..]
        .iter()
        .take(30)
        .map(|line| strip_ansi(line))
        .collect::<Vec<_>>()
        .join("\n");
    Failure { line, message }
}

fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn tail(output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();
    let start = lines.len().saturating_sub(5);
    if lines.is_empty() {
        String::new()
    } else {
        format!(": {}", lines[start..].join(" | "))
    }
}

fn truncate(text: &mut String, max: usize) {
    if text.len() > max {
        let mut end = max;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push_str("...");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    use std::os::unix::process::ExitStatusExt;

    #[cfg(unix)]
    fn exit(code: i32) -> ExitStatus {
        ExitStatus::from_raw(code << 8)
    }

    #[cfg(unix)]
    #[test]
    fn unittest_verdicts() {
        let pass = parse_unittest_output(
            "t.test",
            exit(0),
            "===\nAll tests passed (6 assertions in 1 test case)\n",
        );
        assert_eq!(pass.status, Status::Pass);
        let skip = parse_unittest_output(
            "t.test",
            exit(0),
            "All tests were skipped (total skipped 1)\n\nSkipped tests for the following reasons:\nrequire tpch: 1\n",
        );
        assert_eq!(
            (skip.status, skip.reason.as_deref()),
            (Status::Skip, Some("require tpch"))
        );
        let fail = parse_unittest_output(
            "t.test",
            exit(1),
            "t.test:12: FAILED:\nexplicitly with message:\n  boom\n",
        );
        assert_eq!(fail.status, Status::Fail);
        assert_eq!(fail.failure.and_then(|f| f.line), Some(12));
        let crashed = parse_unittest_output("t.test", ExitStatus::from_raw(6), "");
        assert_eq!(crashed.status, Status::Crash);
    }

    #[cfg(unix)]
    #[test]
    fn slt_verdicts() {
        let output = format!(
            "noise\n{RESULT_MARKER}{{\"file\":\"t.test\",\"status\":\"skip\",\"reason\":\"require x\"}}\n"
        );
        let result = parse_slt_output("t.test", exit(0), &output);
        assert_eq!(result.status, Status::Skip);
        assert_eq!(
            parse_slt_output("t.test", exit(0), "no verdict").status,
            Status::Crash
        );
    }

    #[test]
    fn ansi_codes_are_removed() {
        assert_eq!(strip_ansi("\u{1b}[0;33mrequire icu\u{1b}[0m"), "require icu");
    }
}
