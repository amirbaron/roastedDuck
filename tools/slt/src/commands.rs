//! Executable records (port of the `Command` hierarchy in `test/sqlite/sqllogic_command.hpp`).
//!
//! Commands are built by the runner while it walks a file. Outside loops they execute
//! immediately; inside `loop`/`foreach` they are collected into a [`LoopCommand`] that runs once
//! `endloop` is reached.

use std::time::Duration;

use crate::parser::{Condition, ExpectedResult, SortStyle};

/// A command together with the attributes every command shares.
#[derive(Clone, Debug)]
pub struct Command {
    /// 1-based line of the record header (`query_line`), for statements and queries.
    pub line: Option<usize>,
    /// Named connection (`statement ok con1`); `None` uses the main connection.
    pub connection: Option<String>,
    /// SQL text after keyword replacement, before loop-variable replacement.
    pub sql: String,
    /// Loop-variable conditions from `skipif`/`onlyif`.
    pub conditions: Vec<Condition>,
    pub kind: CommandKind,
}

impl Command {
    /// A command without SQL, line, connection or conditions.
    pub fn bare(kind: CommandKind) -> Self {
        Self {
            line: None,
            connection: None,
            sql: String::new(),
            conditions: Vec::new(),
            kind,
        }
    }

    /// Whether the command may run inside `concurrentloop`/`concurrentforeach`.
    pub fn supports_concurrent(&self) -> bool {
        match &self.kind {
            CommandKind::Statement(_)
            | CommandKind::Query(_)
            | CommandKind::ResetLabel(_)
            | CommandKind::Sleep(_) => true,
            CommandKind::Loop(definition) => definition.commands.iter().all(Self::supports_concurrent),
            CommandKind::Restart { .. }
            | CommandKind::Reconnect
            | CommandKind::Continue
            | CommandKind::Mode(_)
            | CommandKind::Unzip { .. }
            | CommandKind::Load { .. } => false,
        }
    }
}

/// The command-specific part of a [`Command`].
#[derive(Clone, Debug)]
pub enum CommandKind {
    Statement(StatementCommand),
    Query(QueryCommand),
    ResetLabel(String),
    Restart {
        load_extensions: bool,
    },
    Reconnect,
    Loop(LoopCommand),
    Continue,
    /// `mode output_hash|output_result|no_output|debug`.
    Mode(String),
    Sleep(Duration),
    Unzip {
        input: String,
        output: String,
    },
    Load(LoadCommand),
}

/// `statement ok|error|maybe|debug`.
#[derive(Clone, Debug)]
pub struct StatementCommand {
    pub expected: ExpectedResult,
    /// Expected error text (substring, or `<REGEX>:`/`<!REGEX>:` pattern); may be empty.
    pub expected_error: String,
}

/// `query <types> [sort|connection] [label]`.
#[derive(Clone, Debug)]
pub struct QueryCommand {
    pub expected_column_count: usize,
    pub sort_style: SortStyle,
    /// Expected result lines (values, tab-separated rows, a hash, or `<FILE>:path`).
    pub values: Vec<String>,
    pub label: Option<String>,
}

/// `load <path> [readonly|readwrite] [version]`.
#[derive(Clone, Debug)]
pub struct LoadCommand {
    pub path: String,
    pub read_only: bool,
    pub version: Option<String>,
}

/// A loop being iterated (`LoopDefinition`).
#[derive(Clone, Debug)]
pub struct LoopDefinition {
    pub iterator_name: String,
    pub index: i64,
    pub start: i64,
    pub end: i64,
    pub parallel: bool,
    /// `foreach` values; empty for numeric `loop`s.
    pub tokens: Vec<String>,
    /// Set by `continue` for the rest of the current iteration.
    pub skipped: bool,
}

impl LoopDefinition {
    /// The current value of the iterator, as substituted into SQL.
    pub fn current_value(&self) -> Option<String> {
        if self.tokens.is_empty() {
            Some(self.index.to_string())
        } else {
            usize::try_from(self.index)
                .ok()
                .and_then(|index| self.tokens.get(index))
                .cloned()
        }
    }
}

/// `loop`/`foreach` (and their concurrent variants) with the commands of their body.
#[derive(Clone, Debug)]
pub struct LoopCommand {
    pub definition: LoopDefinition,
    pub commands: Vec<Command>,
}

/// Per-execution state threaded through commands (`ExecuteContext`).
#[derive(Clone, Debug, Default)]
pub struct ExecuteContext {
    pub running_loops: Vec<LoopDefinition>,
    /// Whether this context belongs to one thread of a concurrent loop.
    pub parallel: bool,
    /// The SQL of the command being executed, after loop replacement.
    pub sql: String,
    /// In a concurrent loop: the line of the first failed statement or query.
    pub error_line: Option<Option<usize>>,
}
