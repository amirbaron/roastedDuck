//! The shell's state (DuckDB's `ShellState` in `tools/shell/include/shell_state.hpp`,
//! with its helpers from `shell.cpp`): output modes and separators, the output file or
//! pager, highlighting options and the input being read. The database and its connection
//! live beside it in [`crate::shell::Shell`].

use std::fmt::Write as _;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Read};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, PoisonError};

use rustdb::Config;
use rustdb_common::ByteString;

use crate::highlight::{self, HighlightElementType, HighlightMode, ShellHighlight};
use crate::output::{Output, OutputTarget, PrintOutput};
use crate::printer;
use crate::progress_bar::ShellProgressBar;
use crate::prompt::Prompt;
use crate::renderer::ScannedResult;

/// How query results are rendered (`RenderMode`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderMode {
    /// One column per line, a blank line between records.
    Line,
    /// One record per line in neat columns.
    Column,
    /// One record per line with a separator.
    List,
    /// `.schema` output: each statement ends in `;`.
    Semi,
    /// An XHTML table.
    Html,
    /// SQL `INSERT` statements.
    Insert,
    /// Values quoted as for SQL.
    Quote,
    /// TCL list elements.
    Tcl,
    /// Comma-separated values.
    Csv,
    /// The output of `EXPLAIN`.
    Explain,
    /// The table of a `DESCRIBE`.
    Describe,
    /// ASCII unit and record separators (0x1F/0x1E).
    Ascii,
    /// Pretty-printed schemas.
    Pretty,
    /// A JSON array.
    Json,
    /// Markdown tables.
    Markdown,
    /// MySQL-style tables.
    Table,
    /// Unicode box-drawing characters.
    Box,
    /// LaTeX tabular environments.
    Latex,
    /// No output.
    Trash,
    /// JSON lines.
    JsonLines,
    /// DuckDB's own boxes.
    Duckbox,
}

impl RenderMode {
    /// `ShellState::ModeToString`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Line => "line",
            Self::Column => "column",
            Self::List => "list",
            Self::Semi => "semi",
            Self::Html => "html",
            Self::Insert => "insert",
            Self::Quote => "quote",
            Self::Tcl => "tcl",
            Self::Csv => "csv",
            Self::Explain => "explain",
            Self::Describe => "describe",
            Self::Ascii => "ascii",
            Self::Pretty => "prettyprint",
            Self::Json => "json",
            Self::Markdown => "markdown",
            Self::Table => "table",
            Self::Box => "box",
            Self::Latex => "latex",
            Self::Trash => "trash",
            Self::JsonLines => "jsonlines",
            Self::Duckbox => "duckbox",
        }
    }
}

/// When readable renderings of large numbers are shown (`LargeNumberRendering` of the
/// shell, with `DEFAULT` choosing by whether the output is a terminal).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LargeNumberRendering {
    None,
    Footer,
    All,
    Default,
}

/// The input a line is read from (`ShellState::in`).
pub enum Input {
    /// Standard input typed at a terminal: read with the line editor and a prompt.
    Interactive,
    /// Standard input that is a file or a pipe (`in = stdin`), read through the
    /// process's buffered stdin.
    Stdin,
    /// A file of `.read`, `-f` or the init file.
    Reader(BufReader<Box<dyn Read>>),
}

impl std::fmt::Debug for Input {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Interactive => "Interactive",
            Self::Stdin => "Stdin",
            Self::Reader(_) => "Reader",
        })
    }
}

/// The mode of reading input (`InputMode`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputMode {
    Standard,
    File,
    DuckdbRc,
}

/// `ShellFlags`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum ShellFlag {
    /// `.dump --newlines`.
    Newlines = 0x10,
    /// `.changes`.
    CountChanges = 0x20,
    /// `.echo` or `-echo`.
    Echo = 0x40,
    /// `.headers` was used.
    HeaderSet = 0x80,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SuccessState {
    Success,
    Failure,
}

/// An option that is on, off, or decided by the environment (`OptionType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OptionType {
    Default,
    On,
    Off,
}

/// The text shown at start-up (`StartupText`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartupText {
    All,
    Version,
    None,
}

/// How interactive input is read (`ReadLineVersion`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadLineVersion {
    Linenoise,
    Fallback,
}

/// `PagerMode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PagerMode {
    Automatic,
    On,
    Off,
}

/// The outcome of a dot command or command-line option (`MetadataResult`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MetadataResult {
    Success = 0,
    Fail = 1,
    Exit = 2,
    PrintUsage = 3,
}

/// Whether errors stop processing (`BailOnError`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(clippy::enum_variant_names, reason = "DuckDB's names")]
pub enum BailOnError {
    Automatic,
    BailOnError,
    DontBailOnError,
}

/// A column of a table as `.tables` and `DESCRIBE` show it (`ShellColumnInfo`).
#[derive(Clone, Debug, Default)]
pub struct ShellColumnInfo {
    pub column_name: rustdb_common::ByteString,
    pub column_type: String,
    pub is_primary_key: bool,
    pub is_not_null: bool,
    pub is_unique: bool,
    pub default_value: String,
}

/// A table as `.tables` and `DESCRIBE` show it (`ShellTableInfo`).
#[derive(Clone, Debug, Default)]
pub struct ShellTableInfo {
    pub database_name: rustdb_common::ByteString,
    pub schema_name: rustdb_common::ByteString,
    pub table_name: rustdb_common::ByteString,
    pub estimated_size: Option<u64>,
    pub is_view: bool,
    pub columns: Vec<ShellColumnInfo>,
}

/// The result of the last query, which `.last` renders again and `FROM _` reads
/// (`ShellState::last_result`); shared with the replacement scan of `_`.
pub type LastResult = Arc<Mutex<Option<ScannedResult>>>;

/// The separators of the modes (`SEP_*`).
pub const SEP_COLUMN: &str = "|";
pub const SEP_ROW: &str = "\n";
pub const SEP_TAB: &str = "\t";
pub const SEP_SPACE: &str = " ";
pub const SEP_COMMA: &str = ",";
pub const SEP_CRLF: &str = "\r\n";
pub const SEP_UNIT: &str = "\x1F";
pub const SEP_RECORD: &str = "\x1E";

/// The longest prompt (`MAX_PROMPT_SIZE`, including the terminating NUL).
const MAX_PROMPT_SIZE: usize = 20;

/// DuckDB's `ShellState`, except the database and connection.
#[derive(Debug)]
#[allow(clippy::struct_excessive_bools, reason = "DuckDB's flags")]
pub struct ShellState {
    /// The configuration databases are opened with.
    pub config: Config,
    /// Whether `ResetOutput` opens the temporary file with the system viewer.
    pub do_xdg_open: bool,
    /// Commands left before the output returns to stdout (`.once`).
    pub out_count: u32,
    /// The number of the line last read.
    pub lineno: usize,
    /// Where input is read from.
    pub input: Input,
    /// Where results are written.
    pub out: Output,
    /// The number of errors seen.
    pub n_err: usize,
    pub mode: RenderMode,
    pub mode_prior: RenderMode,
    /// The mode of the current statement.
    pub c_mode: RenderMode,
    pub show_header: bool,
    pub shell_flags: u32,
    pub prior_shell_flags: u32,
    /// The table name of `.mode insert`.
    pub dest_table: ByteString,
    /// A temporary file that might need deleting.
    pub temp_file: String,
    pub col_separator: ByteString,
    pub row_separator: ByteString,
    pub col_separator_prior: ByteString,
    pub row_separator_prior: ByteString,
    /// The widths of `.width`; negative values right-align.
    pub col_width: Vec<i32>,
    /// How NULL renders.
    pub null_value: ByteString,
    /// Column-wise duckbox rendering.
    pub columns: bool,
    /// The name of the output file, or `|command` for a pipe.
    pub outfile: ByteString,
    pub db_filename: ByteString,
    /// The file of `.log`.
    pub log: Option<OutputTarget>,
    /// The rows of a duckbox result.
    pub max_rows: usize,
    /// The width of a duckbox result; 0 is the terminal width.
    pub max_width: usize,
    /// The rows analyzed to size the duckbox columns; 0 is automatic.
    pub max_analyze_rows: usize,
    pub decimal_separator: Option<u8>,
    pub thousand_separator: Option<u8>,
    pub large_number_rendering: LargeNumberRendering,
    /// The command `-ui` runs.
    pub ui_command: ByteString,
    pub last_changes: u64,
    pub total_changes: u64,
    /// Whether commands are read from stdin after the command line.
    pub read_stdin: bool,
    pub init_file: ByteString,
    pub run_init: bool,
    pub last_result: LastResult,
    pub bail: BailOnError,
    /// The table name shown for a `DESCRIBE`.
    pub describe_table_name: rustdb_common::ByteString,
    pub stdin_is_interactive: bool,
    pub stdout_is_console: bool,
    pub stderr_is_console: bool,
    pub program_name: ByteString,
    pub safe_mode: bool,
    pub highlight_errors: OptionType,
    pub highlight_results: OptionType,
    /// The init file being read.
    pub duckdb_rc_path: ByteString,
    pub startup_text: StartupText,
    pub displayed_loading_resources_message: bool,
    pub main_prompt: Prompt,
    pub continue_prompt: ByteString,
    pub continue_prompt_selected: ByteString,
    pub scroll_up_prompt: ByteString,
    pub scroll_down_prompt: ByteString,
    pub progress_bar: ShellProgressBar,
    pub rl_version: ReadLineVersion,
    pub pager_mode: PagerMode,
    pub pager_command: ByteString,
    /// In automatic mode, the pager shows results of at least this many rows.
    pub pager_min_rows: usize,
    pub pager_is_active: bool,
    pub highlight_mode: HighlightMode,
    /// `.timer` (the static `enableTimer`).
    pub enable_timer: bool,
}

impl ShellState {
    /// `ShellState::ShellState` followed by `Initialize`.
    pub fn new() -> Self {
        Self {
            config: Config::default(),
            do_xdg_open: false,
            out_count: 0,
            lineno: 0,
            input: Input::Interactive,
            out: Output::default(),
            n_err: 0,
            mode: RenderMode::Duckbox,
            mode_prior: RenderMode::Line,
            c_mode: RenderMode::Duckbox,
            show_header: true,
            shell_flags: 0,
            prior_shell_flags: 0,
            dest_table: ByteString::new(),
            temp_file: String::new(),
            col_separator: SEP_COLUMN.into(),
            row_separator: SEP_ROW.into(),
            col_separator_prior: ByteString::new(),
            row_separator_prior: ByteString::new(),
            col_width: Vec::new(),
            null_value: "NULL".into(),
            columns: false,
            outfile: ByteString::new(),
            db_filename: ByteString::new(),
            log: None,
            max_rows: 40,
            max_width: 0,
            max_analyze_rows: 0,
            decimal_separator: None,
            thousand_separator: None,
            large_number_rendering: LargeNumberRendering::Default,
            ui_command: "CALL start_ui()".into(),
            last_changes: 0,
            total_changes: 0,
            read_stdin: true,
            init_file: ByteString::new(),
            run_init: true,
            last_result: LastResult::default(),
            bail: BailOnError::Automatic,
            describe_table_name: rustdb_common::ByteString::new(),
            stdin_is_interactive: true,
            stdout_is_console: true,
            stderr_is_console: true,
            program_name: "duckdb".into(),
            safe_mode: false,
            highlight_errors: OptionType::Default,
            highlight_results: OptionType::Default,
            duckdb_rc_path: ByteString::new(),
            startup_text: StartupText::All,
            displayed_loading_resources_message: false,
            main_prompt: Prompt::default_main_prompt(),
            continue_prompt: "  ".into(),
            continue_prompt_selected: "  ".into(),
            scroll_up_prompt: "⇡ ".into(),
            scroll_down_prompt: "⇣ ".into(),
            progress_bar: ShellProgressBar::with_default_components(),
            rl_version: ReadLineVersion::Linenoise,
            pager_mode: PagerMode::Automatic,
            pager_command: ByteString::new(),
            pager_min_rows: 50,
            pager_is_active: false,
            highlight_mode: HighlightMode::Automatic,
            enable_timer: false,
        }
    }

    /// The rest of `ShellState::Initialize`: the line editor's continuation prompts.
    pub fn initialize(&self) {
        crate::linenoise::set_prompt(
            &self.continue_prompt,
            &self.continue_prompt_selected,
            &self.scroll_up_prompt,
            &self.scroll_down_prompt,
        );
    }

    /// `Print(...)` to the output.
    pub fn print(&self, text: impl AsRef<[u8]>) {
        self.out.print(PrintOutput::Stdout, text);
    }

    /// `Print(PrintOutput::STDERR, ...)`.
    pub fn print_err(&self, text: impl AsRef<[u8]>) {
        self.out.print(PrintOutput::Stderr, text);
    }

    /// `printf`: to standard output whatever the output is.
    pub fn printf_stdout(text: impl AsRef<[u8]>) {
        printer::write_stdout(text.as_ref());
    }

    /// `ShellState::RenderLength`.
    pub fn render_length(text: impl AsRef<[u8]>) -> usize {
        crate::utf8::terminal_render_width(text.as_ref())
    }

    /// `ShellState::IsCharacter`: whether a byte starts a character.
    pub fn is_character(c: u8) -> bool {
        (c & 0xc0) != 0x80
    }

    pub fn has_flag(&self, flag: ShellFlag) -> bool {
        self.shell_flags & flag as u32 != 0
    }

    pub fn set_flag(&mut self, flag: ShellFlag) {
        self.shell_flags |= flag as u32;
    }

    pub fn clear_flag(&mut self, flag: ShellFlag) {
        self.shell_flags &= !(flag as u32);
    }

    /// `SetOrClearFlag`.
    pub fn set_or_clear_flag(&mut self, flag: ShellFlag, arg: impl AsRef<[u8]>) {
        if self.string_to_bool(arg) {
            self.set_flag(flag);
        } else {
            self.clear_flag(flag);
        }
    }

    /// `HighlightErrors`.
    pub fn highlight_errors(&self) -> bool {
        match self.highlight_errors {
            OptionType::Default => self.stderr_is_console,
            option => option == OptionType::On,
        }
    }

    /// `HighlightResults`.
    pub fn highlight_results(&self) -> bool {
        match self.highlight_results {
            OptionType::Default => self.stdout_is_console,
            option => option == OptionType::On,
        }
    }

    /// `PushOutputMode`.
    pub fn push_output_mode(&mut self) {
        self.mode_prior = self.mode;
        self.prior_shell_flags = self.shell_flags;
        self.col_separator_prior.clone_from(&self.col_separator);
        self.row_separator_prior.clone_from(&self.row_separator);
    }

    /// `PopOutputMode`.
    pub fn pop_output_mode(&mut self) {
        self.mode = self.mode_prior;
        self.shell_flags = self.prior_shell_flags;
        self.col_separator.clone_from(&self.col_separator_prior);
        self.row_separator.clone_from(&self.row_separator_prior);
    }

    /// `EscapeCString`: `str` quoted by C and TCL rules.
    pub fn escape_c_string(text: impl AsRef<[u8]>) -> String {
        let mut result = String::from("\"");
        for &c in text.as_ref() {
            match c {
                b'\\' => result.push_str("\\\\"),
                b'"' => result.push_str("\\\""),
                b'\t' => result.push_str("\\t"),
                b'\n' => result.push_str("\\n"),
                b'\r' => result.push_str("\\r"),
                // `isprint` in the C locale.
                0x20..=0x7e => result.push(char::from(c)),
                _ => {
                    let _ = write!(result, "\\{c:03o}");
                }
            }
        }
        result.push('"');
        result
    }

    /// `GetSchemaLine`.
    pub fn schema_line(text: &str, tail: &str) -> String {
        format!("{text}{tail}")
    }

    /// `GetSchemaLineN`: the first `n` bytes of `text`.
    pub fn schema_line_n(text: &[u8], n: usize, tail: &str) -> String {
        let text = &text[..n.min(text.len())];
        format!("{}{tail}", String::from_utf8_lossy(text))
    }

    /// `ColumnTypeIsInteger`.
    pub fn column_type_is_integer(type_name: &str) -> bool {
        matches!(
            type_name,
            "TINYINT" | "SMALLINT" | "INTEGER" | "BIGINT" | "FLOAT" | "DOUBLE" | "DECIMAL"
        )
    }

    /// `SetTableName`: the table name of `.mode insert`, quoted if needed.
    pub fn set_table_name(&mut self, name: Option<&[u8]>) {
        self.dest_table = name.map(sql_identifier_bytes).unwrap_or_default();
    }

    /// `StringToInt`: an integer with an optional sign, hexadecimal with `0x`, and an
    /// optional size suffix (`KiB`, `MB`, `G`, ...).
    pub fn string_to_int(arg: impl AsRef<[u8]>) -> i64 {
        const MULTIPLIERS: [(&str, i64); 9] = [
            ("KiB", 1024),
            ("MiB", 1024 * 1024),
            ("GiB", 1024 * 1024 * 1024),
            ("KB", 1000),
            ("MB", 1_000_000),
            ("GB", 1_000_000_000),
            ("K", 1000),
            ("M", 1_000_000),
            ("G", 1_000_000_000),
        ];
        let mut bytes = arg.as_ref();
        let mut value: i64 = 0;
        let negative = bytes.first() == Some(&b'-');
        if negative || bytes.first() == Some(&b'+') {
            bytes = &bytes[1..];
        }
        if bytes.starts_with(b"0x") {
            bytes = &bytes[2..];
            while let Some(digit) = bytes.first().and_then(|&c| hex_digit_value(c)) {
                value = (value << 4).wrapping_add(i64::from(digit));
                bytes = &bytes[1..];
            }
        } else {
            while let Some(&c) = bytes.first().filter(|c| c.is_ascii_digit()) {
                value = value.wrapping_mul(10).wrapping_add(i64::from(c - b'0'));
                bytes = &bytes[1..];
            }
        }
        if let Some((_, multiplier)) = MULTIPLIERS
            .iter()
            .find(|(suffix, _)| suffix.as_bytes().eq_ignore_ascii_case(bytes))
        {
            value = value.wrapping_mul(*multiplier);
        }
        if negative { value.wrapping_neg() } else { value }
    }

    /// `StringToBool`: an integer, `on`/`yes` or `off`/`no`; anything else is reported
    /// and taken as false.
    pub fn string_to_bool(&self, arg: impl AsRef<[u8]>) -> bool {
        let arg = arg.as_ref();
        let bytes = arg;
        let digits = if bytes.starts_with(b"0x") {
            2 + bytes[2..]
                .iter()
                .take_while(|&&c| hex_digit_value(c).is_some())
                .count()
        } else {
            bytes.iter().take_while(|c| c.is_ascii_digit()).count()
        };
        if digits > 0 && digits == bytes.len() {
            return Self::string_to_int(arg) & 0xffff_ffff != 0;
        }
        if arg.eq_ignore_ascii_case(b"on") || arg.eq_ignore_ascii_case(b"yes") {
            return true;
        }
        if arg.eq_ignore_ascii_case(b"off") || arg.eq_ignore_ascii_case(b"no") {
            return false;
        }
        self.print_err(rustdb_common::byte_format!(
            "ERROR: Not a boolean value: \"{arg}\". Assuming \"no\".\n"
        ));
        false
    }

    /// `SetOutputMode`: `.mode NAME ?TABLE?`, where NAME may be a prefix of the mode.
    pub fn set_output_mode(&mut self, mode_name: &[u8], table_name: Option<&[u8]>) -> bool {
        let is = |name: &str| name.as_bytes().starts_with(mode_name) && !mode_name.is_empty();
        let first = mode_name.first().copied().unwrap_or(0);
        let long = mode_name.len() > 2;
        if table_name.is_some() && !(first == b'i' && is("insert")) {
            self.print_err("TABLE argument can only be used with .mode insert");
            return false;
        }
        if first == b'l' && long && is("lines") {
            self.mode = RenderMode::Line;
            self.row_separator = SEP_ROW.into();
        } else if first == b'c' && is("columns") {
            self.mode = RenderMode::Column;
            if self.has_flag(ShellFlag::HeaderSet) {
                self.show_header = true;
            }
            self.row_separator = SEP_ROW.into();
        } else if first == b'l' && long && is("list") {
            self.mode = RenderMode::List;
            self.col_separator = SEP_COLUMN.into();
            self.row_separator = SEP_ROW.into();
        } else if first == b'h' && is("html") {
            self.mode = RenderMode::Html;
        } else if first == b't' && is("tcl") {
            self.mode = RenderMode::Tcl;
            self.col_separator = SEP_SPACE.into();
            self.row_separator = SEP_ROW.into();
        } else if first == b'c' && is("csv") {
            self.mode = RenderMode::Csv;
            self.col_separator = SEP_COMMA.into();
            self.row_separator = SEP_CRLF.into();
        } else if first == b't' && is("tabs") {
            self.mode = RenderMode::List;
            self.col_separator = SEP_TAB.into();
        } else if first == b'i' && is("insert") {
            self.mode = RenderMode::Insert;
            self.set_table_name(Some(table_name.unwrap_or(b"table")));
        } else if first == b'q' && is("quote") {
            self.mode = RenderMode::Quote;
            self.col_separator = SEP_COMMA.into();
            self.row_separator = SEP_ROW.into();
        } else if first == b'a' && is("ascii") {
            self.mode = RenderMode::Ascii;
            self.col_separator = SEP_UNIT.into();
            self.row_separator = SEP_RECORD.into();
        } else if first == b'm' && is("markdown") {
            self.mode = RenderMode::Markdown;
        } else if first == b't' && is("table") {
            self.mode = RenderMode::Table;
        } else if first == b'b' && is("box") {
            self.mode = RenderMode::Box;
        } else if first == b'd' && is("duckbox") {
            self.mode = RenderMode::Duckbox;
        } else if first == b'j' && is("json") {
            self.mode = RenderMode::Json;
        } else if first == b'l' && is("latex") {
            self.mode = RenderMode::Latex;
        } else if first == b't' && is("trash") {
            self.mode = RenderMode::Trash;
        } else if first == b'j' && is("jsonlines") {
            self.mode = RenderMode::JsonLines;
        } else {
            self.print_err(
                "Error: mode should be one of: ascii box column csv duckbox html insert json jsonlines latex line \
                 list markdown quote table tabs tcl trash \n",
            );
            return false;
        }
        self.c_mode = self.mode;
        true
    }

    /// `ShowConfiguration` (`.show`).
    pub fn show_configuration(&self) {
        let on_off = |on: bool| if on { "on" } else { "off" };
        self.print(format!(
            "{:>12.12}: {}\n",
            "echo",
            on_off(self.has_flag(ShellFlag::Echo))
        ));
        self.print(format!("{:>12.12}: {}\n", "headers", on_off(self.show_header)));
        self.print(format!("{:>12.12}: {}\n", "mode", self.mode.name()));
        self.print(format!("{:>12.12}: ", "nullvalue"));
        self.print(Self::escape_c_string(&self.null_value));
        self.print("\n");
        let output = if self.outfile.is_empty() {
            b"stdout".as_slice()
        } else {
            self.outfile.as_bytes()
        };
        self.print(format!("{:>12.12}: ", "output"));
        self.print(output);
        self.print("\n");
        self.print(format!("{:>12.12}: ", "colseparator"));
        self.print(Self::escape_c_string(&self.col_separator));
        self.print("\n");
        self.print(format!("{:>12.12}: ", "rowseparator"));
        self.print(Self::escape_c_string(&self.row_separator));
        self.print("\n");
        self.print(format!("{:>12.12}: ", "width"));
        for width in &self.col_width {
            self.print(format!("{width} "));
        }
        self.print("\n");
        self.print(format!("{:>12.12}: ", "filename"));
        self.print(&self.db_filename);
        self.print("\n");
    }

    /// `PrintDatabaseError`: an error to stderr, highlighted when errors are.
    pub fn print_database_error(&mut self, error: impl AsRef<[u8]>) {
        let error = error.as_ref();
        if !self.highlight_errors() {
            self.print_err(error);
            self.print_err(b"\n");
            return;
        }
        if let Ok(text) = std::str::from_utf8(error) {
            self.detect_dark_light_mode();
            ShellHighlight::new(&self.out).print_error(text);
        } else {
            // The text highlighter still requires UTF-8; preserve the original bytes.
            self.print_err(error);
            self.print_err(b"\n");
        }
    }

    /// `DetectDarkLightMode`: on a terminal, the color scheme follows its background.
    pub fn detect_dark_light_mode(&mut self) {
        if self.highlight_mode != HighlightMode::Automatic || !self.stdout_is_console {
            return;
        }
        self.highlight_mode = match crate::linenoise::terminal_color_mode() {
            crate::linenoise::TerminalColorMode::Dark => {
                highlight::toggle_mode(HighlightMode::DarkMode);
                HighlightMode::DarkMode
            }
            crate::linenoise::TerminalColorMode::Light => {
                highlight::toggle_mode(HighlightMode::LightMode);
                HighlightMode::LightMode
            }
            _ => HighlightMode::MixedMode,
        };
    }

    /// `ShellHighlight(state).PrintText(...)`.
    pub fn print_highlighted(
        &self,
        text: impl AsRef<[u8]>,
        output: PrintOutput,
        element: HighlightElementType,
    ) {
        ShellHighlight::new(&self.out).print_bytes(text.as_ref(), output, element);
    }

    /// `ClearInterrupt`, without the connection's flag (the next query clears it).
    pub fn clear_interrupt() {
        crate::interrupt::clear();
    }

    /// `SetPrompt`: at most 19 bytes of `value`.
    pub fn truncated_prompt(value: impl AsRef<[u8]>) -> ByteString {
        let value = value.as_ref();
        let end = value.iter().position(|&byte| byte == 0).unwrap_or(value.len())
            .min(MAX_PROMPT_SIZE - 1);
        value[..end].into()
    }

    /// `GetMaxRenderWidth`: `.maxwidth`, else the terminal width; at least 80.
    pub fn max_render_width(&self) -> usize {
        let width = if self.max_width == 0 {
            printer::terminal_width()
        } else {
            self.max_width
        };
        width.max(80)
    }

    /// `CloseOutputFile` of the output (`fclose(out)` unless it is stdout or stderr).
    fn close_output(&mut self) {
        let previous = self.out.replace(OutputTarget::Stdout);
        previous.close();
    }

    /// `ResetOutput`: the output returns to stdout, closing a file or pipe; a temporary
    /// file of `.excel` / `.once -e` is opened with the system viewer first.
    pub fn reset_output(&mut self) {
        let is_pipe = self.outfile.len() > 1 && self.outfile.starts_with(b"|");
        self.close_output();
        if !is_pipe && self.do_xdg_open {
            let open_command = if cfg!(target_os = "macos") {
                "open"
            } else {
                "xdg-open"
            };
            let command = format!("{open_command} {}", self.temp_file);
            if run_system(&command) == 0 {
                // Give the viewer time to start before the file may be deleted.
                std::thread::sleep(std::time::Duration::from_millis(2000));
            } else {
                self.print_err(format!("Failed: [{command}]\n"));
            }
            self.pop_output_mode();
            self.do_xdg_open = false;
        }
        self.outfile.clear();
        self.stdout_is_console = true;
    }

    /// `OpenOutputFile`: `stdout`, `stderr`, `off` (none) or a file.
    pub fn open_output_file(&self, file: impl AsRef<[u8]>) -> Option<OutputTarget> {
        let file = file.as_ref();
        match file {
            b"stdout" => Some(OutputTarget::Stdout),
            b"stderr" => Some(OutputTarget::Stderr),
            b"off" => None,
            _ => {
                let handle = rustdb_function::file_system::native_path(&expand_path_bytes(file))
                    .ok()
                    .and_then(|path| File::create(path).ok());
                if let Some(handle) = handle {
                    Some(OutputTarget::File(BufWriter::new(handle)))
                } else {
                    self.print_err(rustdb_common::byte_format!("Error: cannot open \"{file}\"\n"));
                    None
                }
            }
        }
    }

    /// `ClearTempFile`. As in DuckDB, this keeps the temporary file: it only acts on an
    /// empty name.
    pub fn clear_temp_file(&mut self) {
        if !self.temp_file.is_empty() || self.do_xdg_open {
            return;
        }
        if std::fs::remove_file(&self.temp_file).is_err() {
            return;
        }
        self.temp_file.clear();
    }

    /// `NewTempFile`: a temporary file name with `suffix`.
    pub fn new_temp_file(&mut self, suffix: &str) {
        self.clear_temp_file();
        let temp = std::env::var("TEMP")
            .or_else(|_| std::env::var("TMP"))
            .unwrap_or_else(|_| "/tmp".to_owned());
        self.temp_file = format!("{temp}/temp{:x}.{suffix}", random_u64());
    }

    /// `GetSystemPager`: `DUCKDB_PAGER`, `PAGER`, or `less -SRX`.
    pub fn system_pager() -> ByteString {
        for variable in ["DUCKDB_PAGER", "PAGER"] {
            if let Some(pager) = std::env::var_os(variable)
                && !pager.is_empty()
            {
                return pager.as_os_str().as_encoded_bytes().into();
            }
        }
        "less -SRX".into()
    }

    /// `ShouldUsePager()`: only for results to an interactive terminal.
    pub fn should_use_pager(&mut self) -> bool {
        if !self.out.is_stdout()
            || !self.stdout_is_console
            || !self.outfile.is_empty()
            || !self.stdin_is_interactive
        {
            return false;
        }
        if self.pager_mode == PagerMode::Off {
            return false;
        }
        if self.pager_command.is_empty() {
            self.pager_command = Self::system_pager();
        }
        true
    }

    /// `ShouldUsePager(line_count)`.
    pub fn should_use_pager_for_lines(&mut self, line_count: usize) -> bool {
        if !self.should_use_pager() {
            return false;
        }
        !(self.pager_mode == PagerMode::Automatic && line_count < self.pager_min_rows)
    }

    /// `SetupPager`: the output goes to the pager until [`ShellState::finish_pager`].
    /// Returns whether the pager started.
    pub fn setup_pager(&mut self) -> bool {
        // A pager that exits early makes writes fail with SIGPIPE: treat that as an
        // interrupt (`StartPagerDisplay`).
        crate::interrupt::install(libc::SIGPIPE);
        let child = Command::new("/bin/sh")
            .arg("-c")
            .arg(match rustdb_function::file_system::native_path(&self.pager_command) {
                Ok(command) => command,
                Err(_) => return false,
            })
            .stdin(Stdio::piped())
            .spawn();
        let mut child = match child {
            Ok(child) => child,
            Err(error) => {
                self.finish_pager_display();
                self.print_err(format!(
                    "Error: Failed to start pager process: {error}. Output will be sent to stdout.\n"
                ));
                return false;
            }
        };
        let Some(stdin) = child.stdin.take() else {
            self.finish_pager_display();
            return false;
        };
        self.pager_is_active = true;
        self.out.replace(OutputTarget::Pipe {
            child,
            stdin: BufWriter::new(stdin),
        });
        self.outfile = rustdb_common::byte_format!("|{}", self.pager_command);
        true
    }

    /// `~PagerState`: the output returns to stdout once the pager exits.
    pub fn finish_pager(&mut self) {
        self.reset_output();
        self.finish_pager_display();
    }

    /// `FinishPagerDisplay`.
    fn finish_pager_display(&mut self) {
        self.pager_is_active = false;
        crate::interrupt::restore_default(libc::SIGPIPE);
    }

    /// Reads the next line of the input (`local_getline` / `OneInputLine` for files and
    /// pipes): the line without its `\n` or `\r\n`, `None` at the end.
    pub fn read_line(reader: &mut dyn BufRead) -> Option<Vec<u8>> {
        let mut line = Vec::new();
        loop {
            let start = line.len();
            match reader.read_until(b'\n', &mut line) {
                Ok(0) | Err(_) if start == 0 => return None,
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            // The line ends at a NUL, as a C string does: the rest of the text `fgets`
            // read is dropped and the next line is read in its place.
            if let Some(nul) = line[start..].iter().position(|&c| c == 0) {
                line.truncate(start + nul);
                continue;
            }
            if line.last() == Some(&b'\n') {
                line.pop();
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
            }
            break;
        }
        Some(line)
    }

    /// `GetBailOnError`: by default, files and init files stop at the first error.
    pub fn bail_on_error(&self, mode: InputMode) -> bool {
        if self.bail != BailOnError::Automatic {
            return self.bail == BailOnError::BailOnError;
        }
        matches!(mode, InputMode::File | InputMode::DuckdbRc)
    }

    /// The result of the last query.
    pub fn last_result(&self) -> Option<ScannedResult> {
        self.last_result
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub fn set_last_result(&self, result: Option<ScannedResult>) {
        *self.last_result.lock().unwrap_or_else(PoisonError::into_inner) = result;
    }
}

impl Default for ShellState {
    fn default() -> Self {
        Self::new()
    }
}

/// `hexDigitValue`.
fn hex_digit_value(c: u8) -> Option<u8> {
    char::from(c).to_digit(16).map(|digit| digit as u8)
}

/// An identifier quoted if it must be (DuckDB's `SQLIdentifier`).
pub fn sql_identifier(name: &str) -> String {
    rustdb_types::identifier::sql_identifier(name).into_owned()
}

/// A string literal (DuckDB's `SQLString`).
pub fn sql_string(text: &str) -> String {
    rustdb_types::identifier::write_quoted(text, '\'')
}

pub fn sql_identifier_bytes(name: &[u8]) -> ByteString {
    rustdb_types::identifier::sql_identifier_bytes(name)
}

pub fn sql_string_bytes(text: &[u8]) -> ByteString {
    rustdb_types::identifier::write_quoted_bytes(text, b'\'')
}

/// Native path expansion, keeping the suffix and OS home-directory bytes intact.
pub fn expand_path_bytes(path: &[u8]) -> ByteString {
    match path.strip_prefix(b"~") {
        Some(rest) if rest.is_empty() || rest.starts_with(b"/") => {
            let home = std::env::var_os("HOME").unwrap_or_default();
            let mut result = ByteString::from(home.as_encoded_bytes());
            result.push_bytes(rest);
            result
        }
        _ => path.into(),
    }
}

/// `LocalFileSystem::GetHomeDirectory`: `$HOME`.
pub fn home_directory() -> String {
    std::env::var("HOME").unwrap_or_default()
}

/// `system()`: runs `command` with `/bin/sh -c`, returning its exit status as `system`
/// reports it (the wait status), -1 if it could not start. Like `system`, it leaves the
/// shell's buffered output in its buffer.
pub fn run_system(command: &str) -> i32 {
    match Command::new("/bin/sh").arg("-c").arg(command).status() {
        Ok(status) => {
            use std::os::unix::process::ExitStatusExt;
            status.into_raw()
        }
        Err(_) => -1,
    }
}

/// `GenerateRandomBytes` for a temporary file name.
fn random_u64() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos()),
    );
    hasher.finish()
}

/// `ShellState::Exit`: exits the process; like C's `exit`, it flushes the output.
pub fn exit(code: i32) -> ! {
    crate::output::flush_shell_output();
    printer::flush_stdout();
    std::process::exit(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integers() {
        assert_eq!(ShellState::string_to_int("10"), 10);
        assert_eq!(ShellState::string_to_int("-0x10"), -16);
        assert_eq!(ShellState::string_to_int("2KiB"), 2048);
        assert_eq!(ShellState::string_to_int("3m"), 3_000_000);
        assert_eq!(ShellState::string_to_int("abc"), 0);
    }

    #[test]
    fn c_strings() {
        assert_eq!(ShellState::escape_c_string("a\tb\"é"), "\"a\\tb\\\"\\303\\251\"");
    }
}
