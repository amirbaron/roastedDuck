//! The CSV state machine (DuckDB's `csv_state.hpp`, `csv_state_machine.hpp/.cpp` and
//! `csv_state_machine_cache.hpp/.cpp`): a transition table over every byte and state,
//! built once per dialect and cached.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use crate::options::{CsvReaderOptions, DialectOptions, NewLineIdentifier, StateMachineOptions};

/// A state of the CSV parser (DuckDB's `CSVState`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
#[repr(u8)]
pub enum CsvState {
    /// Regular unquoted field state.
    #[default]
    Standard = 0,
    /// After a field separator (the last delimiter byte).
    Delimiter = 1,
    DelimiterFirstByte = 2,
    DelimiterSecondByte = 3,
    DelimiterThirdByte = 4,
    /// After a record separator (`\n`).
    RecordSeparator = 5,
    /// After a carriage return (`\r`).
    CarriageReturn = 6,
    /// Inside a quoted field.
    Quoted = 7,
    /// When leaving a quoted field.
    Unquoted = 8,
    /// After an escape character inside quotes.
    Escape = 9,
    Invalid = 10,
    /// Before the first character.
    NotSet = 11,
    QuotedNewLine = 12,
    EmptySpace = 13,
    Comment = 14,
    StandardNewline = 15,
    UnquotedEscape = 16,
    EscapedReturn = 17,
    MaybeQuoted = 18,
}

impl CsvState {
    const ALL: [Self; 19] = [
        Self::Standard,
        Self::Delimiter,
        Self::DelimiterFirstByte,
        Self::DelimiterSecondByte,
        Self::DelimiterThirdByte,
        Self::RecordSeparator,
        Self::CarriageReturn,
        Self::Quoted,
        Self::Unquoted,
        Self::Escape,
        Self::Invalid,
        Self::NotSet,
        Self::QuotedNewLine,
        Self::EmptySpace,
        Self::Comment,
        Self::StandardNewline,
        Self::UnquotedEscape,
        Self::EscapedReturn,
        Self::MaybeQuoted,
    ];
}

/// The current and previous state (DuckDB's `CSVStates`).
#[derive(Clone, Copy, Debug, Default)]
pub struct CsvStates {
    pub states: [CsvState; 2],
}

impl CsvStates {
    pub fn initialize(&mut self, initial: CsvState) {
        self.states = [initial, initial];
    }

    pub fn new_value(&self) -> bool {
        self.states[1] == CsvState::Delimiter
    }

    pub fn new_row(&self) -> bool {
        !matches!(
            self.states[0],
            CsvState::RecordSeparator | CsvState::CarriageReturn
        ) && matches!(
            self.states[1],
            CsvState::RecordSeparator | CsvState::CarriageReturn
        )
    }

    pub fn was_standard(&self) -> bool {
        self.states[0] == CsvState::Standard
    }

    pub fn empty_last_value(&self) -> bool {
        (self.states[0] == CsvState::Delimiter
            && matches!(
                self.states[1],
                CsvState::RecordSeparator | CsvState::CarriageReturn | CsvState::Delimiter
            ))
            || (self.states[0] == CsvState::Standard && self.states[1] == CsvState::Delimiter)
    }

    pub fn empty_line(&self) -> bool {
        matches!(
            self.states[1],
            CsvState::CarriageReturn | CsvState::RecordSeparator
        ) && matches!(self.states[0], CsvState::RecordSeparator | CsvState::NotSet)
    }

    pub fn is_delimiter_bytes(&self) -> bool {
        matches!(
            self.states[0],
            CsvState::DelimiterFirstByte | CsvState::DelimiterSecondByte | CsvState::DelimiterThirdByte
        )
    }

    pub fn is_delimiter(&self) -> bool {
        self.states[1] == CsvState::Delimiter
    }

    pub fn is_not_set(&self) -> bool {
        self.states[1] == CsvState::NotSet
    }

    pub fn is_comment(&self) -> bool {
        self.states[1] == CsvState::Comment
    }

    pub fn is_current_new_row(&self) -> bool {
        matches!(
            self.states[1],
            CsvState::RecordSeparator | CsvState::CarriageReturn
        )
    }

    pub fn is_carriage_return(&self) -> bool {
        self.states[1] == CsvState::CarriageReturn
    }

    pub fn is_invalid(&self) -> bool {
        self.states[1] == CsvState::Invalid
    }

    pub fn is_quoted(&self) -> bool {
        self.states[0] == CsvState::Quoted
    }

    pub fn is_unquoted(&self) -> bool {
        self.states[0] == CsvState::Unquoted
    }

    pub fn is_escaped(&self) -> bool {
        match self.states[1] {
            CsvState::Escape | CsvState::UnquotedEscape | CsvState::EscapedReturn => true,
            CsvState::Quoted => matches!(self.states[0], CsvState::Unquoted | CsvState::MaybeQuoted),
            CsvState::Unquoted => self.states[0] == CsvState::MaybeQuoted,
            _ => false,
        }
    }

    pub fn is_quoted_current(&self) -> bool {
        matches!(self.states[1], CsvState::Quoted | CsvState::QuotedNewLine)
    }

    pub fn is_state(&self, state: CsvState) -> bool {
        self.states[1] == state
    }

    pub fn was_state(&self, state: CsvState) -> bool {
        self.states[0] == state
    }
}

/// The transition table (DuckDB's `StateMachine`).
#[derive(Clone, Debug)]
pub struct StateMachine {
    table: Box<[[CsvState; StateMachine::NUM_STATES]; StateMachine::NUM_TRANSITIONS]>,
    /// Bytes skipped without a transition in the STANDARD state.
    pub skip_standard: [bool; 256],
    pub skip_quoted: [bool; 256],
    pub skip_comment: [bool; 256],
    /// The special bytes replicated over a word, for the eight-bytes-at-a-time skips.
    pub delimiter: u64,
    pub new_line: u64,
    pub carriage_return: u64,
    pub quote: u64,
    pub escape: u64,
    pub comment: u64,
}

impl StateMachine {
    pub const NUM_STATES: usize = 19;
    pub const NUM_TRANSITIONS: usize = 256;

    #[inline]
    pub fn get(&self, byte: u8, state: CsvState) -> CsvState {
        self.table[byte as usize][state as usize]
    }

    fn set(&mut self, byte: u8, state: CsvState, next: CsvState) {
        self.table[byte as usize][state as usize] = next;
    }

    fn initialize_column(&mut self, state: CsvState, next: CsvState) {
        for byte in 0..Self::NUM_TRANSITIONS {
            self.table[byte][state as usize] = next;
        }
    }

    /// `CSVStateMachineCache::Insert`.
    #[allow(clippy::too_many_lines)]
    pub fn build(options: &StateMachineOptions) -> Self {
        let mut sm = Self {
            table: Box::new([[CsvState::Standard; Self::NUM_STATES]; Self::NUM_TRANSITIONS]),
            skip_standard: [true; 256],
            skip_quoted: [true; 256],
            skip_comment: [true; 256],
            delimiter: 0,
            new_line: 0,
            carriage_return: 0,
            quote: 0,
            escape: 0,
            comment: 0,
        };
        let strict = options.strict_mode.value();
        for state in CsvState::ALL {
            let next = match state {
                CsvState::MaybeQuoted | CsvState::Quoted | CsvState::QuotedNewLine | CsvState::Escape => {
                    CsvState::Quoted
                }
                CsvState::Unquoted => {
                    if strict {
                        CsvState::Invalid
                    } else {
                        CsvState::Unquoted
                    }
                }
                CsvState::Comment => CsvState::Comment,
                CsvState::CarriageReturn => {
                    if strict {
                        CsvState::Invalid
                    } else {
                        CsvState::Standard
                    }
                }
                _ => CsvState::Standard,
            };
            sm.initialize_column(state, next);
        }

        let delimiter_value = options.delimiter.get().as_bytes();
        let delimiter_first_byte = delimiter_value.first().copied().unwrap_or(0);
        let quote = options.quote.value();
        let escape = options.escape.value();
        let comment = options.comment.value();
        let new_line_id = options.new_line.value();
        let multi_byte_delimiter = delimiter_value.len() > 1;
        let enable_unquoted_escape =
            !strict && options.quote.value() != options.escape.value() && escape != 0;
        let delimiter_state = if multi_byte_delimiter {
            CsvState::DelimiterFirstByte
        } else {
            CsvState::Delimiter
        };

        // 1) Standard/Invalid State
        for state in [CsvState::Standard, CsvState::Invalid, CsvState::StandardNewline] {
            sm.set(delimiter_first_byte, state, delimiter_state);
            match new_line_id {
                NewLineIdentifier::CarryOn => {
                    sm.set(b'\r', state, CsvState::CarriageReturn);
                    if state == CsvState::StandardNewline {
                        sm.set(b'\n', state, CsvState::Standard);
                    } else if !strict {
                        sm.set(b'\n', state, CsvState::RecordSeparator);
                    } else {
                        sm.set(b'\n', state, CsvState::Invalid);
                    }
                }
                NewLineIdentifier::SingleN => {
                    sm.set(b'\n', state, CsvState::RecordSeparator);
                    sm.set(
                        b'\r',
                        state,
                        if strict {
                            CsvState::Invalid
                        } else {
                            CsvState::RecordSeparator
                        },
                    );
                }
                NewLineIdentifier::SingleR => {
                    sm.set(b'\r', state, CsvState::RecordSeparator);
                    sm.set(
                        b'\n',
                        state,
                        if strict {
                            CsvState::Invalid
                        } else {
                            CsvState::RecordSeparator
                        },
                    );
                }
                NewLineIdentifier::NotSet => {
                    sm.set(b'\r', state, CsvState::RecordSeparator);
                    sm.set(b'\n', state, CsvState::RecordSeparator);
                }
            }
            if comment != 0 {
                sm.set(comment, state, CsvState::Comment);
            }
            if enable_unquoted_escape {
                sm.set(escape, state, CsvState::UnquotedEscape);
            }
        }
        // 2) Field Separator State
        if quote != 0 {
            sm.set(quote, CsvState::Delimiter, CsvState::Quoted);
        }
        if delimiter_first_byte != b' ' {
            sm.set(b' ', CsvState::Delimiter, CsvState::EmptySpace);
        }
        for state in [
            CsvState::Delimiter,
            CsvState::DelimiterFirstByte,
            CsvState::DelimiterSecondByte,
            CsvState::DelimiterThirdByte,
        ] {
            sm.set(delimiter_first_byte, state, delimiter_state);
            sm.set(b'\n', state, CsvState::RecordSeparator);
            if new_line_id == NewLineIdentifier::CarryOn {
                sm.set(b'\r', state, CsvState::CarriageReturn);
            } else {
                sm.set(b'\r', state, CsvState::RecordSeparator);
            }
            if comment != 0 {
                sm.set(comment, CsvState::Delimiter, CsvState::Comment);
            }
        }
        // Other multi-byte delimiters.
        match delimiter_value.len() {
            2 => sm.set(
                delimiter_value[1],
                CsvState::DelimiterFirstByte,
                CsvState::Delimiter,
            ),
            3 => {
                if delimiter_first_byte == delimiter_value[1] {
                    sm.set(
                        delimiter_value[1],
                        CsvState::DelimiterSecondByte,
                        CsvState::DelimiterSecondByte,
                    );
                }
                sm.set(
                    delimiter_value[1],
                    CsvState::DelimiterFirstByte,
                    CsvState::DelimiterSecondByte,
                );
                sm.set(
                    delimiter_value[2],
                    CsvState::DelimiterSecondByte,
                    CsvState::Delimiter,
                );
            }
            4 => {
                if delimiter_first_byte == delimiter_value[2] {
                    sm.set(
                        delimiter_value[1],
                        CsvState::DelimiterThirdByte,
                        CsvState::DelimiterSecondByte,
                    );
                }
                if delimiter_first_byte == delimiter_value[1] && delimiter_value[1] == delimiter_value[2] {
                    sm.set(
                        delimiter_value[1],
                        CsvState::DelimiterThirdByte,
                        CsvState::DelimiterThirdByte,
                    );
                }
                sm.set(
                    delimiter_value[1],
                    CsvState::DelimiterFirstByte,
                    CsvState::DelimiterSecondByte,
                );
                sm.set(
                    delimiter_value[2],
                    CsvState::DelimiterSecondByte,
                    CsvState::DelimiterThirdByte,
                );
                sm.set(
                    delimiter_value[3],
                    CsvState::DelimiterThirdByte,
                    CsvState::Delimiter,
                );
            }
            _ => {}
        }
        if enable_unquoted_escape {
            sm.set(escape, CsvState::Delimiter, CsvState::UnquotedEscape);
        }

        // 3) Record Separator State
        sm.set(delimiter_first_byte, CsvState::RecordSeparator, delimiter_state);
        sm.set(b'\n', CsvState::RecordSeparator, CsvState::RecordSeparator);
        if new_line_id == NewLineIdentifier::CarryOn {
            sm.set(b'\r', CsvState::RecordSeparator, CsvState::CarriageReturn);
        } else {
            sm.set(b'\r', CsvState::RecordSeparator, CsvState::RecordSeparator);
        }
        if quote != 0 {
            sm.set(quote, CsvState::RecordSeparator, CsvState::Quoted);
        }
        if delimiter_first_byte != b' ' {
            sm.set(b' ', CsvState::RecordSeparator, CsvState::EmptySpace);
        }
        if comment != 0 {
            sm.set(comment, CsvState::RecordSeparator, CsvState::Comment);
        }
        if enable_unquoted_escape {
            sm.set(escape, CsvState::RecordSeparator, CsvState::UnquotedEscape);
        }
        if strict {
            match new_line_id {
                NewLineIdentifier::CarryOn | NewLineIdentifier::SingleR => {
                    sm.set(b'\n', CsvState::RecordSeparator, CsvState::Invalid);
                }
                NewLineIdentifier::SingleN => sm.set(b'\r', CsvState::RecordSeparator, CsvState::Invalid),
                NewLineIdentifier::NotSet => {}
            }
        }

        // 4) Carriage Return State
        sm.set(b'\n', CsvState::CarriageReturn, CsvState::RecordSeparator);
        sm.set(
            b'\r',
            CsvState::CarriageReturn,
            if strict {
                CsvState::Invalid
            } else {
                CsvState::CarriageReturn
            },
        );
        if quote != 0 {
            sm.set(quote, CsvState::CarriageReturn, CsvState::Quoted);
        }
        if delimiter_first_byte != b' ' {
            sm.set(b' ', CsvState::CarriageReturn, CsvState::EmptySpace);
        }
        if comment != 0 {
            sm.set(comment, CsvState::CarriageReturn, CsvState::Comment);
        }
        if enable_unquoted_escape {
            sm.set(escape, CsvState::CarriageReturn, CsvState::UnquotedEscape);
        }

        // 5) Quoted State
        sm.set(quote, CsvState::Quoted, CsvState::Unquoted);
        sm.set(b'\n', CsvState::Quoted, CsvState::QuotedNewLine);
        sm.set(b'\r', CsvState::Quoted, CsvState::QuotedNewLine);
        if options.quote.value() != options.escape.value() && escape != 0 {
            sm.set(escape, CsvState::Quoted, CsvState::Escape);
        }

        // 6) Unquoted State
        sm.set(b'\n', CsvState::Unquoted, CsvState::RecordSeparator);
        if new_line_id == NewLineIdentifier::CarryOn {
            sm.set(b'\r', CsvState::Unquoted, CsvState::CarriageReturn);
        } else {
            sm.set(b'\r', CsvState::Unquoted, CsvState::RecordSeparator);
        }
        sm.set(delimiter_first_byte, CsvState::Unquoted, delimiter_state);
        if options.quote.value() == options.escape.value() {
            sm.set(quote, CsvState::Unquoted, CsvState::Quoted);
        }
        if strict {
            match new_line_id {
                NewLineIdentifier::CarryOn | NewLineIdentifier::SingleR => {
                    sm.set(b'\n', CsvState::Unquoted, CsvState::Invalid);
                }
                NewLineIdentifier::SingleN => sm.set(b'\r', CsvState::Unquoted, CsvState::Invalid),
                NewLineIdentifier::NotSet => {}
            }
        } else if escape == 0 {
            sm.set(quote, CsvState::Unquoted, CsvState::MaybeQuoted);
        } else {
            sm.set(quote, CsvState::Unquoted, CsvState::Quoted);
        }
        if comment != 0 {
            sm.set(comment, CsvState::Unquoted, CsvState::Comment);
        }
        if delimiter_first_byte != b' ' && quote != b' ' && escape != b' ' && comment != b' ' {
            sm.set(b' ', CsvState::Unquoted, CsvState::Unquoted);
        }

        // 8) Not Set
        sm.set(delimiter_first_byte, CsvState::NotSet, delimiter_state);
        sm.set(b'\n', CsvState::NotSet, CsvState::RecordSeparator);
        if new_line_id == NewLineIdentifier::CarryOn {
            sm.set(b'\r', CsvState::NotSet, CsvState::CarriageReturn);
        } else {
            sm.set(b'\r', CsvState::NotSet, CsvState::RecordSeparator);
        }
        if quote != 0 {
            sm.set(quote, CsvState::NotSet, CsvState::Quoted);
        }
        if delimiter_first_byte != b' ' {
            sm.set(b' ', CsvState::NotSet, CsvState::EmptySpace);
        }
        if comment != 0 {
            sm.set(comment, CsvState::NotSet, CsvState::Comment);
        }
        if enable_unquoted_escape {
            sm.set(escape, CsvState::NotSet, CsvState::UnquotedEscape);
        }

        // 9) Quoted NewLine
        sm.set(quote, CsvState::QuotedNewLine, CsvState::Unquoted);
        if options.quote.value() != options.escape.value() && escape != 0 {
            sm.set(escape, CsvState::QuotedNewLine, CsvState::Escape);
        }

        // 10) Empty Value State (Not first value)
        sm.set(delimiter_first_byte, CsvState::EmptySpace, delimiter_state);
        sm.set(b'\n', CsvState::EmptySpace, CsvState::RecordSeparator);
        if new_line_id == NewLineIdentifier::CarryOn {
            sm.set(b'\r', CsvState::EmptySpace, CsvState::CarriageReturn);
        } else {
            sm.set(b'\r', CsvState::EmptySpace, CsvState::RecordSeparator);
        }
        if quote != 0 {
            sm.set(quote, CsvState::EmptySpace, CsvState::Quoted);
        }
        if comment != 0 {
            sm.set(comment, CsvState::EmptySpace, CsvState::Comment);
        }
        if enable_unquoted_escape {
            sm.set(escape, CsvState::EmptySpace, CsvState::UnquotedEscape);
        }

        // 11) Comment State
        sm.set(b'\n', CsvState::Comment, CsvState::RecordSeparator);
        if new_line_id == NewLineIdentifier::CarryOn {
            sm.set(b'\r', CsvState::Comment, CsvState::CarriageReturn);
        } else {
            sm.set(b'\r', CsvState::Comment, CsvState::RecordSeparator);
        }

        // 12) Unquoted Escape State
        if enable_unquoted_escape && new_line_id == NewLineIdentifier::CarryOn {
            sm.set(b'\r', CsvState::UnquotedEscape, CsvState::EscapedReturn);
        }

        // 13) Escaped Return State
        if enable_unquoted_escape {
            sm.set(delimiter_first_byte, CsvState::EscapedReturn, CsvState::Delimiter);
            if new_line_id == NewLineIdentifier::CarryOn {
                sm.set(b'\r', CsvState::EscapedReturn, CsvState::CarriageReturn);
            } else {
                sm.set(b'\r', CsvState::EscapedReturn, CsvState::RecordSeparator);
            }
            if comment != 0 {
                sm.set(comment, CsvState::EscapedReturn, CsvState::Comment);
            }
            sm.set(escape, CsvState::EscapedReturn, CsvState::UnquotedEscape);
        }

        // 14) Maybe quoted
        sm.set(quote, CsvState::MaybeQuoted, CsvState::MaybeQuoted);
        sm.set(b'\n', CsvState::MaybeQuoted, CsvState::RecordSeparator);
        if new_line_id == NewLineIdentifier::CarryOn {
            sm.set(b'\r', CsvState::MaybeQuoted, CsvState::CarriageReturn);
        } else {
            sm.set(b'\r', CsvState::MaybeQuoted, CsvState::RecordSeparator);
        }
        sm.set(delimiter_first_byte, CsvState::MaybeQuoted, delimiter_state);

        // Bytes that can be skipped without a transition.
        sm.skip_standard[delimiter_first_byte as usize] = false;
        sm.skip_standard[b'\n' as usize] = false;
        sm.skip_standard[b'\r' as usize] = false;
        sm.skip_standard[comment as usize] = false;
        if enable_unquoted_escape {
            sm.skip_standard[escape as usize] = false;
        }
        sm.skip_quoted[quote as usize] = false;
        sm.skip_quoted[escape as usize] = false;
        sm.skip_quoted[b'\n' as usize] = false;
        sm.skip_quoted[b'\r' as usize] = false;
        sm.skip_comment[b'\r' as usize] = false;
        sm.skip_comment[b'\n' as usize] = false;

        sm.delimiter = replicate(delimiter_first_byte);
        sm.new_line = replicate(b'\n');
        sm.carriage_return = replicate(b'\r');
        sm.quote = replicate(quote);
        sm.escape = replicate(escape);
        sm.comment = 0;
        sm
    }
}

/// `ShiftAndReplicateBits`: the byte in every byte of a word.
fn replicate(byte: u8) -> u64 {
    u64::from_ne_bytes([byte; 8])
}

/// `BaseScanner::ContainsZeroByte`.
#[inline]
pub fn contains_zero_byte(v: u64) -> bool {
    (v.wrapping_sub(0x0101_0101_0101_0101) & !v & 0x8080_8080_8080_8080) != 0
}

type CacheKey = (String, u8, u8, u8, NewLineIdentifier, bool);

/// The process-wide cache of state machines (DuckDB's `CSVStateMachineCache` object
/// cache entry).
fn cache() -> &'static Mutex<HashMap<CacheKey, Arc<StateMachine>>> {
    static CACHE: OnceLock<Mutex<HashMap<CacheKey, Arc<StateMachine>>>> = OnceLock::new();
    CACHE.get_or_init(Mutex::default)
}

/// `CSVStateMachineCache::Get`.
pub fn cached_state_machine(options: &StateMachineOptions) -> Arc<StateMachine> {
    let mut cache = cache().lock().unwrap_or_else(PoisonError::into_inner);
    Arc::clone(
        cache
            .entry(options.key())
            .or_insert_with(|| Arc::new(StateMachine::build(options))),
    )
}

/// A state machine with its dialect (DuckDB's `CSVStateMachine`).
#[derive(Clone, Debug)]
pub struct CsvStateMachine {
    pub transition_array: Arc<StateMachine>,
    pub state_machine_options: StateMachineOptions,
    pub options: CsvReaderOptions,
    pub dialect_options: DialectOptions,
}

impl CsvStateMachine {
    /// `CSVStateMachine(options, state_machine_options, cache)`.
    pub fn new(options: &CsvReaderOptions, state_machine_options: &StateMachineOptions) -> Self {
        let dialect_options = DialectOptions {
            state_machine_options: state_machine_options.clone(),
            ..DialectOptions::default()
        };
        Self {
            transition_array: cached_state_machine(state_machine_options),
            state_machine_options: state_machine_options.clone(),
            options: options.clone(),
            dialect_options,
        }
    }

    /// `CSVStateMachine(transition_array, options)`.
    pub fn with_transitions(transition_array: Arc<StateMachine>, options: &CsvReaderOptions) -> Self {
        let mut dialect_options = options.dialect_options.clone();
        dialect_options.state_machine_options = options.dialect_options.state_machine_options.clone();
        Self {
            transition_array,
            state_machine_options: options.dialect_options.state_machine_options.clone(),
            options: options.clone(),
            dialect_options,
        }
    }

    /// `Transition`.
    #[inline]
    pub fn transition(&self, states: &mut CsvStates, byte: u8) {
        states.states[0] = states.states[1];
        states.states[1] = self.transition_array.get(byte, states.states[1]);
    }
}
