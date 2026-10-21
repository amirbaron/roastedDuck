//! The history of the line editor (DuckDB's `tools/shell/linenoise/history.cpp`): entries
//! in memory, the history file and its loading.

use std::fs::File;
use std::io::{Read, Write};
use std::sync::{Mutex, MutexGuard, PoisonError};

use super::{LINENOISE_MAX_HISTORY, LINENOISE_MAX_LINE, terminal};

/// `LINENOISE_DEFAULT_HISTORY_MAX_LEN`.
const DEFAULT_HISTORY_MAX_LEN: usize = 1000;

#[derive(Debug)]
struct History {
    max_len: usize,
    entries: Vec<Vec<u8>>,
    /// The file the history was loaded from, which added lines are appended to.
    file: Option<String>,
}

static HISTORY: Mutex<History> = Mutex::new(History {
    max_len: DEFAULT_HISTORY_MAX_LEN,
    entries: Vec::new(),
    file: None,
});

fn history() -> MutexGuard<'static, History> {
    HISTORY.lock().unwrap_or_else(PoisonError::into_inner)
}

/// `History::Free`.
pub fn free() {
    history().entries = Vec::new();
}

/// `History::GetLength`.
pub fn len() -> usize {
    history().entries.len()
}

/// `History::GetEntry`: the entry at `index`, empty if there is none.
pub fn entry(index: usize) -> Vec<u8> {
    history().entries.get(index).cloned().unwrap_or_default()
}

/// `History::Overwrite`.
pub fn overwrite(index: usize, new_entry: &[u8]) {
    if let Some(entry) = history().entries.get_mut(index) {
        new_entry.clone_into(entry);
    }
}

/// `History::RemoveLastEntry`.
pub fn remove_last_entry() {
    history().entries.pop();
}

/// `History::Add`: adds a line unless it repeats the last one or is not UTF-8; in
/// single-line mode newlines become spaces, in multi-line mode `\r\n`. Lines are appended
/// to the history file as well. Returns whether the line was added.
pub fn add(line: &[u8]) -> bool {
    let mut history = history();
    if history.max_len == 0 {
        return false;
    }
    if history.entries.last().is_some_and(|last| last.as_slice() == line) {
        return false;
    }
    if std::str::from_utf8(line).is_err() {
        return false;
    }
    let line_copy: Vec<u8> = if terminal::is_multiline() {
        // Every `\n` not after a `\r` gets one.
        let mut copy = Vec::with_capacity(line.len());
        let mut i = 0;
        while i < line.len() {
            if line[i] == b'\r' && line.get(i + 1) == Some(&b'\n') {
                copy.push(b'\r');
                i += 1;
            } else if line[i] == b'\n' {
                copy.push(b'\r');
            }
            copy.push(line[i]);
            i += 1;
        }
        copy
    } else {
        line.iter()
            .map(|&c| if c == b'\n' || c == b'\r' { b' ' } else { c })
            .collect()
    };
    if history.entries.len() == history.max_len {
        history.entries.remove(0);
    }
    history.entries.push(line_copy);
    if let Some(file) = &history.file
        && !line.is_empty()
    {
        // Appending keeps the history should the shell crash.
        let Ok(mut file) = std::fs::OpenOptions::new().append(true).create(true).open(file) else {
            return true;
        };
        let _ = file.write_all(line).and_then(|()| file.write_all(b"\n"));
    }
    true
}

/// `History::SetMaxLength`: keeps the latest `len` entries.
pub fn set_max_length(len: usize) -> bool {
    if len < 1 {
        return false;
    }
    let mut history = history();
    let excess = history.entries.len().saturating_sub(len);
    history.entries.drain(..excess);
    history.max_len = len;
    true
}

/// `History::Save`: writes the history to `filename`, readable by its owner only.
pub fn save(filename: &str) -> bool {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let history = history();
    let Ok(mut file) = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(filename)
    else {
        return false;
    };
    let _ = std::fs::set_permissions(filename, std::fs::Permissions::from_mode(0o600));
    for entry in &history.entries {
        let _ = file.write_all(entry).and_then(|()| file.write_all(b"\n"));
    }
    true
}

/// `LineReader`: the lines of the history file, skipping lines longer than
/// `LINENOISE_MAX_LINE`, empty ones and ones that are not UTF-8, and reading at most
/// `LINENOISE_MAX_HISTORY` bytes.
struct LineReader {
    file: File,
    filename: String,
    end_of_file: bool,
    data: Vec<u8>,
    position: usize,
    total_read: usize,
}

impl LineReader {
    const LINE_BUFFER_SIZE: usize = LINENOISE_MAX_LINE * 2;

    fn next_newline(&self) -> usize {
        self.data[self.position..]
            .iter()
            .position(|&c| c == b'\r' || c == b'\n')
            .map_or(self.data.len(), |offset| self.position + offset)
    }

    fn skip_newline(&mut self) {
        if self.position >= self.data.len() {
            self.fill_buffer();
        }
        if self.data.get(self.position) == Some(&b'\n') {
            self.position += 1;
        }
    }

    fn next_line(&mut self) -> Option<Vec<u8>> {
        let mut line = Vec::new();
        loop {
            let mut i = self.next_newline();
            let read_count = i - self.position;
            if line.len() + read_count > LINENOISE_MAX_LINE {
                // Too long: skip to the next line.
                let mut found_next_newline = false;
                while !found_next_newline && !self.data.is_empty() {
                    i = self.next_newline();
                    if i < self.data.len() {
                        found_next_newline = true;
                    } else {
                        self.fill_buffer();
                    }
                }
                if !found_next_newline {
                    return None;
                }
                self.position = i + 1;
                if self.data[i] == b'\r' {
                    self.skip_newline();
                }
                continue;
            }
            line.extend_from_slice(&self.data[self.position..i]);
            if i < self.data.len() {
                self.position = i + 1;
                if self.data[i] == b'\r' {
                    self.skip_newline();
                }
                if line.is_empty() || std::str::from_utf8(&line).is_err() {
                    line.clear();
                    continue;
                }
                return Some(line);
            }
            self.fill_buffer();
            if self.data.is_empty() {
                return (!line.is_empty()).then_some(line);
            }
        }
    }

    fn fill_buffer(&mut self) {
        if self.end_of_file {
            return;
        }
        let mut data = vec![0; Self::LINE_BUFFER_SIZE];
        let read = self.file.read(&mut data).unwrap_or(0);
        data.truncate(read);
        self.data = data;
        self.position = 0;
        self.total_read += read;
        if read == 0 {
            self.end_of_file = true;
        }
        if self.total_read >= LINENOISE_MAX_HISTORY {
            crate::printer::write_stderr(
                format!(
                    "History file \"{}\" exceeds maximum history file size of {} MB - skipping full load\n",
                    self.filename,
                    LINENOISE_MAX_HISTORY / 1024 / 1024
                )
                .as_bytes(),
            );
            self.data.clear();
            self.end_of_file = true;
        }
    }
}

/// `History::Load`: dot commands are an entry per line, SQL an entry per complete
/// statement; later additions are appended to the file.
pub fn load(filename: &str) -> bool {
    let Ok(file) = File::open(filename) else {
        return false;
    };
    let mut reader = LineReader {
        file,
        filename: filename.to_owned(),
        end_of_file: false,
        data: Vec::new(),
        position: 0,
        total_read: 0,
    };
    let mut result: Vec<u8> = Vec::new();
    while let Some(line) = reader.next_line() {
        if result.is_empty() && line.first() == Some(&b'.') {
            add(&line);
            continue;
        }
        result.extend_from_slice(&line);
        if crate::shell::sql_is_complete(&String::from_utf8_lossy(&result)) {
            add(&result);
            result.clear();
            continue;
        }
        result.extend_from_slice(b"\r\n");
    }
    history().file = Some(filename.to_owned());
    true
}
