//! Process-level measurement: peak RSS via `wait4`/`getrusage`, wall time, timeouts,
//! and summary statistics over run timings.

use std::io;
use std::process::{Child, Command};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// How a measured child process ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exit {
    Code(i32),
    Signal(i32),
    /// Killed by us after exceeding its time budget.
    TimedOut,
}

/// The result of [`run_measured`].
#[derive(Clone, Copy, Debug)]
pub struct Measured {
    pub exit: Exit,
    /// Peak resident set size of the child, in bytes.
    pub peak_rss_bytes: u64,
    pub wall: Duration,
}

/// `ru_maxrss` in bytes: macOS reports bytes, Linux and the BSDs kibibytes.
fn max_rss_bytes(usage: &libc::rusage) -> u64 {
    let raw = u64::try_from(usage.ru_maxrss).unwrap_or(0);
    if cfg!(target_os = "macos") {
        raw
    } else {
        raw * 1024
    }
}

/// Peak RSS of the current process so far, in bytes.
pub fn self_peak_rss_bytes() -> Option<u64> {
    // SAFETY: an all-zero `rusage` is a valid value of this plain C struct.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: `usage` is a valid out-pointer.
    (unsafe { libc::getrusage(libc::RUSAGE_SELF, &raw mut usage) } == 0).then(|| max_rss_bytes(&usage))
}

/// Spawns `command`, waits for it with `wait4` to obtain its own rusage (so the peak RSS
/// is exactly this process's), and kills it with `SIGKILL` once `budget` elapses.
pub fn run_measured(command: &mut Command, budget: Option<Duration>) -> io::Result<Measured> {
    let start = Instant::now();
    let child: Child = command.spawn()?;
    let pid = libc::pid_t::try_from(child.id()).map_err(io::Error::other)?;
    let (done, done_signal) = mpsc::channel::<()>();
    let killer = budget.map(|budget| {
        std::thread::spawn(move || {
            if done_signal.recv_timeout(budget) == Err(mpsc::RecvTimeoutError::Timeout) {
                // SAFETY: `kill` has no memory-safety preconditions. The child is not
                // reaped until this thread has been joined, so `pid` still names it.
                unsafe { libc::kill(pid, libc::SIGKILL) };
                true
            } else {
                false
            }
        })
    });
    // Wait for the exit without reaping, stop the killer, then reap with `wait4`: this
    // way the killer can never signal a recycled pid.
    let waited = retry_interrupted(|| {
        // SAFETY: an all-zero `siginfo_t` is a valid value of this plain C struct.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: a valid out-pointer; `pid` is our unreaped child.
        unsafe {
            libc::waitid(
                libc::P_PID,
                libc::id_t::try_from(pid).unwrap_or_default(),
                &raw mut info,
                libc::WEXITED | libc::WNOWAIT,
            )
        }
    });
    let wall = start.elapsed();
    let _ = done.send(());
    let killed = killer.is_some_and(|k| k.join().unwrap_or(false));
    let mut status: libc::c_int = 0;
    // SAFETY: an all-zero `rusage` is a valid value of this plain C struct.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: valid out-pointers; `pid` is our exited, unreaped child.
    let reaped = retry_interrupted(|| unsafe { libc::wait4(pid, &raw mut status, 0, &raw mut usage) });
    // The child is reaped; `Child` must not wait for it again (dropping it does not).
    drop(child);
    if waited == -1 || reaped == -1 {
        return Err(io::Error::last_os_error());
    }
    let exit = if killed && libc::WIFSIGNALED(status) && libc::WTERMSIG(status) == libc::SIGKILL {
        Exit::TimedOut
    } else if libc::WIFEXITED(status) {
        Exit::Code(libc::WEXITSTATUS(status))
    } else {
        Exit::Signal(libc::WTERMSIG(status))
    };
    Ok(Measured {
        exit,
        peak_rss_bytes: max_rss_bytes(&usage),
        wall,
    })
}

/// Repeats a libc call while it fails with `EINTR`.
fn retry_interrupted(mut call: impl FnMut() -> libc::c_int) -> libc::c_int {
    loop {
        let r = call();
        if r != -1 || io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
            return r;
        }
    }
}

/// Median, minimum and mean of a sample.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Summary {
    pub median: f64,
    pub min: f64,
    pub mean: f64,
}

impl Summary {
    /// `None` for an empty sample.
    pub fn of(values: &[f64]) -> Option<Self> {
        if values.is_empty() {
            return None;
        }
        let mut sorted = values.to_vec();
        sorted.sort_by(f64::total_cmp);
        let n = sorted.len();
        let median = if n % 2 == 1 {
            sorted[n / 2]
        } else {
            f64::midpoint(sorted[n / 2 - 1], sorted[n / 2])
        };
        Some(Self {
            median,
            min: sorted[0],
            mean: sorted.iter().sum::<f64>() / n as f64,
        })
    }
}

/// Geometric mean of positive values; `None` when empty.
pub fn geomean(values: impl IntoIterator<Item = f64>) -> Option<f64> {
    let (sum, n) = values
        .into_iter()
        .filter(|v| *v > 0.0)
        .fold((0.0, 0_usize), |(sum, n), v| (sum + v.ln(), n + 1));
    (n > 0).then(|| (sum / n as f64).exp())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_and_geomean() {
        let s = Summary::of(&[3.0, 1.0, 2.0, 10.0]).unwrap();
        assert!((s.median - 2.5).abs() < 1e-12);
        assert!((s.min - 1.0).abs() < 1e-12);
        assert!((s.mean - 4.0).abs() < 1e-12);
        assert!((Summary::of(&[5.0, 1.0, 3.0]).unwrap().median - 3.0).abs() < 1e-12);
        assert!(Summary::of(&[]).is_none());
        assert!((geomean([2.0, 8.0]).unwrap() - 4.0).abs() < 1e-12);
        assert!(geomean([]).is_none());
    }

    #[test]
    fn measures_a_child_process() {
        let measured = run_measured(&mut Command::new("true"), Some(Duration::from_secs(10))).unwrap();
        assert_eq!(measured.exit, Exit::Code(0));
        assert!(measured.peak_rss_bytes > 0);
        let measured =
            run_measured(Command::new("sleep").arg("10"), Some(Duration::from_millis(100))).unwrap();
        assert_eq!(measured.exit, Exit::TimedOut);
        assert!(self_peak_rss_bytes().unwrap() > 0);
    }
}
