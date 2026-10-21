//! Where the shell prints (`ShellState::out` and `ShellState::Print`): standard output,
//! standard error, a file of `.output`/`.once`, or the pipe to a pager or a `|command`.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::process::{Child, ChildStdin};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::interrupt;
use crate::printer;

/// The stream a print goes to (`PrintOutput`): the shell's output or standard error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrintOutput {
    Stdout,
    Stderr,
}

/// The `FILE *` the shell's output goes to.
#[derive(Debug)]
pub enum OutputTarget {
    Stdout,
    Stderr,
    File(BufWriter<File>),
    /// A process reading the output (`popen`): a pager or the command of `.output |cmd`.
    Pipe { child: Child, stdin: BufWriter<ChildStdin> },
}

impl OutputTarget {
    fn write_all(&mut self, bytes: &[u8]) {
        let _ = match self {
            Self::Stdout => {
                printer::write_stdout(bytes);
                Ok(())
            }
            Self::Stderr => {
                printer::write_stderr(bytes);
                Ok(())
            }
            Self::File(file) => file.write_all(bytes),
            Self::Pipe { stdin, .. } => stdin.write_all(bytes),
        };
    }

    fn flush(&mut self) {
        let _ = match self {
            Self::Stdout => {
                printer::flush_stdout();
                Ok(())
            }
            Self::Stderr => Ok(()),
            Self::File(file) => file.flush(),
            Self::Pipe { stdin, .. } => stdin.flush(),
        };
    }

    /// Closes the target (`fclose` / `pclose`): files are flushed, a piped process gets
    /// the end of its input and is waited for.
    pub fn close(self) {
        match self {
            Self::Stdout | Self::Stderr => {}
            Self::File(mut file) => {
                let _ = file.flush();
            }
            Self::Pipe { mut child, stdin } => {
                drop(stdin);
                let _ = child.wait();
            }
        }
    }
}

/// The shell's output, shared with the log storage that prints warnings to it.
#[derive(Clone, Debug)]
pub struct Output(Arc<Mutex<OutputTarget>>);

impl Default for Output {
    fn default() -> Self {
        Self(Arc::new(Mutex::new(OutputTarget::Stdout)))
    }
}

impl Output {
    fn target(&self) -> MutexGuard<'_, OutputTarget> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// `ShellState::Print`: nothing is printed after an interrupt.
    pub fn print(&self, output: PrintOutput, text: impl AsRef<[u8]>) {
        if interrupt::seen_interrupt() {
            return;
        }
        match output {
            PrintOutput::Stdout => self.target().write_all(text.as_ref()),
            PrintOutput::Stderr => printer::write_stderr(text.as_ref()),
        }
    }

    /// `fflush(out)`.
    pub fn flush(&self) {
        self.target().flush();
    }

    /// Whether the output is the process's standard output (`out == stdout`).
    pub fn is_stdout(&self) -> bool {
        matches!(*self.target(), OutputTarget::Stdout)
    }

    /// Redirects the output, returning the previous target.
    pub fn replace(&self, target: OutputTarget) -> OutputTarget {
        std::mem::replace(&mut *self.target(), target)
    }
}
