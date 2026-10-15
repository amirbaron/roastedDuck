//! The scanner skeleton (DuckDB's `base_scanner.hpp/.cpp`): the per-byte state machine
//! loop, generic over what a scanner does with values, rows and special states.

use std::sync::Arc;

use rustdb_common::Result;

use super::boundary::{CsvIterator, LinePosition};
use crate::buffer::{CsvBufferHandle, CsvBufferManager};
use crate::error::CsvErrorHandler;
use crate::options::NewLineIdentifier;
use crate::state_machine::{CsvState, CsvStateMachine, CsvStates, contains_zero_byte};

/// The state every scanner has (DuckDB's `BaseScanner` members).
#[derive(Debug)]
#[allow(clippy::struct_excessive_bools)]
pub struct BaseScanner {
    pub buffer_manager: Arc<CsvBufferManager>,
    pub state_machine: Arc<CsvStateMachine>,
    pub error_handler: Arc<CsvErrorHandler>,
    /// Whether this scanner sniffs.
    pub sniffing: bool,
    pub states: CsvStates,
    pub ever_quoted: bool,
    pub ever_escaped: bool,
    pub used_unstrictness: bool,
    pub iterator: CsvIterator,
    pub cur_buffer_handle: Option<CsvBufferHandle>,
    pub initialized: bool,
    pub lines_read: u64,
    pub bytes_read: u64,
}

impl BaseScanner {
    /// `BaseScanner::BaseScanner`: pins the first buffer of the iterator.
    pub fn new(
        buffer_manager: Arc<CsvBufferManager>,
        state_machine: Arc<CsvStateMachine>,
        error_handler: Arc<CsvErrorHandler>,
        sniffing: bool,
        iterator: CsvIterator,
    ) -> Result<Self> {
        let cur_buffer_handle = buffer_manager.get_buffer(iterator.buffer_idx())?;
        Ok(Self {
            buffer_manager,
            state_machine,
            error_handler,
            sniffing,
            states: CsvStates::default(),
            ever_quoted: false,
            ever_escaped: false,
            used_unstrictness: false,
            iterator,
            cur_buffer_handle,
            initialized: false,
            lines_read: 0,
            bytes_read: 0,
        })
    }

    /// `FinishedFile`.
    pub fn finished_file(&self) -> bool {
        let Some(handle) = &self.cur_buffer_handle else {
            return true;
        };
        if !self.buffer_manager.done() {
            return false;
        }
        if self.iterator.pos.buffer_idx != self.buffer_manager.buffer_count() {
            return false;
        }
        self.iterator.pos.buffer_pos + 1 == handle.actual_size()
    }

    /// `RemoveSeparator`.
    pub fn remove_separator(value: &[u8], separator: u8) -> Vec<u8> {
        value.iter().copied().filter(|&byte| byte != separator).collect()
    }

    pub fn current_buffer_size(&self) -> u64 {
        self.cur_buffer_handle
            .as_ref()
            .map_or(0, |handle| handle.actual_size())
    }

    pub fn line_position(&self) -> LinePosition {
        LinePosition::new(
            self.iterator.pos.buffer_idx,
            self.iterator.pos.buffer_pos,
            self.current_buffer_size(),
        )
    }

    pub fn new_line(&self) -> NewLineIdentifier {
        self.state_machine
            .dialect_options
            .state_machine_options
            .new_line
            .value()
    }
}

/// Reads eight bytes at `pos` as a word (DuckDB's `Load<uint64_t>`).
#[inline]
fn load_word(buffer: &[u8], pos: usize) -> u64 {
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&buffer[pos..pos + 8]);
    u64::from_ne_bytes(bytes)
}

/// What a scanner does with the events of the state machine (the static functions of
/// DuckDB's `ScannerResult` subclasses), and the loop driving it (`BaseScanner::Process`).
pub trait CsvProcess {
    fn base(&self) -> &BaseScanner;
    fn base_mut(&mut self) -> &mut BaseScanner;

    fn invalid_state(&mut self) -> Result<()>;
    /// Returns whether the result is full.
    fn empty_line(&mut self, buffer_pos: u64) -> Result<bool>;
    fn is_comment_set(&self) -> bool;
    /// Returns whether the result is full.
    fn unset_comment(&mut self, buffer_pos: u64) -> Result<bool>;
    /// Returns whether the result is full.
    fn add_row(&mut self, buffer_pos: u64) -> Result<bool>;
    fn add_value(&mut self, buffer_pos: u64) -> Result<()>;
    fn set_escaped(&mut self);
    fn set_quoted(&mut self, buffer_pos: u64);
    fn set_unquoted(&mut self);
    fn quoted_new_line(&mut self);
    fn set_comment(&mut self, buffer_pos: u64);

    /// `Process`: transitions over the current buffer up to the boundary, calling the
    /// handlers; returns early when the result is full.
    #[allow(clippy::too_many_lines)]
    fn process(&mut self) -> Result<()> {
        let Some(handle) = self.base().cur_buffer_handle.clone() else {
            return Ok(());
        };
        let buffer = handle.data();
        let (has_escaped_value, only_rn_newlines, quote, strict, transitions) = {
            let base = self.base();
            let sm = &base.state_machine;
            let options = &sm.state_machine_options;
            (
                sm.dialect_options.state_machine_options.escape.value() != 0,
                options.strict_mode.value()
                    && options.strict_mode.is_set_by_user()
                    && options.new_line.value() == NewLineIdentifier::CarryOn
                    && options.new_line.is_set_by_user(),
                sm.dialect_options.state_machine_options.quote.value(),
                sm.dialect_options.state_machine_options.strict_mode.value(),
                Arc::clone(&sm.transition_array),
            )
        };
        let start_pos = self.base().iterator.pos.buffer_pos;
        let actual_size = handle.actual_size();
        let to_pos = if self.base().iterator.is_boundary_set() {
            self.base().iterator.end_pos().min(actual_size)
        } else {
            actual_size
        };
        macro_rules! pos {
            () => {
                self.base_mut().iterator.pos.buffer_pos
            };
        }
        macro_rules! finish_early {
            () => {{
                pos!() += 1;
                let base = self.base_mut();
                base.bytes_read = base.iterator.pos.buffer_pos - start_pos;
                base.lines_read += 1;
                return Ok(());
            }};
        }
        while self.base().iterator.pos.buffer_pos < to_pos {
            let current = self.base().iterator.pos.buffer_pos;
            let byte = buffer[current as usize];
            {
                let base = self.base_mut();
                base.states.states[0] = base.states.states[1];
                base.states.states[1] = transitions.get(byte, base.states.states[1]);
            }
            let states = self.base().states;
            match states.states[1] {
                CsvState::Invalid => {
                    self.invalid_state()?;
                    pos!() += 1;
                    let base = self.base_mut();
                    base.bytes_read = base.iterator.pos.buffer_pos - start_pos;
                    return Ok(());
                }
                CsvState::RecordSeparator => {
                    if matches!(states.states[0], CsvState::RecordSeparator | CsvState::NotSet) {
                        if self.empty_line(current)? {
                            finish_early!();
                        }
                        self.base_mut().lines_read += 1;
                    } else if states.states[0] != CsvState::CarriageReturn {
                        if self.is_comment_set() {
                            if self.unset_comment(current)? {
                                finish_early!();
                            }
                        } else if self.add_row(current)? {
                            finish_early!();
                        }
                        self.base_mut().lines_read += 1;
                    }
                    pos!() += 1;
                }
                CsvState::CarriageReturn => {
                    if matches!(states.states[0], CsvState::RecordSeparator | CsvState::NotSet) {
                        if self.empty_line(current)? {
                            finish_early!();
                        }
                    } else if states.states[0] != CsvState::CarriageReturn {
                        if self.is_comment_set() {
                            if self.unset_comment(current)? {
                                finish_early!();
                            }
                        } else if !only_rn_newlines && self.add_row(current)? {
                            finish_early!();
                        }
                    }
                    pos!() += 1;
                    self.base_mut().lines_read += 1;
                }
                CsvState::Delimiter => {
                    self.add_value(current)?;
                    pos!() += 1;
                }
                CsvState::Quoted => {
                    if matches!(states.states[0], CsvState::Unquoted | CsvState::MaybeQuoted)
                        && has_escaped_value
                    {
                        self.base_mut().ever_escaped = true;
                        self.set_escaped();
                    }
                    if matches!(
                        states.states[0],
                        CsvState::Escape | CsvState::EscapedReturn | CsvState::UnquotedEscape
                    ) && (byte == quote || !strict)
                    {
                        self.base_mut().ever_escaped = true;
                        if states.states[0] == CsvState::UnquotedEscape {
                            self.base_mut().used_unstrictness = true;
                        }
                    }
                    self.base_mut().ever_quoted = true;
                    self.set_quoted(current);
                    pos!() += 1;
                    let mut position = self.base().iterator.pos.buffer_pos;
                    while position + 8 < to_pos {
                        let value = load_word(buffer, position as usize);
                        if contains_zero_byte((value ^ transitions.quote) & (value ^ transitions.escape)) {
                            break;
                        }
                        position += 8;
                    }
                    while position + 1 < to_pos && transitions.skip_quoted[buffer[position as usize] as usize]
                    {
                        position += 1;
                    }
                    pos!() = position;
                }
                CsvState::Unquoted => {
                    if states.states[0] == CsvState::MaybeQuoted {
                        self.base_mut().ever_escaped = true;
                        self.set_escaped();
                    }
                    self.set_unquoted();
                    pos!() += 1;
                }
                CsvState::Escape | CsvState::EscapedReturn => {
                    self.set_escaped();
                    pos!() += 1;
                }
                CsvState::UnquotedEscape => {
                    self.set_escaped();
                    pos!() += 1;
                    self.base_mut().used_unstrictness = true;
                }
                CsvState::Standard => {
                    let mut position = current + 1;
                    while position + 8 < to_pos {
                        let value = load_word(buffer, position as usize);
                        if contains_zero_byte(
                            (value ^ transitions.delimiter)
                                & (value ^ transitions.new_line)
                                & (value ^ transitions.carriage_return)
                                & (value ^ transitions.escape)
                                & (value ^ transitions.comment),
                        ) {
                            break;
                        }
                        position += 8;
                    }
                    while position + 1 < to_pos
                        && transitions.skip_standard[buffer[position as usize] as usize]
                    {
                        position += 1;
                    }
                    pos!() = position;
                }
                CsvState::QuotedNewLine => {
                    self.quoted_new_line();
                    pos!() += 1;
                }
                CsvState::Comment => {
                    self.set_comment(current);
                    let mut position = current + 1;
                    while position + 8 < to_pos {
                        let value = load_word(buffer, position as usize);
                        if contains_zero_byte(
                            (value ^ transitions.new_line) & (value ^ transitions.carriage_return),
                        ) {
                            break;
                        }
                        position += 8;
                    }
                    while position + 1 < to_pos
                        && transitions.skip_comment[buffer[position as usize] as usize]
                    {
                        position += 1;
                    }
                    pos!() = position;
                }
                _ => {
                    pos!() += 1;
                }
            }
        }
        let base = self.base_mut();
        base.bytes_read = base.iterator.pos.buffer_pos - start_pos;
        Ok(())
    }
}

/// Transitions `states` with one byte (`CSVStateMachine::Transition`).
#[inline]
pub fn transition(state_machine: &CsvStateMachine, states: &mut CsvStates, byte: u8) {
    state_machine.transition(states, byte);
}
