//! Skipping the first rows of a file (DuckDB's `skip_scanner.hpp/.cpp` and
//! `BaseScanner::SkipCSVRows`).

use std::sync::Arc;

use rustdb_common::Result;

use super::base::{BaseScanner, CsvProcess};
use super::boundary::CsvIterator;
use crate::buffer::CsvBufferManager;
use crate::error::CsvErrorHandler;
use crate::options::NewLineIdentifier;
use crate::state_machine::{CsvState, CsvStateMachine};

/// Counts rows until enough are skipped (DuckDB's `SkipScanner` with `SkipResult`).
#[derive(Debug)]
pub struct SkipScanner {
    base: BaseScanner,
    row_count: u64,
    rows_to_skip: u64,
    comment: bool,
    cur_line_starts_as_comment: bool,
}

impl SkipScanner {
    pub fn new(
        buffer_manager: Arc<CsvBufferManager>,
        state_machine: Arc<CsvStateMachine>,
        error_handler: Arc<CsvErrorHandler>,
        rows_to_skip: u64,
    ) -> Result<Self> {
        Ok(Self {
            base: BaseScanner::new(
                buffer_manager,
                state_machine,
                error_handler,
                false,
                CsvIterator::default(),
            )?,
            row_count: 0,
            rows_to_skip,
            comment: false,
            cur_line_starts_as_comment: false,
        })
    }

    /// `ParseChunk`.
    pub fn parse_chunk(&mut self) -> Result<()> {
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

    /// `FinalizeChunkProcess`.
    fn finalize_chunk_process(&mut self) -> Result<()> {
        while !self.base.finished_file() && self.row_count < self.rows_to_skip {
            self.base.iterator.pos.buffer_idx += 1;
            self.base.cur_buffer_handle = self
                .base
                .buffer_manager
                .get_buffer(self.base.iterator.pos.buffer_idx)?;
            if self.base.cur_buffer_handle.is_some() {
                self.base.iterator.pos.buffer_pos = 0;
                self.process()?;
            }
        }
        if self
            .base
            .state_machine
            .options
            .dialect_options
            .state_machine_options
            .new_line
            .value()
            == NewLineIdentifier::CarryOn
            && self.base.states.states[1] == CsvState::CarriageReturn
        {
            self.base.iterator.pos.buffer_pos += 1;
        }
        self.base.iterator.done = self.base.finished_file();
        Ok(())
    }

    pub fn iterator(&self) -> CsvIterator {
        self.base.iterator
    }

    pub fn lines_read(&self) -> u64 {
        self.base.lines_read
    }

    fn internal_add_row(&mut self) -> bool {
        self.row_count += 1;
        self.row_count >= self.rows_to_skip
    }
}

impl CsvProcess for SkipScanner {
    fn base(&self) -> &BaseScanner {
        &self.base
    }

    fn base_mut(&mut self) -> &mut BaseScanner {
        &mut self.base
    }

    fn invalid_state(&mut self) -> Result<()> {
        Ok(())
    }

    fn empty_line(&mut self, _: u64) -> Result<bool> {
        Ok(self.internal_add_row())
    }

    fn is_comment_set(&self) -> bool {
        self.comment
    }

    fn unset_comment(&mut self, _: u64) -> Result<bool> {
        let mut done = false;
        if !self.cur_line_starts_as_comment {
            done = self.internal_add_row();
        }
        self.comment = false;
        self.cur_line_starts_as_comment = false;
        Ok(done)
    }

    fn add_row(&mut self, _: u64) -> Result<bool> {
        Ok(self.internal_add_row())
    }

    fn add_value(&mut self, _: u64) -> Result<()> {
        Ok(())
    }

    fn set_escaped(&mut self) {}

    fn set_quoted(&mut self, _: u64) {}

    fn set_unquoted(&mut self) {}

    fn quoted_new_line(&mut self) {}

    fn set_comment(&mut self, _: u64) {
        if !self.base.states.was_standard() {
            self.cur_line_starts_as_comment = true;
        }
        self.comment = true;
    }
}

/// `BaseScanner::SkipCSVRows`: the iterator after `rows_to_skip` rows.
pub fn skip_csv_rows(
    buffer_manager: &Arc<CsvBufferManager>,
    state_machine: &Arc<CsvStateMachine>,
    rows_to_skip: u64,
) -> Result<CsvIterator> {
    if rows_to_skip == 0 {
        return Ok(CsvIterator::default());
    }
    let mut skipper = SkipScanner::new(
        Arc::clone(buffer_manager),
        Arc::clone(state_machine),
        Arc::new(CsvErrorHandler::default()),
        rows_to_skip,
    )?;
    skipper.parse_chunk()?;
    Ok(skipper.iterator())
}
