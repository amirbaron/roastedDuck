//! The process's standard streams as the shell writes them: C's `stdout` (fully buffered
//! unless it is a terminal) and `stderr` (unbuffered), and DuckDB's `Printer` queries of
//! the terminal (`src/common/printer.cpp`).

use std::io::{BufWriter, IsTerminal, Stdout, Write};
use std::sync::{LazyLock, Mutex, PoisonError};

/// `Printer::IsTerminal(STREAM_STDOUT)`.
pub fn stdout_is_terminal() -> bool {
    std::io::stdout().is_terminal()
}

/// `Printer::TerminalWidth`: the column count of the terminal on standard input, 120 if
/// it has none.
pub fn terminal_width() -> usize {
    // SAFETY: `TIOCGWINSZ` writes a `winsize` into the struct passed.
    let mut size: libc::winsize = unsafe { std::mem::zeroed() };
    // SAFETY: as above; a failing call leaves `size` zeroed.
    unsafe {
        libc::ioctl(0, libc::TIOCGWINSZ, &raw mut size);
    }
    if size.ws_col == 0 {
        120
    } else {
        usize::from(size.ws_col)
    }
}

/// C's `stdout`: buffered unless it is a terminal, where every write goes out at once
/// (the line buffering of a terminal, as the shell's lines end in newlines).
#[derive(Debug)]
enum StdoutBuffer {
    Terminal(Stdout),
    Buffered(BufWriter<Stdout>),
}

static STDOUT: LazyLock<Mutex<StdoutBuffer>> = LazyLock::new(|| {
    let stdout = std::io::stdout();
    Mutex::new(if stdout.is_terminal() {
        StdoutBuffer::Terminal(stdout)
    } else {
        StdoutBuffer::Buffered(BufWriter::with_capacity(1 << 16, stdout))
    })
});

/// Writes to standard output (`fwrite` to `stdout`); write errors are ignored like C's.
pub fn write_stdout(bytes: &[u8]) {
    let mut stdout = STDOUT.lock().unwrap_or_else(PoisonError::into_inner);
    let _ = match &mut *stdout {
        StdoutBuffer::Terminal(stdout) => stdout.write_all(bytes).and_then(|()| stdout.flush()),
        StdoutBuffer::Buffered(buffer) => buffer.write_all(bytes),
    };
}

/// `fflush(stdout)`.
pub fn flush_stdout() {
    let mut stdout = STDOUT.lock().unwrap_or_else(PoisonError::into_inner);
    let _ = match &mut *stdout {
        StdoutBuffer::Terminal(stdout) => stdout.flush(),
        StdoutBuffer::Buffered(buffer) => buffer.flush(),
    };
}

/// Writes to standard error, which is unbuffered.
pub fn write_stderr(bytes: &[u8]) {
    let _ = std::io::stderr().write_all(bytes);
}
