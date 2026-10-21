//! The process's standard streams as the shell writes them: C's `stdout` (fully buffered
//! unless it is a terminal) and `stderr` (unbuffered), and DuckDB's `Printer` queries of
//! the terminal (`src/common/printer.cpp`).

use std::io::{IsTerminal, Stdout, Write};
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
    Buffered(FullyBuffered),
}

/// A fully buffered C stream (`__sfvwrite`): the buffer, as large as the descriptor's
/// `st_blksize` (a pipe's 16 KiB, a file's 4 KiB), goes out whenever it is full, so the
/// output reaches the descriptor in blocks of that size between flushes, and interleaves
/// with standard error (unbuffered) where C's does.
#[derive(Debug)]
struct FullyBuffered {
    stdout: Stdout,
    buffer: Vec<u8>,
    capacity: usize,
}

impl FullyBuffered {
    fn new(stdout: Stdout) -> Self {
        // SAFETY: `fstat` writes a `stat` into the struct passed.
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: as above; a failing call is checked.
        let blksize = if unsafe { libc::fstat(1, &raw mut stat) } == 0 {
            usize::try_from(stat.st_blksize).unwrap_or(0)
        } else {
            0
        };
        // `BUFSIZ` without a block size.
        let capacity = if blksize == 0 { 1024 } else { blksize };
        Self {
            stdout,
            buffer: Vec::with_capacity(capacity),
            capacity,
        }
    }

    fn write(&mut self, mut bytes: &[u8]) -> std::io::Result<()> {
        while !bytes.is_empty() {
            let space = self.capacity - self.buffer.len();
            if !self.buffer.is_empty() && bytes.len() > space {
                // fill and flush
                self.buffer.extend_from_slice(&bytes[..space]);
                bytes = &bytes[space..];
                self.flush()?;
            } else if bytes.len() >= self.capacity {
                // write a block directly (through `Stdout`'s line buffer, flushed at once)
                self.stdout.write_all(&bytes[..self.capacity])?;
                self.stdout.flush()?;
                bytes = &bytes[self.capacity..];
            } else {
                // fill and done
                self.buffer.extend_from_slice(bytes);
                bytes = &[];
            }
        }
        Ok(())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        let written = self
            .stdout
            .write_all(&self.buffer)
            .and_then(|()| self.stdout.flush());
        self.buffer.clear();
        written
    }
}

static STDOUT: LazyLock<Mutex<StdoutBuffer>> = LazyLock::new(|| {
    let stdout = std::io::stdout();
    Mutex::new(if stdout.is_terminal() {
        StdoutBuffer::Terminal(stdout)
    } else {
        StdoutBuffer::Buffered(FullyBuffered::new(stdout))
    })
});

/// Writes to standard output (`fwrite` to `stdout`); write errors are ignored like C's.
pub fn write_stdout(bytes: &[u8]) {
    let mut stdout = STDOUT.lock().unwrap_or_else(PoisonError::into_inner);
    let _ = match &mut *stdout {
        StdoutBuffer::Terminal(stdout) => stdout.write_all(bytes).and_then(|()| stdout.flush()),
        StdoutBuffer::Buffered(buffer) => buffer.write(bytes),
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
