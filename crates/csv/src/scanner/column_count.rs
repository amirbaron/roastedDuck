//! Counting the columns of rows for dialect sniffing (DuckDB's
//! `column_count_scanner.hpp/.cpp`).

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use roastedduck_common::Result;

use super::base::{BaseScanner, CsvProcess};
use super::boundary::{CsvIterator, FullLinePosition, LinePosition};
use super::skip::skip_csv_rows;
use super::string_value::StringValueScanner;
use crate::buffer::{CsvBufferHandle, CsvBufferManager};
use crate::error::{CsvError, CsvErrorHandler, LinesPerBoundary};
use crate::state_machine::{CsvState, CsvStateMachine};

/// The columns of one sniffed row (DuckDB's `ColumnCount`).
#[derive(Clone, Copy, Debug)]
pub struct ColumnCount {
    pub number_of_columns: u64,
    /// Whether the last value of this row and every later row is empty.
    pub last_value_always_empty: bool,
    pub is_comment: bool,
    pub is_mid_comment: bool,
    /// Empty lines before this row.
    pub empty_lines: u64,
}

impl Default for ColumnCount {
    fn default() -> Self {
        Self {
            number_of_columns: 0,
            last_value_always_empty: true,
            is_comment: false,
            is_mid_comment: false,
            empty_lines: 0,
        }
    }
}

/// The column counts of a chunk of rows (DuckDB's `ColumnCountScanner` with
/// `ColumnCountResult`).
#[derive(Debug)]
pub struct ColumnCountScanner {
    pub base: BaseScanner,
    pub column_counts: Vec<ColumnCount>,
    current_column_count: u64,
    pub error: bool,
    pub result_position: u64,
    cur_buffer_idx: u64,
    current_buffer_size: u64,
    /// How many rows have each column count.
    pub rows_per_column_count: BTreeMap<u64, u64>,
    buffer_handles: HashMap<u64, CsvBufferHandle>,
    empty_lines: u64,
    comment: bool,
    cur_line_starts_as_comment: bool,
    last_position: LinePosition,
    pub result_size: u64,
}

impl ColumnCountScanner {
    pub fn new(
        buffer_manager: Arc<CsvBufferManager>,
        state_machine: Arc<CsvStateMachine>,
        error_handler: Arc<CsvErrorHandler>,
        result_size: u64,
        iterator: CsvIterator,
    ) -> Result<Self> {
        let base = BaseScanner::new(buffer_manager, state_machine, error_handler, true, iterator)?;
        let mut buffer_handles = HashMap::new();
        let mut actual_size = 0;
        if let Some(handle) = &base.cur_buffer_handle {
            actual_size = handle.actual_size();
            buffer_handles.insert(0, Arc::clone(handle));
        }
        Ok(Self {
            last_position: LinePosition::new(iterator.pos.buffer_idx, iterator.pos.buffer_pos, actual_size),
            base,
            column_counts: vec![ColumnCount::default(); result_size as usize],
            current_column_count: 0,
            error: false,
            result_position: 0,
            cur_buffer_idx: iterator.pos.buffer_idx,
            current_buffer_size: actual_size,
            rows_per_column_count: BTreeMap::new(),
            buffer_handles,
            empty_lines: 0,
            comment: false,
            cur_line_starts_as_comment: false,
            result_size,
        })
    }

    /// `UpgradeToStringValueScanner`: a sniffing scanner over the same file and dialect,
    /// positioned after the rows to skip.
    pub fn upgrade_to_string_value_scanner(&self) -> Result<StringValueScanner> {
        let rows_to_skip = self.base.state_machine.dialect_options.skip_rows.value();
        let mut iterator = skip_csv_rows(&self.base.buffer_manager, &self.base.state_machine, rows_to_skip)?;
        if iterator.done {
            iterator = CsvIterator::default();
        }
        StringValueScanner::new(
            0,
            Arc::clone(&self.base.buffer_manager),
            Arc::clone(&self.base.state_machine),
            Arc::clone(&self.base.error_handler),
            None,
            true,
            iterator,
            self.result_size,
        )
    }

    /// The state machine, to change its dialect while sniffing.
    pub fn state_machine_mut(&mut self) -> &mut crate::state_machine::CsvStateMachine {
        Arc::make_mut(&mut self.base.state_machine)
    }

    /// `GetMostFrequentColumnCount`: ties go to the larger count.
    pub fn most_frequent_column_count(&self) -> u64 {
        if self.rows_per_column_count.is_empty() {
            return 1;
        }
        let mut column_count = 0;
        let mut current_max = 0;
        for (&count, &rows) in &self.rows_per_column_count {
            if rows > current_max || (rows == current_max && count > column_count) {
                current_max = rows;
                column_count = count;
            }
        }
        column_count
    }

    /// `ParseChunk`.
    pub fn parse_chunk(&mut self) -> Result<()> {
        self.result_position = 0;
        if let Some(handle) = &self.base.cur_buffer_handle {
            self.current_buffer_size = handle.actual_size();
        }
        if self.base.iterator.done {
            return Ok(());
        }
        if !self.base.initialized {
            self.base.states.initialize(CsvState::NotSet);
            self.base.initialized = true;
        }
        if !self.base.iterator.done && self.base.cur_buffer_handle.is_some() {
            self.process()?;
        }
        self.finalize_chunk_process()
    }

    fn internal_add_row(&mut self) {
        let column_count = self.current_column_count + 1;
        let position = self.result_position as usize;
        self.column_counts[position].number_of_columns = column_count;
        self.column_counts[position].empty_lines = self.empty_lines;
        *self.rows_per_column_count.entry(column_count).or_insert(0) += 1;
        self.current_column_count = 0;
    }

    fn line_size_error(&mut self, end: LinePosition) -> Result<()> {
        let options = &self.base.state_machine.options;
        let current_line_position = FullLinePosition {
            begin: self.last_position,
            end,
        };
        let mut mock = false;
        let csv_row = current_line_position.reconstruct_current_line(&mut mock, &self.buffer_handles, true);
        let error = CsvError::line_size_error(
            options,
            LinesPerBoundary::default(),
            csv_row,
            self.last_position
                .global_position(options.buffer_size_option.value(), false),
            &options.file_path,
        );
        self.base.error_handler.error(error, false)?;
        self.error = true;
        Ok(())
    }

    /// `ColumnCountResult::AddRow`.
    fn add_row_internal(&mut self, buffer_pos: u64) -> Result<bool> {
        let cur_position = LinePosition::new(
            self.cur_buffer_idx,
            buffer_pos.wrapping_add(1),
            self.current_buffer_size,
        );
        if cur_position.distance_from(&self.last_position)
            > self.base.state_machine.options.maximum_line_size.value()
            && buffer_pos != u64::MAX
        {
            self.line_size_error(cur_position)?;
        }
        self.internal_add_row();
        self.last_position = cur_position;
        if !self.base.states.empty_last_value() {
            let mut index = self.result_position as i64;
            for _ in 0..=self.result_position {
                if index < 0 || !self.column_counts[index as usize].last_value_always_empty {
                    break;
                }
                self.column_counts[index as usize].last_value_always_empty = false;
                index -= 1;
            }
        }
        self.result_position += 1;
        Ok(self.result_position >= self.result_size)
    }

    /// `FinalizeChunkProcess`.
    fn finalize_chunk_process(&mut self) -> Result<()> {
        if self.result_position == self.result_size || self.error {
            return Ok(());
        }
        while !self.base.finished_file() && self.result_position < self.result_size && !self.error {
            let at_end = self
                .base
                .cur_buffer_handle
                .as_ref()
                .is_some_and(|handle| self.base.iterator.pos.buffer_pos == handle.actual_size());
            if at_end {
                self.base.iterator.pos.buffer_idx += 1;
                self.base.cur_buffer_handle = self
                    .base
                    .buffer_manager
                    .get_buffer(self.base.iterator.pos.buffer_idx)?;
                let Some(handle) = self.base.cur_buffer_handle.clone() else {
                    let states = self.base.states;
                    if states.is_quoted_current() && !states.is_unquoted() {
                        self.error = true;
                        return Ok(());
                    }
                    if states.empty_line()
                        || states.new_row()
                        || states.is_current_new_row()
                        || states.is_not_set()
                    {
                        return Ok(());
                    }
                    if self.comment {
                        self.unset_comment(u64::MAX)?;
                    } else {
                        self.add_row_internal(u64::MAX)?;
                    }
                    return Ok(());
                };
                self.buffer_handles
                    .insert(self.base.iterator.pos.buffer_idx, Arc::clone(&handle));
                self.cur_buffer_idx = self.base.iterator.pos.buffer_idx;
                self.current_buffer_size = handle.actual_size();
                let cur_position = LinePosition::new(self.cur_buffer_idx, 0, self.current_buffer_size);
                if cur_position.distance_from(&self.last_position)
                    > self.base.state_machine.options.maximum_line_size.value()
                {
                    self.line_size_error(cur_position)?;
                    return Ok(());
                }
                if self.buffer_handles.len() > 2
                    && let Some(&lowest) = self.buffer_handles.keys().min()
                {
                    self.buffer_handles.remove(&lowest);
                }
                self.base.iterator.pos.buffer_pos = 0;
            }
            self.process()?;
        }
        Ok(())
    }
}

impl CsvProcess for ColumnCountScanner {
    fn base(&self) -> &BaseScanner {
        &self.base
    }

    fn base_mut(&mut self) -> &mut BaseScanner {
        &mut self.base
    }

    fn invalid_state(&mut self) -> Result<()> {
        self.result_position = 0;
        self.error = true;
        Ok(())
    }

    fn empty_line(&mut self, _: u64) -> Result<bool> {
        self.empty_lines += 1;
        Ok(false)
    }

    fn is_comment_set(&self) -> bool {
        self.comment
    }

    fn unset_comment(&mut self, buffer_pos: u64) -> Result<bool> {
        let done = self.add_row_internal(buffer_pos)?;
        let index = (self.result_position - 1) as usize;
        if self.cur_line_starts_as_comment {
            self.column_counts[index].is_comment = true;
        } else {
            self.column_counts[index].is_mid_comment = true;
        }
        self.comment = false;
        self.cur_line_starts_as_comment = false;
        Ok(done)
    }

    fn add_row(&mut self, buffer_pos: u64) -> Result<bool> {
        self.add_row_internal(buffer_pos)
    }

    fn add_value(&mut self, _: u64) -> Result<()> {
        self.current_column_count += 1;
        Ok(())
    }

    fn set_escaped(&mut self) {}

    fn set_quoted(&mut self, _: u64) {}

    fn set_unquoted(&mut self) {}

    fn quoted_new_line(&mut self) {}

    fn set_comment(&mut self, _: u64) {
        let states = self.base.states;
        if !states.was_standard() && !states.was_state(CsvState::Delimiter) {
            self.cur_line_starts_as_comment = true;
        }
        self.comment = true;
    }
}
