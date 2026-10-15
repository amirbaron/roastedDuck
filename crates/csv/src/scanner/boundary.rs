//! Where a scanner reads (DuckDB's `scanner_boundary.hpp/.cpp`) and line positions for
//! error messages (`LinePosition`, `FullLinePosition` of `base_scanner.hpp` and
//! `string_value_scanner.hpp`).

use std::collections::HashMap;

use crate::buffer::{CsvBufferHandle, CsvBufferManager};
use crate::options::{CsvReaderOptions, ROWS_PER_BUFFER};

/// A boundary of the scan (DuckDB's `CSVBoundary`).
#[derive(Clone, Copy, Debug)]
pub struct CsvBoundary {
    pub buffer_idx: u64,
    pub buffer_pos: u64,
    pub boundary_idx: u64,
    pub end_pos: u64,
}

impl Default for CsvBoundary {
    fn default() -> Self {
        Self {
            buffer_idx: 0,
            buffer_pos: 0,
            boundary_idx: 0,
            end_pos: u64::MAX,
        }
    }
}

/// A position in a file: buffer and offset in it (DuckDB's `CSVPosition`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CsvPosition {
    pub buffer_idx: u64,
    pub buffer_pos: u64,
}

/// The part of a file a scanner reads (DuckDB's `CSVIterator`).
#[derive(Clone, Copy, Debug)]
pub struct CsvIterator {
    pub pos: CsvPosition,
    pub done: bool,
    pub first_one: bool,
    pub buffer_size: u64,
    boundary: CsvBoundary,
    is_set: bool,
}

impl Default for CsvIterator {
    fn default() -> Self {
        Self {
            pos: CsvPosition::default(),
            done: false,
            first_one: true,
            buffer_size: 0,
            boundary: CsvBoundary::default(),
            is_set: false,
        }
    }
}

/// `CSVIterator::ROWS_PER_THREAD`.
const ROWS_PER_THREAD: u64 = 4;

impl CsvIterator {
    /// `BytesPerThread`.
    pub fn bytes_per_thread(options: &CsvReaderOptions) -> u64 {
        let buffer_size = options.buffer_size_option.value();
        let max_row_size = options.maximum_line_size.value();
        let bytes_per_thread = buffer_size / ROWS_PER_BUFFER * ROWS_PER_THREAD;
        if bytes_per_thread < max_row_size {
            return max_row_size;
        }
        if bytes_per_thread == 0 {
            return 1;
        }
        bytes_per_thread
    }

    /// `Next`: moves to the next boundary; `false` when the file is done.
    pub fn next(
        &mut self,
        buffer_manager: &CsvBufferManager,
        options: &CsvReaderOptions,
    ) -> rustdb_common::Result<bool> {
        if !self.is_set {
            return Ok(false);
        }
        let bytes_per_thread = Self::bytes_per_thread(options);
        self.first_one = false;
        self.boundary.boundary_idx += 1;
        let Some(buffer) = buffer_manager.get_buffer(self.boundary.buffer_idx)? else {
            return Ok(false);
        };
        if buffer.is_last_buffer && self.boundary.buffer_pos + bytes_per_thread > buffer.actual_size() {
            return Ok(false);
        } else if self.boundary.buffer_pos + bytes_per_thread >= buffer.actual_size() {
            self.boundary.buffer_idx += 1;
            self.boundary.buffer_pos = 0;
            if buffer_manager.get_buffer(self.boundary.buffer_idx)?.is_none() {
                return Ok(false);
            }
        } else {
            self.boundary.buffer_pos += bytes_per_thread;
        }
        self.boundary.end_pos = self.boundary.buffer_pos + bytes_per_thread;
        self.set_current_position_to_boundary();
        Ok(true)
    }

    pub fn is_boundary_set(&self) -> bool {
        self.is_set
    }

    pub fn end_pos(&self) -> u64 {
        self.boundary.end_pos
    }

    /// `GetBufferIdx`.
    pub fn buffer_idx(&self) -> u64 {
        self.pos.buffer_idx
    }

    pub fn boundary_idx(&self) -> u64 {
        self.boundary.boundary_idx
    }

    pub fn set_current_position_to_boundary(&mut self) {
        self.pos.buffer_idx = self.boundary.buffer_idx;
        self.pos.buffer_pos = self.boundary.buffer_pos;
    }

    /// `SetCurrentBoundaryToPosition`.
    pub fn set_current_boundary_to_position(&mut self, single_threaded: bool, options: &CsvReaderOptions) {
        if single_threaded {
            self.is_set = false;
            return;
        }
        let bytes_per_thread = Self::bytes_per_thread(options);
        self.boundary.buffer_idx = self.pos.buffer_idx;
        if self.pos.buffer_pos == 0 {
            self.boundary.end_pos = bytes_per_thread;
        } else {
            self.boundary.end_pos = self.pos.buffer_pos.div_ceil(bytes_per_thread) * bytes_per_thread;
        }
        self.boundary.buffer_pos = self.boundary.end_pos - bytes_per_thread;
        self.is_set = true;
    }

    pub fn set_start(&mut self, start: u64) {
        self.boundary.buffer_pos = start;
    }

    pub fn set_end(&mut self, end: u64) {
        self.boundary.end_pos = end;
    }

    /// `GetGlobalCurrentPos`.
    pub fn global_current_pos(&self) -> u64 {
        self.pos.buffer_pos + self.buffer_size * self.pos.buffer_idx
    }

    /// `CheckIfDone`.
    pub fn check_if_done(&mut self) {
        if self.is_boundary_set()
            && (self.pos.buffer_idx > self.boundary.buffer_idx
                || self.pos.buffer_pos > self.boundary.buffer_pos)
        {
            self.done = true;
        }
    }
}

/// The start of a line, for line sizes and error positions (DuckDB's `LinePosition`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LinePosition {
    pub buffer_pos: u64,
    pub buffer_size: u64,
    pub buffer_idx: u64,
}

impl LinePosition {
    pub fn new(buffer_idx: u64, buffer_pos: u64, buffer_size: u64) -> Self {
        Self {
            buffer_pos,
            buffer_size,
            buffer_idx,
        }
    }

    /// `operator-`: the bytes between two positions.
    pub fn distance_from(&self, other: &Self) -> u64 {
        if other.buffer_idx == self.buffer_idx {
            return self.buffer_pos.wrapping_sub(other.buffer_pos);
        }
        other
            .buffer_size
            .wrapping_sub(other.buffer_pos)
            .wrapping_add(self.buffer_pos)
    }

    /// `GetGlobalPosition`.
    pub fn global_position(&self, requested_buffer_size: u64, first_char_nl: bool) -> u64 {
        requested_buffer_size * self.buffer_idx + self.buffer_pos + u64::from(first_char_nl)
    }
}

/// The start and end of a line (DuckDB's `FullLinePosition`).
#[derive(Clone, Copy, Debug, Default)]
pub struct FullLinePosition {
    pub begin: LinePosition,
    pub end: LinePosition,
}

/// `FullLinePosition::SanitizeError` (`Utf8Proc::MakeValid`): the text with the bytes of
/// invalid UTF-8 sequences replaced by `?`.
pub fn sanitize_error(bytes: &[u8]) -> String {
    let mut result = String::with_capacity(bytes.len());
    let mut rest = bytes;
    loop {
        match std::str::from_utf8(rest) {
            Ok(valid) => {
                result.push_str(valid);
                return result;
            }
            Err(error) => {
                let valid = error.valid_up_to();
                result.push_str(std::str::from_utf8(&rest[..valid]).unwrap_or_default());
                let invalid = error.error_len().unwrap_or(rest.len() - valid);
                for _ in 0..invalid {
                    result.push('?');
                }
                rest = &rest[valid + invalid..];
            }
        }
    }
}

impl FullLinePosition {
    /// `ReconstructCurrentLine`: the text of the line, for error messages.
    pub fn reconstruct_current_line(
        &self,
        first_char_nl: &mut bool,
        buffer_handles: &HashMap<u64, CsvBufferHandle>,
        reconstruct_line: bool,
    ) -> String {
        if !reconstruct_line || self.begin == self.end {
            return String::new();
        }
        let mut result: Vec<u8> = Vec::new();
        if self.end.buffer_idx == self.begin.buffer_idx || self.begin.buffer_pos == self.begin.buffer_size {
            let Some(buffer) = buffer_handles.get(&self.end.buffer_idx) else {
                return String::new();
            };
            let data = buffer.data();
            let start_pos = if self.begin.buffer_pos == self.begin.buffer_size {
                0
            } else {
                self.begin.buffer_pos as usize
            };
            *first_char_nl = matches!(data.get(start_pos), Some(b'\n' | b'\r'));
            let from = start_pos + usize::from(*first_char_nl);
            let to = (self.end.buffer_pos as usize).min(data.len());
            if from < to {
                result.extend_from_slice(&data[from..to]);
            }
        } else {
            let (Some(first), Some(second)) = (
                buffer_handles.get(&self.begin.buffer_idx),
                buffer_handles.get(&self.end.buffer_idx),
            ) else {
                return String::new();
            };
            let first_data = first.data();
            let begin = self.begin.buffer_pos as usize;
            if begin >= first_data.len() {
                return String::new();
            }
            *first_char_nl = matches!(first_data[begin], b'\n' | b'\r');
            result.extend_from_slice(&first_data[begin + usize::from(*first_char_nl)..]);
            let end = (self.end.buffer_pos as usize).min(second.data().len());
            result.extend_from_slice(&second.data()[..end]);
        }
        sanitize_error(&result)
    }
}
