//! The command-line options (DuckDB's `tools/shell/shell_command_line_option.cpp`): their
//! table, the callbacks run before the database opens and after the init file, and
//! `-help`.

use rustdb::Value;

use crate::highlight::{
    self, HighlightElementType, HighlightMode, PrintColor, PrintIntensity, ShellHighlight,
};
use crate::output::PrintOutput;
use crate::shell::Shell;
use crate::state::{BailOnError, MetadataResult, RenderMode, SEP_RECORD, SEP_UNIT, ShellFlag, ShellState};

/// The callback of an option (`metadata_command_t`), given the option's name and
/// arguments.
pub type OptionCallback = fn(&mut Shell, &[String]) -> MetadataResult;

/// A command-line option (`CommandLineOption`).
#[derive(Debug)]
pub struct CommandLineOption {
    pub option: &'static str,
    pub argument_count: usize,
    pub arguments: &'static str,
    /// Run while the command line is read, before the database opens.
    pub pre_init_callback: Option<OptionCallback>,
    /// Run after the init file, in the order of the command line.
    pub post_init_callback: Option<OptionCallback>,
    pub description: &'static str,
}

/// `ToggleOutputMode<mode>`.
fn set_mode(state: &mut ShellState, mode: RenderMode) -> MetadataResult {
    state.mode = mode;
    state.c_mode = mode;
    MetadataResult::Success
}

fn toggle_ascii_mode(shell: &mut Shell, _args: &[String]) -> MetadataResult {
    let state = &mut shell.state;
    set_mode(state, RenderMode::Ascii);
    SEP_UNIT.clone_into(&mut state.col_separator);
    SEP_RECORD.clone_into(&mut state.row_separator);
    MetadataResult::Success
}

fn toggle_box_mode(shell: &mut Shell, _args: &[String]) -> MetadataResult {
    set_mode(&mut shell.state, RenderMode::Box)
}

fn toggle_column_mode(shell: &mut Shell, _args: &[String]) -> MetadataResult {
    set_mode(&mut shell.state, RenderMode::Column)
}

fn toggle_csv_mode(shell: &mut Shell, _args: &[String]) -> MetadataResult {
    set_mode(&mut shell.state, RenderMode::Csv);
    ",".clone_into(&mut shell.state.col_separator);
    MetadataResult::Success
}

fn toggle_html_mode(shell: &mut Shell, _args: &[String]) -> MetadataResult {
    set_mode(&mut shell.state, RenderMode::Html)
}

fn toggle_json_mode(shell: &mut Shell, _args: &[String]) -> MetadataResult {
    set_mode(&mut shell.state, RenderMode::Json)
}

fn toggle_json_lines_mode(shell: &mut Shell, _args: &[String]) -> MetadataResult {
    set_mode(&mut shell.state, RenderMode::JsonLines)
}

fn toggle_line_mode(shell: &mut Shell, _args: &[String]) -> MetadataResult {
    set_mode(&mut shell.state, RenderMode::Line)
}

fn toggle_list_mode(shell: &mut Shell, _args: &[String]) -> MetadataResult {
    set_mode(&mut shell.state, RenderMode::List)
}

fn toggle_markdown_mode(shell: &mut Shell, _args: &[String]) -> MetadataResult {
    set_mode(&mut shell.state, RenderMode::Markdown)
}

fn toggle_quote_mode(shell: &mut Shell, _args: &[String]) -> MetadataResult {
    set_mode(&mut shell.state, RenderMode::Quote)
}

fn toggle_table_mode(shell: &mut Shell, _args: &[String]) -> MetadataResult {
    set_mode(&mut shell.state, RenderMode::Table)
}

fn enable_bail(shell: &mut Shell, _args: &[String]) -> MetadataResult {
    shell.state.bail = BailOnError::BailOnError;
    MetadataResult::Success
}

fn enable_batch(shell: &mut Shell, _args: &[String]) -> MetadataResult {
    shell.state.stdin_is_interactive = false;
    MetadataResult::Success
}

fn disable_batch(shell: &mut Shell, _args: &[String]) -> MetadataResult {
    shell.state.stdin_is_interactive = true;
    MetadataResult::Success
}

fn set_read_only_mode(shell: &mut Shell, _args: &[String]) -> MetadataResult {
    shell.state.config.access_mode = rustdb::AccessMode::ReadOnly;
    MetadataResult::Success
}

fn enable_header(shell: &mut Shell, _args: &[String]) -> MetadataResult {
    shell.state.show_header = true;
    MetadataResult::Success
}

fn disable_header(shell: &mut Shell, _args: &[String]) -> MetadataResult {
    shell.state.show_header = false;
    MetadataResult::Success
}

fn disable_stdin(shell: &mut Shell, _args: &[String]) -> MetadataResult {
    shell.state.read_stdin = false;
    MetadataResult::Success
}

fn enable_echo(shell: &mut Shell, _args: &[String]) -> MetadataResult {
    shell.state.set_flag(ShellFlag::Echo);
    MetadataResult::Success
}

/// `config.SetOptionByName(name, true)`.
fn set_config_flag(shell: &mut Shell, name: &str) -> MetadataResult {
    if let Err(error) = shell.state.config.set_option_by_name(name, Value::boolean(true)) {
        shell.state.print_err(format!("Exited due to error: {error}"));
        crate::state::exit(1);
    }
    MetadataResult::Success
}

fn allow_unredacted(shell: &mut Shell, _args: &[String]) -> MetadataResult {
    set_config_flag(shell, "allow_unredacted_secrets")
}

fn allow_unsigned(shell: &mut Shell, _args: &[String]) -> MetadataResult {
    set_config_flag(shell, "allow_unsigned_extensions")
}

fn show_version_and_exit(_shell: &mut Shell, _args: &[String]) -> MetadataResult {
    ShellState::printf_stdout(&format!(
        "{} ({}) {}\n",
        rustdb::Database::library_version(),
        rustdb::Database::release_codename(),
        rustdb::Database::source_id()
    ));
    MetadataResult::Exit
}

/// `PrintHelpAndExit`: DuckDB's `PrintUsage` exits with code 0 after destroying the
/// shell's state; returning `Exit` ends the shell the same way.
fn print_help_and_exit(shell: &mut Shell, _args: &[String]) -> MetadataResult {
    print_usage(&shell.state);
    MetadataResult::Exit
}

/// `-ui`: runs the UI command, exiting if it fails.
fn launch_ui(shell: &mut Shell, _args: &[String]) -> MetadataResult {
    let command = shell.state.ui_command.clone();
    let rc = shell.run_initial_command(&command, true);
    if rc != 0 {
        crate::state::exit(rc);
    }
    MetadataResult::Success
}

fn set_newline_separator(shell: &mut Shell, args: &[String]) -> MetadataResult {
    args[1].clone_into(&mut shell.state.row_separator);
    MetadataResult::Success
}

/// `-storage-version VER` (`SerializationCompatibility::FromString`).
fn set_storage_version(shell: &mut Shell, args: &[String]) -> MetadataResult {
    let storage_version = &args[1];
    let parsed = if storage_version.is_empty() {
        Err(rustdb::Error::invalid_input("Version string can not be empty"))
    } else {
        rustdb_serde::SerializationCompatibility::from_version_name(storage_version).map(|_| ())
    };
    let set = parsed.and_then(|()| {
        shell.state.config.set_option_by_name(
            "storage_compatibility_version",
            Value::varchar(storage_version.clone()),
        )
    });
    if let Err(error) = set {
        shell.state.print_err(format!(
            "{}: Error: unknown argument ({storage_version}) for '-storage-version': {error}\n",
            shell.state.program_name
        ));
        return MetadataResult::Exit;
    }
    MetadataResult::Success
}

/// `SetColorScheme<mode>`.
fn set_color_scheme(shell: &mut Shell, mode: HighlightMode) -> MetadataResult {
    shell.state.highlight_mode = mode;
    highlight::toggle_mode(mode);
    MetadataResult::Success
}

fn set_dark_mode(shell: &mut Shell, _args: &[String]) -> MetadataResult {
    set_color_scheme(shell, HighlightMode::DarkMode)
}

fn set_light_mode(shell: &mut Shell, _args: &[String]) -> MetadataResult {
    set_color_scheme(shell, HighlightMode::LightMode)
}

/// `-f FILENAME`: runs the file and exits if it fails.
fn process_file(shell: &mut Shell, args: &[String]) -> MetadataResult {
    shell.state.read_stdin = false;
    if !shell.process_file(&args[1], crate::state::InputMode::File, false) {
        crate::state::exit(1);
    }
    MetadataResult::Success
}

fn set_init_file(shell: &mut Shell, args: &[String]) -> MetadataResult {
    args[1].clone_into(&mut shell.state.init_file);
    MetadataResult::Success
}

fn skip_init(shell: &mut Shell, _args: &[String]) -> MetadataResult {
    shell.state.run_init = false;
    MetadataResult::Success
}

/// `RunCommand<EXIT>` of `-c`, `-s` and `-cmd`: a failing command exits, as `-c` and `-s`
/// bail on errors unless `.bail` says otherwise.
fn run_command(shell: &mut Shell, args: &[String], exit: bool) -> MetadataResult {
    if exit {
        shell.state.read_stdin = false;
    }
    let bail = match shell.state.bail {
        BailOnError::Automatic => true,
        bail => bail == BailOnError::BailOnError,
    };
    let rc = shell.run_initial_command(&args[1], bail);
    if rc != 0 {
        crate::state::exit(rc);
    }
    MetadataResult::Success
}

fn run_command_and_continue(shell: &mut Shell, args: &[String]) -> MetadataResult {
    run_command(shell, args, false)
}

fn run_command_and_exit(shell: &mut Shell, args: &[String]) -> MetadataResult {
    run_command(shell, args, true)
}

/// `ShellState::SetNullValue` (`-nullvalue` and `.nullvalue`).
pub fn set_null_value(state: &mut ShellState, args: &[String]) -> MetadataResult {
    args[1].clone_into(&mut state.null_value);
    MetadataResult::Success
}

/// `ShellState::SetSeparator` (`-separator` and `.separator`).
pub fn set_separator(state: &mut ShellState, args: &[String]) -> MetadataResult {
    if args.len() < 2 || args.len() > 3 {
        return MetadataResult::PrintUsage;
    }
    args[1].clone_into(&mut state.col_separator);
    if let Some(row_separator) = args.get(2) {
        row_separator.clone_into(&mut state.row_separator);
    }
    MetadataResult::Success
}

fn set_null_value_option(shell: &mut Shell, args: &[String]) -> MetadataResult {
    set_null_value(&mut shell.state, args)
}

fn set_separator_option(shell: &mut Shell, args: &[String]) -> MetadataResult {
    set_separator(&mut shell.state, args)
}

fn enable_safe_mode(shell: &mut Shell, _args: &[String]) -> MetadataResult {
    shell.enable_safe_mode();
    MetadataResult::Success
}

const fn option(
    option: &'static str,
    argument_count: usize,
    arguments: &'static str,
    pre_init_callback: Option<OptionCallback>,
    post_init_callback: Option<OptionCallback>,
    description: &'static str,
) -> CommandLineOption {
    CommandLineOption {
        option,
        argument_count,
        arguments,
        pre_init_callback,
        post_init_callback,
        description,
    }
}

/// `command_line_options`, in DuckDB's order.
pub static COMMAND_LINE_OPTIONS: [CommandLineOption; 39] = [
    option(
        "ascii",
        0,
        "",
        None,
        Some(toggle_ascii_mode),
        "set output mode to 'ascii'",
    ),
    option(
        "bail",
        0,
        "",
        None,
        Some(enable_bail),
        "stop after hitting an error",
    ),
    option(
        "batch",
        0,
        "",
        Some(enable_batch),
        Some(enable_batch),
        "force batch I/O'",
    ),
    option(
        "box",
        0,
        "",
        None,
        Some(toggle_box_mode),
        "set output mode to 'box'",
    ),
    option(
        "column",
        0,
        "",
        None,
        Some(toggle_column_mode),
        "set output mode to 'column'",
    ),
    option(
        "cmd",
        1,
        "COMMAND",
        None,
        Some(run_command_and_continue),
        "run \"COMMAND\" before reading stdin",
    ),
    option(
        "csv",
        0,
        "",
        None,
        Some(toggle_csv_mode),
        "set output mode to 'csv'",
    ),
    option(
        "c",
        1,
        "COMMAND",
        Some(enable_batch),
        Some(run_command_and_exit),
        "run \"COMMAND\" and exit",
    ),
    option(
        "dark-mode",
        0,
        "",
        Some(set_dark_mode),
        Some(set_dark_mode),
        "use dark mode colors",
    ),
    option(
        "echo",
        0,
        "",
        None,
        Some(enable_echo),
        "print commands before execution",
    ),
    option(
        "f",
        1,
        "FILENAME",
        Some(enable_batch),
        Some(process_file),
        "read/process named file and exit",
    ),
    option(
        "init",
        1,
        "FILENAME",
        Some(set_init_file),
        None,
        "read/process named file",
    ),
    option("header", 0, "", None, Some(enable_header), "turn headers on"),
    option(
        "h",
        0,
        "",
        Some(enable_batch),
        Some(print_help_and_exit),
        "show help message",
    ),
    option(
        "help",
        0,
        "",
        Some(enable_batch),
        Some(print_help_and_exit),
        "show help message",
    ),
    option(
        "html",
        0,
        "",
        None,
        Some(toggle_html_mode),
        "set output mode to HTML",
    ),
    option(
        "interactive",
        0,
        "",
        None,
        Some(disable_batch),
        "force interactive I/O",
    ),
    option(
        "json",
        0,
        "",
        None,
        Some(toggle_json_mode),
        "set output mode to 'json'",
    ),
    option(
        "jsonlines",
        0,
        "",
        None,
        Some(toggle_json_lines_mode),
        "set output mode to 'jsonlines'",
    ),
    option(
        "light-mode",
        0,
        "",
        Some(set_light_mode),
        Some(set_light_mode),
        "use light mode colors",
    ),
    option(
        "line",
        0,
        "",
        None,
        Some(toggle_line_mode),
        "set output mode to 'line'",
    ),
    option(
        "list",
        0,
        "",
        None,
        Some(toggle_list_mode),
        "set output mode to 'list'",
    ),
    option(
        "markdown",
        0,
        "",
        None,
        Some(toggle_markdown_mode),
        "set output mode to 'markdown'",
    ),
    option(
        "newline",
        1,
        "SEP",
        None,
        Some(set_newline_separator),
        "set output row separator. Default: '\\n'",
    ),
    option(
        "no-init",
        0,
        "",
        Some(skip_init),
        None,
        "skip processing the init file",
    ),
    option(
        "no-stdin",
        0,
        "",
        None,
        Some(disable_stdin),
        "exit after processing options instead of reading stdin",
    ),
    option("noheader", 0, "", None, Some(disable_header), "turn headers off"),
    option(
        "nullvalue",
        1,
        "TEXT",
        None,
        Some(set_null_value_option),
        "set text string for NULL values. Default 'NULL'",
    ),
    option(
        "quote",
        0,
        "",
        None,
        Some(toggle_quote_mode),
        "set output mode to 'quote'",
    ),
    option(
        "readonly",
        0,
        "",
        Some(set_read_only_mode),
        None,
        "open the database read-only",
    ),
    option(
        "s",
        1,
        "COMMAND",
        Some(enable_batch),
        Some(run_command_and_exit),
        "run \"COMMAND\" and exit",
    ),
    option("safe", 0, "", Some(enable_safe_mode), None, "enable safe-mode"),
    option(
        "separator",
        1,
        "SEP",
        None,
        Some(set_separator_option),
        "set output column separator. Default: '|'",
    ),
    option(
        "storage-version",
        1,
        "VER",
        Some(set_storage_version),
        None,
        "database storage compatibility version to use. Default: 'v0.10.0'",
    ),
    option(
        "table",
        0,
        "",
        None,
        Some(toggle_table_mode),
        "set output mode to 'table'",
    ),
    option(
        "ui",
        0,
        "",
        None,
        Some(launch_ui),
        "launches a web interface using the ui extension (configurable with .ui_command)",
    ),
    option(
        "unredacted",
        0,
        "",
        Some(allow_unredacted),
        None,
        "allow printing unredacted secrets",
    ),
    option(
        "unsigned",
        0,
        "",
        Some(allow_unsigned),
        None,
        "allow loading of unsigned extensions",
    ),
    option(
        "version",
        0,
        "",
        None,
        Some(show_version_and_exit),
        "show DuckDB version",
    ),
];

/// `FindOption`.
fn find_option(name: &str) -> Option<&'static CommandLineOption> {
    COMMAND_LINE_OPTIONS.iter().find(|option| option.option == name)
}

/// `ShellState::FindCommandLineOption`: the option `option` names (with `_` for `-` as a
/// legacy spelling), or the error with the closest names.
pub fn find_command_line_option(
    state: &ShellState,
    option: &str,
) -> std::result::Result<&'static CommandLineOption, String> {
    if let Some(found) = find_option(option).or_else(|| find_option(&option.replace('_', "-"))) {
        return Ok(found);
    }
    let option_names: Vec<String> = COMMAND_LINE_OPTIONS
        .iter()
        .map(|option| format!("-{}", option.option))
        .collect();
    let candidates = rustdb_common::string_util::candidates_error_message(
        &option_names,
        &format!("-{option}"),
        "Did you mean",
    );
    Err(format!(
        "Unknown Option Error: Unrecognized option '-{option}'\n{candidates}\nRun '{} -help' for a list of options.\n",
        state.program_name
    ))
}

/// `ShellState::PrintUsage`: the usage and the options.
pub fn print_usage(state: &ShellState) {
    const INITIAL_SPACING: usize = 2;
    const MIN_SPACING: usize = 4;
    let highlighter = ShellHighlight::new(&state.out);
    highlighter.print_text_colored(
        "Usage: ",
        PrintOutput::Stdout,
        PrintColor::STANDARD,
        PrintIntensity::Bold,
    );
    highlighter.print_text(
        &state.program_name,
        PrintOutput::Stdout,
        HighlightElementType::Keyword,
    );
    highlighter.print_text(
        " [OPTIONS] FILENAME [SQL]\n\n",
        PrintOutput::Stdout,
        HighlightElementType::StringConstant,
    );
    highlighter.print_text_colored(
        "FILENAME",
        PrintOutput::Stdout,
        PrintColor::STANDARD,
        PrintIntensity::Bold,
    );
    state.print(" is the name of a DuckDB database. A new database is created\nif the file does not previously exist.\n\n");
    highlighter.print_text_colored(
        "OPTIONS:\n",
        PrintOutput::Stdout,
        PrintColor::STANDARD,
        PrintIntensity::Bold,
    );
    let lhs_length = |option: &CommandLineOption| {
        INITIAL_SPACING
            + 1
            + option.option.len()
            + option.arguments.len()
            + usize::from(!option.arguments.is_empty())
    };
    let max_lhs_length = COMMAND_LINE_OPTIONS.iter().map(lhs_length).max().unwrap_or(0);
    for option in &COMMAND_LINE_OPTIONS {
        let padding = max_lhs_length - lhs_length(option) + MIN_SPACING;
        highlighter.print_text(
            &format!("{}-{}", " ".repeat(INITIAL_SPACING), option.option),
            PrintOutput::Stdout,
            HighlightElementType::Keyword,
        );
        if !option.arguments.is_empty() {
            highlighter.print_text(
                &format!(" {}", option.arguments),
                PrintOutput::Stdout,
                HighlightElementType::StringConstant,
            );
        }
        state.print(format!("{}{}\n", " ".repeat(padding), option.description));
    }
}
