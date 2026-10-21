//! The dot commands (DuckDB's `tools/shell/shell_metadata_command.cpp`): their table,
//! callbacks, `.help` and the completion of their names.

#![allow(
    clippy::unnecessary_wraps,
    reason = "every callback has the signature of the command table, `MetadataCommandFn`"
)]

use std::fmt::Write as _;

use rustdb::Result;

use crate::highlight::{
    self, COLOR_INFO, ColorInfo, HighlightElementType, HighlightMode, PrintColor, PrintIntensity,
    ShellHighlight,
};
use crate::linenoise;
use crate::output::PrintOutput;
use crate::prompt::{MainPromptOptions, Prompt};
use crate::shell::{Shell, value_to_string};
use crate::state::{
    BailOnError, LargeNumberRendering, MetadataResult, OptionType, PagerMode, ReadLineVersion, ShellFlag,
    ShellState, StartupText, SuccessState, sql_identifier, sql_string,
};

/// A dot command's callback (`metadata_command_t`); an error is printed like an
/// exception thrown by the callback.
pub type MetadataCommandFn = fn(&mut Shell, &[String]) -> Result<MetadataResult>;

/// A dot command (`MetadataCommand`).
#[derive(Debug)]
pub struct MetadataCommand {
    pub command: &'static str,
    /// The exact number of arguments, the command included; 0 for any.
    pub argument_count: usize,
    /// `None` for `.edit`, which the line editor runs; as a command it is unsupported.
    pub callback: Option<MetadataCommandFn>,
    pub usage: &'static str,
    pub description: &'static str,
    /// The length of the shortest prefix that selects the command; 0 for the full name.
    pub match_size: usize,
    /// The text `.help --all` adds; `None` hides the command from `.help`.
    pub extra_description: Option<&'static str>,
}

fn toggle_bail(shell: &mut Shell, args: &[String]) -> Result<MetadataResult> {
    let state = &mut shell.state;
    state.bail = if args[1] == "auto" {
        BailOnError::Automatic
    } else if state.string_to_bool(&args[1]) {
        BailOnError::BailOnError
    } else {
        BailOnError::DontBailOnError
    };
    Ok(MetadataResult::Success)
}

/// `.binary`: binary output only differs from text output on Windows.
fn toggle_binary(shell: &mut Shell, args: &[String]) -> Result<MetadataResult> {
    shell.state.string_to_bool(&args[1]);
    Ok(MetadataResult::Success)
}

fn change_directory(shell: &mut Shell, args: &[String]) -> Result<MetadataResult> {
    if shell.state.safe_mode {
        shell.state.print_err(".cd cannot be used in -safe mode\n");
        return Ok(MetadataResult::Fail);
    }
    Ok(success_or_fail(shell.change_directory(&args[1])))
}

fn toggle_changes(shell: &mut Shell, args: &[String]) -> Result<MetadataResult> {
    shell.state.set_or_clear_flag(ShellFlag::CountChanges, &args[1]);
    Ok(MetadataResult::Success)
}

fn show_databases(shell: &mut Shell, _args: &[String]) -> Result<MetadataResult> {
    Ok(success_or_fail(shell.show_databases()?))
}

fn success_or_fail(result: SuccessState) -> MetadataResult {
    if result == SuccessState::Success {
        MetadataResult::Success
    } else {
        MetadataResult::Fail
    }
}

/// `SetSeparator` of `.decimal_sep` and `.thousand_sep`.
fn set_number_separator(
    state: &ShellState,
    args: &[String],
    separator_name: &str,
    separator: &mut Option<u8>,
) -> MetadataResult {
    if args.len() == 1 {
        // `%c` of the NUL "none" prints a NUL byte.
        let mut line = format!("current {separator_name} separator: ").into_bytes();
        line.push(separator.unwrap_or(0));
        line.push(b'\n');
        state.print(line);
    } else if args.len() != 2 {
        return MetadataResult::PrintUsage;
    } else if args[1] == "space" {
        *separator = Some(b' ');
    } else if args[1] == "none" {
        *separator = None;
    } else if args[1].len() != 1 {
        state.print_err(format!(
            ".{separator_name}_sep SEP must be one byte, \"space\" or \"none\"\n"
        ));
        return MetadataResult::Fail;
    } else {
        *separator = Some(args[1].as_bytes()[0]);
    }
    MetadataResult::Success
}

fn set_decimal_sep(shell: &mut Shell, args: &[String]) -> Result<MetadataResult> {
    let mut separator = shell.state.decimal_separator;
    let result = set_number_separator(&shell.state, args, "decimal", &mut separator);
    shell.state.decimal_separator = separator;
    Ok(result)
}

fn set_thousand_sep(shell: &mut Shell, args: &[String]) -> Result<MetadataResult> {
    let mut separator = shell.state.thousand_separator;
    let result = set_number_separator(&shell.state, args, "thousand", &mut separator);
    shell.state.thousand_separator = separator;
    Ok(result)
}

fn set_large_number_rendering(shell: &mut Shell, args: &[String]) -> Result<MetadataResult> {
    let state = &mut shell.state;
    state.large_number_rendering = match args[1].as_str() {
        "all" => LargeNumberRendering::All,
        "footer" => LargeNumberRendering::Footer,
        arg if state.string_to_bool(arg) => LargeNumberRendering::Default,
        _ => LargeNumberRendering::None,
    };
    Ok(MetadataResult::Success)
}

/// `.dump`: the schema and contents of the database as SQL.
fn dump_table(shell: &mut Shell, args: &[String]) -> Result<MetadataResult> {
    let mut like = String::new();
    let saved_show_header = shell.state.show_header;
    let saved_shell_flags = shell.state.shell_flags;
    shell.state.clear_flag(ShellFlag::Newlines);
    shell.state.clear_flag(ShellFlag::Echo);
    for arg in &args[1..] {
        if let Some(option) = arg.strip_prefix('-') {
            let option = option.strip_prefix('-').unwrap_or(option);
            if option == "newlines" {
                shell.state.set_flag(ShellFlag::Newlines);
            } else {
                shell
                    .state
                    .print_err(format!("Unknown option \"{arg}\" on \".dump\"\n"));
                return Ok(MetadataResult::Fail);
            }
        } else if like.is_empty() {
            like = format!("name LIKE {} ESCAPE '\\'", sql_string(arg));
        } else {
            like = format!("{like} OR name LIKE {} ESCAPE '\\'", sql_string(arg));
        }
    }
    shell.state.print("BEGIN TRANSACTION;\n");
    shell.state.show_header = false;
    shell.state.n_err = 0;
    if like.is_empty() {
        "true".clone_into(&mut like);
    }
    // CREATE SCHEMA for the schemas other than main first.
    let query = format!(
        "SELECT DISTINCT table_schema FROM information_schema.tables WHERE table_schema != 'main' AND table_schema \
         NOT LIKE 'pg_%' AND table_schema != 'information_schema' AND table_name IN (SELECT name FROM \
         sqlite_schema WHERE ({like}) AND type=='table') ORDER BY table_schema"
    );
    // (Like DuckDB, after `BEGIN TRANSACTION;`, a missing connection is an error.)
    if let Ok(result) = shell.conn()?.query(&query) {
        for row in 0..result.row_count() {
            let schema = value_to_string(&result.value(0, row));
            let create_schema = format!("CREATE SCHEMA IF NOT EXISTS {};", sql_identifier(&schema));
            shell.state.print(format!("{create_schema};\n"));
        }
    }
    shell.run_schema_dump_query(&format!(
        "SELECT name, type, sql FROM sqlite_schema WHERE ({like}) AND type=='table'  AND sql NOT NULL ORDER BY \
         tbl_name='sqlite_sequence'"
    ));
    shell.run_table_dump_query(&format!(
        "SELECT sql FROM sqlite_schema WHERE ({like}) AND sql NOT NULL  AND type IN ('index','trigger','view')"
    ));
    shell.state.print(if shell.state.n_err > 0 {
        "ROLLBACK; -- due to errors\n"
    } else {
        "COMMIT;\n"
    });
    shell.state.show_header = saved_show_header;
    shell.state.shell_flags = saved_shell_flags;
    Ok(MetadataResult::Success)
}

fn toggle_echo(shell: &mut Shell, args: &[String]) -> Result<MetadataResult> {
    shell.state.set_or_clear_flag(ShellFlag::Echo, &args[1]);
    Ok(MetadataResult::Success)
}

/// `.exit ?CODE?`: a non-zero code exits at once.
fn exit_process(_shell: &mut Shell, args: &[String]) -> Result<MetadataResult> {
    if args.len() > 2 {
        return Ok(MetadataResult::PrintUsage);
    }
    if let Some(code) = args.get(1) {
        let rc = ShellState::string_to_int(code) as i32;
        if rc != 0 {
            crate::state::exit(rc);
        }
    }
    Ok(MetadataResult::Exit)
}

fn toggle_headers(shell: &mut Shell, args: &[String]) -> Result<MetadataResult> {
    shell.state.show_header = shell.state.string_to_bool(&args[1]);
    shell.state.set_flag(ShellFlag::HeaderSet);
    Ok(MetadataResult::Success)
}

fn set_highlight_colors(shell: &mut Shell, args: &[String]) -> Result<MetadataResult> {
    if args.len() < 3 || args.len() > 4 {
        return Ok(MetadataResult::PrintUsage);
    }
    if !set_color(
        &mut shell.state,
        &args[1],
        &args[2],
        args.get(3).map(String::as_str),
    ) {
        return Ok(MetadataResult::Fail);
    }
    Ok(MetadataResult::Success)
}

/// `ShellHighlight::SetColor(element_type, color, intensity)` from their names.
fn set_color(state: &mut ShellState, element_type: &str, color: &str, intensity: Option<&str>) -> bool {
    let element = match highlight::try_get_highlight_element(element_type) {
        Ok(element) => element,
        Err(error) => {
            state.print_err(error);
            return false;
        }
    };
    let print_color = match highlight::try_get_print_color(color) {
        Ok(color) => color,
        Err(error) => {
            state.print_database_error(&error);
            return false;
        }
    };
    let print_intensity = match intensity {
        None => PrintIntensity::Standard,
        Some(intensity) if intensity.eq_ignore_ascii_case("standard") => PrintIntensity::Standard,
        Some(intensity) if intensity.eq_ignore_ascii_case("bold") => PrintIntensity::Bold,
        Some(intensity) if intensity.eq_ignore_ascii_case("underline") => PrintIntensity::Underline,
        Some(intensity) if intensity.eq_ignore_ascii_case("bold_underline") => PrintIntensity::BoldUnderline,
        Some(intensity) => {
            state.print_err(format!(
                "Unknown intensity '{intensity}', supported options: standard, bold, underline\n"
            ));
            return false;
        }
    };
    highlight::set_element_color(element, print_color, print_intensity, true);
    true
}

fn toggle_highlight_errors(shell: &mut Shell, args: &[String]) -> Result<MetadataResult> {
    shell.state.highlight_errors = if shell.state.string_to_bool(&args[1]) {
        OptionType::On
    } else {
        OptionType::Off
    };
    Ok(MetadataResult::Success)
}

fn toggle_highlight_mode(shell: &mut Shell, args: &[String]) -> Result<MetadataResult> {
    shell.state.highlight_mode = match args[1].as_str() {
        "mixed" => HighlightMode::MixedMode,
        "dark" => HighlightMode::DarkMode,
        "light" => HighlightMode::LightMode,
        "auto" => HighlightMode::Automatic,
        _ => return Ok(MetadataResult::PrintUsage),
    };
    highlight::toggle_mode(shell.state.highlight_mode);
    Ok(MetadataResult::Success)
}

fn toggle_highlight_result(shell: &mut Shell, args: &[String]) -> Result<MetadataResult> {
    shell.state.highlight_results = if shell.state.string_to_bool(&args[1]) {
        OptionType::On
    } else {
        OptionType::Off
    };
    Ok(MetadataResult::Success)
}

/// `.help ?-all? ?PATTERN?`, and `.help shortcuts` for the keyboard shortcuts.
fn show_help(shell: &mut Shell, args: &[String]) -> Result<MetadataResult> {
    let state = &shell.state;
    if let Some(pattern) = args.get(1) {
        if pattern.eq_ignore_ascii_case("shortcuts") {
            let highlighter = ShellHighlight::new(&state.out);
            let mut current_category: Option<&str> = None;
            for entry in linenoise::shell_shortcuts() {
                if current_category != Some(entry.category) {
                    if current_category.is_some() {
                        state.print("\n");
                    }
                    current_category = Some(entry.category);
                    highlighter.print_text(
                        entry.category,
                        PrintOutput::Stdout,
                        HighlightElementType::Keyword,
                    );
                    state.print("\n");
                }
                state.print(format!("  {:<24} {}\n", entry.key_name, entry.description));
            }
            return Ok(MetadataResult::Success);
        }
        if print_help(state, Some(pattern)) == 0 {
            state.print(format!("Nothing matches '{pattern}'\n"));
        }
    } else {
        print_help(state, None);
    }
    Ok(MetadataResult::Success)
}

fn render_last_result(shell: &mut Shell, _args: &[String]) -> Result<MetadataResult> {
    shell.render_last_result()
}

fn toggle_log(shell: &mut Shell, args: &[String]) -> Result<MetadataResult> {
    if shell.state.safe_mode {
        shell.state.print_err(".log cannot be used in -safe mode\n");
        return Ok(MetadataResult::Fail);
    }
    if let Some(log) = shell.state.log.take() {
        log.close();
    }
    shell.state.log = shell.state.open_output_file(&args[1]);
    Ok(MetadataResult::Success)
}

fn set_max_rows(shell: &mut Shell, args: &[String]) -> Result<MetadataResult> {
    let state = &mut shell.state;
    if args.len() > 3 {
        return Ok(MetadataResult::PrintUsage);
    }
    if args.len() == 1 {
        state.print(format!("current max rows: {}\n", state.max_rows));
        return Ok(MetadataResult::Success);
    }
    state.max_rows = ShellState::string_to_int(&args[1]) as usize;
    if let Some(analyze_rows) = args.get(2) {
        state.max_analyze_rows = ShellState::string_to_int(analyze_rows) as usize;
    }
    Ok(MetadataResult::Success)
}

/// `.maxwidth ?COUNT?` (DuckDB prints the width as "max rows").
fn set_max_width(shell: &mut Shell, args: &[String]) -> Result<MetadataResult> {
    let state = &mut shell.state;
    if args.len() > 2 {
        return Ok(MetadataResult::PrintUsage);
    }
    match args.get(1) {
        None => state.print(format!("current max rows: {}\n", state.max_width)),
        Some(width) => state.max_width = ShellState::string_to_int(width) as usize,
    }
    Ok(MetadataResult::Success)
}

fn set_column_rendering(shell: &mut Shell, _args: &[String]) -> Result<MetadataResult> {
    shell.state.columns = true;
    Ok(MetadataResult::Success)
}

fn set_row_rendering(shell: &mut Shell, _args: &[String]) -> Result<MetadataResult> {
    shell.state.columns = false;
    Ok(MetadataResult::Success)
}

fn import_data(shell: &mut Shell, args: &[String]) -> Result<MetadataResult> {
    Ok(success_or_fail_bool(shell.import_data(args)?))
}

fn success_or_fail_bool(success: bool) -> MetadataResult {
    if success {
        MetadataResult::Success
    } else {
        MetadataResult::Fail
    }
}

fn open_database(shell: &mut Shell, args: &[String]) -> Result<MetadataResult> {
    Ok(success_or_fail_bool(shell.open_database(args)?))
}

fn print_arguments(shell: &mut Shell, args: &[String]) -> Result<MetadataResult> {
    shell.state.print(format!("{}\n", args[1..].join(" ")));
    Ok(MetadataResult::Success)
}

/// `.prompt MAIN ?CONTINUE? ...`: the main prompt, then the continuation prompts.
fn set_prompt(shell: &mut Shell, args: &[String]) -> Result<MetadataResult> {
    let state = &mut shell.state;
    if let Some(main) = args.get(1) {
        state.main_prompt = Prompt::parse(main, &mut MainPromptOptions)?;
    }
    if let Some(prompt) = args.get(2) {
        state.continue_prompt = ShellState::truncated_prompt(prompt);
    }
    if let Some(prompt) = args.get(3) {
        state.continue_prompt_selected = ShellState::truncated_prompt(prompt);
    }
    if let Some(prompt) = args.get(4) {
        state.scroll_up_prompt = ShellState::truncated_prompt(prompt);
    }
    if let Some(prompt) = args.get(5) {
        state.scroll_down_prompt = ShellState::truncated_prompt(prompt);
    }
    linenoise::set_prompt(
        &state.continue_prompt,
        &state.continue_prompt_selected,
        &state.scroll_up_prompt,
        &state.scroll_down_prompt,
    );
    Ok(MetadataResult::Success)
}

fn configure_progress_bar(shell: &mut Shell, args: &[String]) -> Result<MetadataResult> {
    let progress_bar = &mut shell.state.progress_bar;
    match (args.len(), args.get(1).map(String::as_str)) {
        (2, Some("--clear")) => progress_bar.clear_components(),
        (3, Some("--add")) => progress_bar.add_component(&args[2])?,
        _ => return Ok(MetadataResult::PrintUsage),
    }
    Ok(MetadataResult::Success)
}

fn set_output_mode(shell: &mut Shell, args: &[String]) -> Result<MetadataResult> {
    let state = &mut shell.state;
    if args.len() > 3 {
        return Ok(MetadataResult::PrintUsage);
    }
    if args.len() == 1 {
        state.print(format!("current output mode: {}\n", state.mode.name()));
    } else if !state.set_output_mode(&args[1], args.get(2).map(String::as_str)) {
        return Ok(MetadataResult::Fail);
    }
    Ok(MetadataResult::Success)
}

fn quit_process(_shell: &mut Shell, _args: &[String]) -> Result<MetadataResult> {
    Ok(MetadataResult::Exit)
}

fn set_output(shell: &mut Shell, args: &[String]) -> Result<MetadataResult> {
    Ok(success_or_fail_bool(shell.set_output_file(args, '\0')))
}

fn set_output_once(shell: &mut Shell, args: &[String]) -> Result<MetadataResult> {
    Ok(success_or_fail_bool(shell.set_output_file(args, 'o')))
}

fn set_output_excel(shell: &mut Shell, args: &[String]) -> Result<MetadataResult> {
    Ok(success_or_fail_bool(shell.set_output_file(args, 'e')))
}

fn read_from_file(shell: &mut Shell, args: &[String]) -> Result<MetadataResult> {
    Ok(success_or_fail_bool(shell.read_from_file(&args[1])))
}

fn display_schemas(shell: &mut Shell, args: &[String]) -> Result<MetadataResult> {
    Ok(success_or_fail_bool(shell.display_schemas(args)?))
}

/// `.shell` / `.system CMD ARGS...`: arguments without spaces are double-quoted.
fn run_shell_command(shell: &mut Shell, args: &[String]) -> Result<MetadataResult> {
    let state = &shell.state;
    if state.safe_mode {
        state.print_err(".sh/.system cannot be used in -safe mode\n");
        return Ok(MetadataResult::Fail);
    }
    if args.len() < 2 {
        return Ok(MetadataResult::PrintUsage);
    }
    let quote = |arg: &str| {
        if arg.contains(' ') {
            arg.to_owned()
        } else {
            format!("\"{arg}\"")
        }
    };
    let command = args[1..]
        .iter()
        .map(|arg| quote(arg))
        .collect::<Vec<_>>()
        .join(" ");
    let rc = crate::state::run_system(&command);
    if rc != 0 {
        state.print_err(format!("System command returns {rc}\n"));
    }
    Ok(MetadataResult::Success)
}

fn show_configuration(shell: &mut Shell, _args: &[String]) -> Result<MetadataResult> {
    shell.state.show_configuration();
    Ok(MetadataResult::Success)
}

fn set_startup_text(shell: &mut Shell, args: &[String]) -> Result<MetadataResult> {
    let state = &mut shell.state;
    let previous = state.startup_text;
    state.startup_text = match args[1].as_str() {
        "all" => StartupText::All,
        "version" => StartupText::Version,
        "none" => StartupText::None,
        _ => return Ok(MetadataResult::PrintUsage),
    };
    if state.displayed_loading_resources_message
        && previous == StartupText::All
        && state.startup_text != StartupText::All
    {
        state.print_highlighted(
            "WARNING: .startup_text should be on top of your ~/.duckdbrc in order to prevent the \"Loading \
             resources\" message from being displayed\n",
            PrintOutput::Stderr,
            HighlightElementType::StartupText,
        );
    }
    Ok(MetadataResult::Success)
}

/// `.version`: the library version, then the compiler that built the shell.
fn show_version(shell: &mut Shell, _args: &[String]) -> Result<MetadataResult> {
    shell.state.print(format!(
        "DuckDB {} ({}) {}\n",
        rustdb::Database::library_version(),
        rustdb::Database::release_codename(),
        rustdb::Database::source_id()
    ));
    shell
        .state
        .print(format!("rustc-{}\n", env!("RUSTDB_SHELL_RUSTC_VERSION")));
    Ok(MetadataResult::Success)
}

fn set_widths(shell: &mut Shell, args: &[String]) -> Result<MetadataResult> {
    shell.state.col_width = args[1..]
        .iter()
        .map(|width| ShellState::string_to_int(width) as i32)
        .collect();
    Ok(MetadataResult::Success)
}

fn show_indexes(shell: &mut Shell, args: &[String]) -> Result<MetadataResult> {
    shell.display_entries(args, 'i')
}

fn show_tables(shell: &mut Shell, args: &[String]) -> Result<MetadataResult> {
    Ok(shell.display_tables(args))
}

fn set_ui_command(shell: &mut Shell, args: &[String]) -> Result<MetadataResult> {
    shell.state.ui_command = format!("CALL {}", args[1..].join(" "));
    Ok(MetadataResult::Success)
}

fn toggle_highlighting(shell: &mut Shell, args: &[String]) -> Result<MetadataResult> {
    highlight::set_highlighting(shell.state.string_to_bool(&args[1]));
    Ok(MetadataResult::Success)
}

fn toggle_error_rendering(shell: &mut Shell, args: &[String]) -> Result<MetadataResult> {
    linenoise::set_error_rendering(shell.state.string_to_bool(&args[1]));
    Ok(MetadataResult::Success)
}

fn toggle_completion_rendering(shell: &mut Shell, args: &[String]) -> Result<MetadataResult> {
    linenoise::set_completion_rendering(shell.state.string_to_bool(&args[1]));
    Ok(MetadataResult::Success)
}

fn toggle_multi_line(_shell: &mut Shell, args: &[String]) -> Result<MetadataResult> {
    if args.len() != 1 {
        return Ok(MetadataResult::PrintUsage);
    }
    linenoise::set_multi_line(true);
    Ok(MetadataResult::Success)
}

fn toggle_single_line(_shell: &mut Shell, args: &[String]) -> Result<MetadataResult> {
    if args.len() != 1 {
        return Ok(MetadataResult::PrintUsage);
    }
    linenoise::set_multi_line(false);
    Ok(MetadataResult::Success)
}

/// The colors of the deprecated `.comment`, `.keyword`, ... commands
/// (`DeprecatedHighlightColors`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DeprecatedHighlightColors {
    Comment,
    Constant,
    Keyword,
    Error,
    Cont,
    ContSel,
}

/// `SetHighlightingColor<T>`: a deprecated command sets its `.highlight_colors` element.
fn set_highlighting_color(
    shell: &mut Shell,
    args: &[String],
    color: DeprecatedHighlightColors,
) -> MetadataResult {
    let (literal, hint) = match color {
        DeprecatedHighlightColors::Comment => ("comment", "comment"),
        DeprecatedHighlightColors::Constant => {
            shell.state.print_err(format!(
                ".constant has been split into numeric_constant and string_constant, use .highlight_colors \
                 numeric_constant {0} and .highlight_colors string_constant {0} instead\n",
                args[1]
            ));
            return MetadataResult::Fail;
        }
        DeprecatedHighlightColors::Keyword => ("keyword", "keyword"),
        DeprecatedHighlightColors::Error => ("error", "error"),
        DeprecatedHighlightColors::Cont => ("cont", "continuation"),
        DeprecatedHighlightColors::ContSel => ("cont_sel", "continuation_selected"),
    };
    shell.state.print_err(format!(
        "WARNING: .{literal} [COLOR] will be removed in a future release, use .highlight_colors {hint} {} instead\n",
        args[1]
    ));
    // `TrySetHighlightColor`.
    if set_color(&mut shell.state, hint, &args[1], None) {
        MetadataResult::Success
    } else {
        MetadataResult::Fail
    }
}

fn set_comment_color(shell: &mut Shell, args: &[String]) -> Result<MetadataResult> {
    Ok(set_highlighting_color(
        shell,
        args,
        DeprecatedHighlightColors::Comment,
    ))
}

fn set_constant_color(shell: &mut Shell, args: &[String]) -> Result<MetadataResult> {
    Ok(set_highlighting_color(
        shell,
        args,
        DeprecatedHighlightColors::Constant,
    ))
}

fn set_cont_color(shell: &mut Shell, args: &[String]) -> Result<MetadataResult> {
    Ok(set_highlighting_color(
        shell,
        args,
        DeprecatedHighlightColors::Cont,
    ))
}

fn set_cont_sel_color(shell: &mut Shell, args: &[String]) -> Result<MetadataResult> {
    Ok(set_highlighting_color(
        shell,
        args,
        DeprecatedHighlightColors::ContSel,
    ))
}

fn set_error_color(shell: &mut Shell, args: &[String]) -> Result<MetadataResult> {
    Ok(set_highlighting_color(
        shell,
        args,
        DeprecatedHighlightColors::Error,
    ))
}

fn set_keyword_color(shell: &mut Shell, args: &[String]) -> Result<MetadataResult> {
    Ok(set_highlighting_color(
        shell,
        args,
        DeprecatedHighlightColors::Keyword,
    ))
}

/// `GetHue`.
#[allow(
    clippy::float_cmp,
    reason = "the channel that is the maximum, as DuckDB compares"
)]
fn hue(input: &ColorInfo) -> f64 {
    let r = f64::from(input.r) / 255.0;
    let g = f64::from(input.g) / 255.0;
    let b = f64::from(input.b) / 255.0;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let delta = max - min;
    if delta == 0.0 {
        return 0.0;
    }
    let mut hue = if max == r {
        (g - b) / delta
    } else if max == g {
        2.0 + (b - r) / delta
    } else {
        4.0 + (r - g) / delta
    };
    hue *= 60.0;
    if hue < 0.0 {
        hue += 360.0;
    }
    hue
}

/// `GetLum`.
fn luminosity(input: &ColorInfo) -> f64 {
    (0.241 * f64::from(input.r) + 0.691 * f64::from(input.g) + 0.068 * f64::from(input.b)).sqrt()
}

/// `FindColorGroup`: the last group with a word the color's name contains.
fn find_color_group(groups: &[&[&str]], name: &str) -> Result<usize> {
    groups
        .iter()
        .rposition(|group| group.iter().any(|word| name.contains(word)))
        .ok_or_else(|| rustdb::Error::internal(format!("Color {name} does not have a group")))
}

/// `.display_colors [bold|underline]`: every color in its own color, by group.
#[allow(clippy::float_cmp, reason = "DuckDB's comparator compares the hues exactly")]
fn display_colors(shell: &mut Shell, args: &[String]) -> Result<MetadataResult> {
    // The order the groups are displayed in.
    const COLOR_GROUPS: [&[&str]; 11] = [
        &["red", "maroon", "coral"],
        &["orange"],
        &["yellow", "gold", "khaki", "wheat"],
        &["green", "chartreuse", "lime", "honeydew", "olive"],
        &["cyan", "aqua", "turquoise"],
        &["blue", "navy"],
        &["pink", "orchid", "rose", "thistle", "salmon", "tan"],
        &["purple", "magenta", "plum", "fuchsia", "violet"],
        &["brown"],
        &["grey", "gray", "black"],
        &["white", "silver", "cornsilk"],
    ];
    let mut bold = false;
    let mut underline = false;
    for arg in &args[1..] {
        match arg.as_str() {
            "bold" => bold = true,
            "underline" => underline = true,
            _ => return Ok(MetadataResult::PrintUsage),
        }
    }
    let intensity = match (bold, underline) {
        (true, true) => PrintIntensity::BoldUnderline,
        (true, false) => PrintIntensity::Bold,
        (false, true) => PrintIntensity::Underline,
        (false, false) => PrintIntensity::Standard,
    };
    let mut color_list = Vec::with_capacity(COLOR_INFO.len());
    for (code, info) in COLOR_INFO.iter().enumerate() {
        color_list.push((find_color_group(&COLOR_GROUPS, info.color_name)?, code, info));
    }
    // By group, then hue, then luminosity.
    rustdb_common::libcxx_sort::sort(&mut color_list, &mut |a, b| {
        if a.0 != b.0 {
            return a.0 < b.0;
        }
        let (a_hue, b_hue) = (hue(a.2), hue(b.2));
        if a_hue != b_hue {
            return a_hue < b_hue;
        }
        luminosity(a.2) < luminosity(b.2)
    });
    let highlighter = ShellHighlight::new(&shell.state.out);
    for (_, code, info) in color_list {
        highlighter.print_text_colored(
            info.color_name,
            PrintOutput::Stdout,
            PrintColor::from_code(code as u8),
            intensity,
        );
        shell.state.print(" ");
    }
    shell.state.print("\n");
    Ok(MetadataResult::Success)
}

fn set_read_line_version(shell: &mut Shell, args: &[String]) -> Result<MetadataResult> {
    shell.state.rl_version = match args[1].as_str() {
        "linenoise" => ReadLineVersion::Linenoise,
        "fallback" => ReadLineVersion::Fallback,
        _ => return Ok(MetadataResult::PrintUsage),
    };
    Ok(MetadataResult::Success)
}

/// `.pager ?on|off|automatic|COMMAND?`, `.pager set_row_threshold N`, and the current
/// configuration without arguments.
fn set_pager(shell: &mut Shell, args: &[String]) -> Result<MetadataResult> {
    let state = &mut shell.state;
    if args.len() == 1 {
        let mode = match state.pager_mode {
            PagerMode::Off => "off",
            PagerMode::On => "on",
            PagerMode::Automatic => "automatic",
        };
        state.print(format!("Pager mode: {mode}\n"));
        if state.pager_mode == PagerMode::Automatic {
            let mut page_mode = format!("Trigger pager when rows exceed {}", state.pager_min_rows);
            if state.max_width == 0 {
                page_mode += " or result set is wider than terminal";
            } else {
                let _ = write!(
                    page_mode,
                    " or result set is wider than {} characters",
                    state.max_width
                );
            }
            state.print(page_mode);
        }
        if state.pager_mode != PagerMode::Off || !state.pager_command.is_empty() {
            state.print(format!("Pager command: {}\n", state.pager_command));
        }
        return Ok(MetadataResult::Success);
    }
    if args[1] == "set_row_threshold" {
        if args.len() != 3 {
            return Ok(MetadataResult::PrintUsage);
        }
        state.pager_min_rows = ShellState::string_to_int(&args[2]) as usize;
        return Ok(MetadataResult::Success);
    }
    if args.len() != 2 {
        return Ok(MetadataResult::PrintUsage);
    }
    match args[1].as_str() {
        "on" => {
            state.pager_mode = PagerMode::On;
            if state.pager_command.is_empty() {
                state.pager_command = ShellState::system_pager();
            }
        }
        "off" => state.pager_mode = PagerMode::Off,
        "automatic" => state.pager_mode = PagerMode::Automatic,
        command => command.clone_into(&mut state.pager_command),
    }
    Ok(MetadataResult::Success)
}

fn set_null_value(shell: &mut Shell, args: &[String]) -> Result<MetadataResult> {
    Ok(crate::command_line_option::set_null_value(&mut shell.state, args))
}

fn set_separator(shell: &mut Shell, args: &[String]) -> Result<MetadataResult> {
    Ok(crate::command_line_option::set_separator(&mut shell.state, args))
}

fn enable_safe_mode(shell: &mut Shell, _args: &[String]) -> Result<MetadataResult> {
    shell.enable_safe_mode();
    Ok(MetadataResult::Success)
}

fn toggle_timer(shell: &mut Shell, args: &[String]) -> Result<MetadataResult> {
    shell.state.enable_timer = shell.state.string_to_bool(&args[1]);
    Ok(MetadataResult::Success)
}

/// `MetadataCommand` with the fields in DuckDB's order.
const fn command(
    command: &'static str,
    argument_count: usize,
    callback: Option<MetadataCommandFn>,
    usage: &'static str,
    description: &'static str,
    match_size: usize,
    extra_description: Option<&'static str>,
) -> MetadataCommand {
    MetadataCommand {
        command,
        argument_count,
        callback,
        usage,
        description,
        match_size,
        extra_description,
    }
}

/// `metadata_commands`, in DuckDB's order.
pub static METADATA_COMMANDS: [MetadataCommand; 64] = [
    command(
        "bail",
        2,
        Some(toggle_bail),
        "on|off",
        "Stop after hitting an error.  Default OFF",
        3,
        Some(""),
    ),
    command(
        "binary",
        2,
        Some(toggle_binary),
        "on|off",
        "Turn binary output on or off.  Default OFF",
        3,
        Some(""),
    ),
    command(
        "cd",
        2,
        Some(change_directory),
        "DIRECTORY",
        "Change the working directory to DIRECTORY",
        0,
        Some(""),
    ),
    command(
        "changes",
        2,
        Some(toggle_changes),
        "on|off",
        "Show number of rows changed by SQL",
        3,
        Some(""),
    ),
    command(
        "columns",
        1,
        Some(set_column_rendering),
        "",
        "Column-wise rendering of query results",
        0,
        Some(""),
    ),
    command(
        "comment",
        2,
        Some(set_comment_color),
        "?COLOR?",
        "DEPRECATED: Sets the syntax highlighting color used for comment values",
        0,
        None,
    ),
    command(
        "constant",
        2,
        Some(set_constant_color),
        "?COLOR?",
        "DEPRECATED: Sets the syntax highlighting color used for constant values",
        0,
        None,
    ),
    command(
        "cont",
        2,
        Some(set_cont_color),
        "?COLOR?",
        "DEPRECATED: Sets the syntax highlighting color used for continuation markers",
        0,
        None,
    ),
    command(
        "cont_sel",
        2,
        Some(set_cont_sel_color),
        "?COLOR?",
        "DEPRECATED: Sets the syntax highlighting color used for continuation markers",
        0,
        None,
    ),
    command(
        "decimal_sep",
        0,
        Some(set_decimal_sep),
        "SEP",
        "Sets the decimal separator used when rendering numbers. Only for duckbox mode.",
        3,
        Some(""),
    ),
    command(
        "databases",
        1,
        Some(show_databases),
        "",
        "List names and files of attached databases",
        2,
        Some(""),
    ),
    command(
        "dump",
        0,
        Some(dump_table),
        "?TABLE?",
        "Render database content as SQL",
        0,
        Some(
            "Options:\n\t--newlines\tAllow unescaped newline characters in output\nTABLE is a LIKE pattern for the \
             tables to dump\nAdditional LIKE patterns can be given in subsequent arguments",
        ),
    ),
    command(
        "display_colors",
        0,
        Some(display_colors),
        "[bold|underline]",
        "Display all terminal colors and their names",
        0,
        Some(""),
    ),
    command(
        "echo",
        2,
        Some(toggle_echo),
        "on|off",
        "Turn command echo on or off",
        3,
        Some(""),
    ),
    command(
        "edit",
        0,
        None,
        "",
        "Opens an external text editor to edit a query.",
        0,
        Some(
            "Notes:\n\t* The editor is read from the environment variables\n\t  DUCKDB_EDITOR, EDITOR, VISUAL \
             in-order\n\t* If none of these are set, the default editor is vi\n\t* \\e can be used as an alias for \
             .edit",
        ),
    ),
    command(
        "error",
        2,
        Some(set_error_color),
        "?COLOR?",
        "DEPRECATED: Sets the syntax highlighting color used for errors",
        0,
        None,
    ),
    command(
        "excel",
        0,
        Some(set_output_excel),
        "",
        "Display the output of next command in spreadsheet",
        0,
        Some("--bom\tPut a UTF8 byte-order mark on intermediate file"),
    ),
    command(
        "exit",
        0,
        Some(exit_process),
        "?CODE?",
        "Exit this program with return-code CODE",
        0,
        Some(""),
    ),
    command(
        "headers",
        2,
        Some(toggle_headers),
        "on|off",
        "Turn display of headers on or off",
        0,
        Some(""),
    ),
    command(
        "help",
        0,
        Some(show_help),
        "?-all? ?PATTERN?",
        "Show help text for PATTERN",
        0,
        Some(""),
    ),
    command(
        "highlight",
        2,
        Some(toggle_highlighting),
        "on|off",
        "Toggle syntax highlighting in the shell on/off",
        0,
        Some(""),
    ),
    command(
        "highlight_colors",
        0,
        Some(set_highlight_colors),
        "OPTIONS",
        "Configure highlighting colors",
        0,
        Some(""),
    ),
    command(
        "highlight_errors",
        2,
        Some(toggle_highlight_errors),
        "on|off",
        "Turn highlighting of errors on or off",
        0,
        Some(""),
    ),
    command(
        "highlight_mode",
        2,
        Some(toggle_highlight_mode),
        "mixed|dark|light",
        "Toggle the highlight mode to dark or light mode",
        0,
        Some(""),
    ),
    command(
        "highlight_results",
        2,
        Some(toggle_highlight_result),
        "on|off",
        "Turn highlighting of results on or off",
        0,
        Some(""),
    ),
    command(
        "import",
        0,
        Some(import_data),
        "FILE TABLE",
        "Import data from FILE into TABLE",
        0,
        Some(
            "Options:\n\t--csv\tImport data from CSV (read_csv)\n\t--json\tImport data from JSON \
             (read_json)\n\t--parquet\tImport data from Parquet (read_parquet)\n\t--[parameter] [value]\tProvides \
             a parameter to the reader function\n\tNotes:\n\t* If TABLE does not exist, it is created.\n\t* If \
             file type is not selected, the input mode is derived from the file extension\n\t* Generic parameters \
             are passed to the reader functions",
        ),
    ),
    command(
        "indexes",
        0,
        Some(show_indexes),
        "?TABLE?",
        "Show names of indexes",
        0,
        Some(
            "Notes:\n\t* If TABLE is specified, only show indexes for\n\t  tables matching TABLE using the LIKE operator.",
        ),
    ),
    command(
        "indices",
        0,
        Some(show_indexes),
        "?TABLE?",
        "Show names of indexes",
        0,
        None,
    ),
    command(
        "keyword",
        2,
        Some(set_keyword_color),
        "?COLOR?",
        "DEPRECATED: Sets the syntax highlighting color used for keywords",
        0,
        None,
    ),
    command(
        "last",
        1,
        Some(render_last_result),
        "",
        "Render the last result without truncating",
        0,
        Some(""),
    ),
    command(
        "large_number_rendering",
        2,
        Some(set_large_number_rendering),
        "MODE",
        "Toggle readable rendering of large numbers (duckbox only)",
        0,
        Some("Mode: all|footer|off"),
    ),
    command(
        "log",
        2,
        Some(toggle_log),
        "FILE|off",
        "Turn logging on or off.  FILE can be stderr/stdout",
        0,
        Some(""),
    ),
    command(
        "maxrows",
        0,
        Some(set_max_rows),
        "COUNT",
        "Sets the maximum number of rows for display (default: 40). Only for duckbox mode.",
        0,
        Some(""),
    ),
    command(
        "maxwidth",
        0,
        Some(set_max_width),
        "COUNT",
        "Sets the maximum width in characters. 0 defaults to terminal width. Only for duckbox mode.",
        0,
        Some(""),
    ),
    command(
        "mode",
        0,
        Some(set_output_mode),
        "MODE ?TABLE?",
        "Set output mode",
        0,
        Some(
            "MODE is one of:\n\tascii\tColumns/rows delimited by 0x1F and 0x1E\n\tbox\tTables using unicode \
             box-drawing characters\n\tcsv\tComma-separated values\n\tcolumn\tOutput in columns. (See \
             .width)\n\tduckbox\tTables with extensive features\n\thtml\tHTML <table> code\n\tinsert\tSQL insert \
             statements for TABLE\n\tjson\tResults in a JSON array\n\tjsonlines\tResults in a NDJSON\n\tlatex\tLaTeX \
             tabular environment code\n\tline\tOne value per line\n\tlist\tValues delimited by \
             \"|\"\n\tmarkdown\tMarkdown table format\n\tquote\tEscape answers as for SQL\n\ttable\tASCII-art \
             table\n\ttabs\tTab-separated values\n\ttcl\tTCL list elements\n\ttrash\tNo output",
        ),
    ),
    command(
        "multiline",
        1,
        Some(toggle_multi_line),
        "",
        "Sets the render mode to multi-line",
        0,
        Some(""),
    ),
    command(
        "nullvalue",
        2,
        Some(set_null_value),
        "STRING",
        "Use STRING in place of NULL values",
        0,
        Some(""),
    ),
    command(
        "open",
        0,
        Some(open_database),
        "?OPTIONS? ?FILE?",
        "Close existing database and reopen FILE",
        2,
        Some(
            "Options:\n\t--new\tInitialize FILE to an empty database\n\t--nofollow\tDo not follow symbolic \
             links\n\t--readonly\tOpen FILE in read-only mode\n\t--sql\tSet FILE using SQL",
        ),
    ),
    command(
        "once",
        0,
        Some(set_output_once),
        "?FILE?",
        "Output for the next SQL command only to FILE",
        0,
        Some(
            "If FILE begins with '|' then open as a pipe\n\t--bom\tPut a UTF8 byte-order mark at the \
             beginning\n\t-e\tSend output to the system text editor\n\t-x\tSend output as CSV to a spreadsheet \
             (same as \".excel\")",
        ),
    ),
    command(
        "output",
        0,
        Some(set_output),
        "?FILE?",
        "Send output to FILE or stdout if FILE is omitted",
        0,
        Some(
            "If FILE begins with '|' then open as a pipe\n\t--bom\tPut a UTF8 byte-order mark at the \
             beginning\n\t-e\tSend output to the system text editor\n\t-x\tSend output as CSV to a spreadsheet \
             (same as \".excel\")",
        ),
    ),
    command(
        "pager",
        0,
        Some(set_pager),
        "OPTIONS",
        "Control pager usage for output",
        0,
        Some(
            "Options:\n\t[on|off|automatic]\tToggle pager mode (default: \
             automatic)\n\tset_[row|column]_threshold THRESHOLD\tIn automatic mode, trigger the pager when the \
             result has more rows/columns than this\n\t[pager_command]\tSet the pager command to invoke\nNote: Set \
             DUCKDB_PAGER or PAGER environment variable or <cmd> to configure default pager",
        ),
    ),
    command(
        "print",
        0,
        Some(print_arguments),
        "STRING...",
        "Print literal STRING",
        3,
        Some(""),
    ),
    command(
        "progress_bar",
        0,
        Some(configure_progress_bar),
        "OPTIONS",
        "Configure the progress bar display",
        0,
        Some(
            "OPTIONS:\n\t--add [COMPONENT]\tAdd a component to the progress bar\n\t--clear\tClear all components",
        ),
    ),
    command(
        "prompt",
        0,
        Some(set_prompt),
        "MAIN CONTINUE",
        "Replace the standard prompts",
        0,
        Some(""),
    ),
    command(
        "quit",
        0,
        Some(quit_process),
        "",
        "Exit this program",
        0,
        Some(""),
    ),
    command(
        "read",
        2,
        Some(read_from_file),
        "FILE",
        "Read input from FILE",
        3,
        Some(""),
    ),
    command(
        "read_line_version",
        2,
        Some(set_read_line_version),
        "linenoise|fallback",
        "Sets the library used for processing interactive input",
        0,
        Some(""),
    ),
    command(
        "render_completion",
        2,
        Some(toggle_completion_rendering),
        "on|off",
        "Toggle displaying of completion prompts in the shell on/off",
        0,
        Some(""),
    ),
    command(
        "render_errors",
        2,
        Some(toggle_error_rendering),
        "on|off",
        "Toggle rendering of errors in the shell on/off",
        0,
        Some(""),
    ),
    command(
        "rows",
        1,
        Some(set_row_rendering),
        "",
        "Row-wise rendering of query results (default)",
        0,
        Some(""),
    ),
    command(
        "safe_mode",
        0,
        Some(enable_safe_mode),
        "",
        "Enable safe-mode",
        0,
        Some(""),
    ),
    command(
        "separator",
        0,
        Some(set_separator),
        "COL ?ROW?",
        "Change the column and row separators",
        0,
        Some(""),
    ),
    command(
        "schema",
        0,
        Some(display_schemas),
        "?PATTERN?",
        "Show the CREATE statements matching PATTERN",
        0,
        Some("Options:\n\t--indent\tTry to pretty-print the schema"),
    ),
    command(
        "shell",
        0,
        Some(run_shell_command),
        "CMD ARGS...",
        "Run CMD ARGS... in a system shell",
        0,
        Some(""),
    ),
    command(
        "show",
        1,
        Some(show_configuration),
        "",
        "Show the current values for various settings",
        0,
        Some(""),
    ),
    command(
        "singleline",
        1,
        Some(toggle_single_line),
        "",
        "Sets the render mode to single-line",
        0,
        Some(""),
    ),
    command(
        "startup_text",
        2,
        Some(set_startup_text),
        "none|version|all",
        "Start-up text to display. Set this as the first line in .duckdbrc",
        0,
        Some(""),
    ),
    command(
        "system",
        0,
        Some(run_shell_command),
        "CMD ARGS...",
        "Run CMD ARGS... in a system shell",
        0,
        Some(""),
    ),
    command(
        "tables",
        0,
        Some(show_tables),
        "?TABLE?",
        "List names of tables matching LIKE pattern TABLE",
        2,
        Some(""),
    ),
    command(
        "thousand_sep",
        0,
        Some(set_thousand_sep),
        "SEP",
        "Sets the thousand separator used when rendering numbers. Only for duckbox mode.",
        4,
        Some(""),
    ),
    command(
        "timer",
        2,
        Some(toggle_timer),
        "on|off",
        "Turn SQL timer on or off",
        0,
        Some(""),
    ),
    command(
        "ui_command",
        0,
        Some(set_ui_command),
        "[command]",
        "Set the UI command",
        0,
        Some(""),
    ),
    command(
        "version",
        1,
        Some(show_version),
        "",
        "Show the version",
        0,
        Some(""),
    ),
    command(
        "width",
        0,
        Some(set_widths),
        "NUM1 NUM2 ...",
        "Set minimum column widths for columnar output",
        0,
        Some("Negative values right-justify"),
    ),
];

/// `ShouldPrintCommand`: commands with help text that are not deprecated, matching the
/// glob pattern if there is one.
fn should_print_command(command: &MetadataCommand, glob_pattern: &str) -> bool {
    if command.extra_description.is_none() || command.description.contains("DEPRECATED") {
        return false;
    }
    glob_pattern.is_empty() || crate::helpers::string_glob(glob_pattern, command.command)
}

/// A line of `.help` (`PrintCommandInfo`).
#[derive(Debug, Default)]
struct PrintCommandInfo {
    command_name: String,
    first_part: String,
    second_part: String,
    first_part_highlight: Option<HighlightElementType>,
}

/// `ShellState::PrintHelp`: the commands matching `pattern` (with their extended
/// help), or all of them; returns the number of lines printed.
pub fn print_help(state: &ShellState, pattern: Option<&str>) -> usize {
    const MIN_SPACING: usize = 4;
    const SPACING_PER_LAYER: usize = 2;
    let print_extended = pattern.is_some();
    let glob_pattern = match pattern {
        // `--all` matches every command.
        Some("-a" | "-all" | "--all") | None => String::new(),
        Some(pattern) => format!("{pattern}*"),
    };
    let mut print_info_list = Vec::new();
    for command in &METADATA_COMMANDS {
        if !should_print_command(command, &glob_pattern) {
            continue;
        }
        print_info_list.push(PrintCommandInfo {
            command_name: format!(".{}", command.command),
            first_part: format!(" {}", command.usage),
            second_part: command.description.to_owned(),
            first_part_highlight: Some(HighlightElementType::StringConstant),
        });
        if !print_extended {
            continue;
        }
        // The extended help: a line per newline, a tab separating its two parts and tabs
        // at the start indenting it.
        let mut current = PrintCommandInfo::default();
        let mut first_part = true;
        let mut after_newline = true;
        for c in command.extra_description.unwrap_or_default().chars() {
            match c {
                '\n' => {
                    print_info_list.push(std::mem::take(&mut current));
                    first_part = true;
                    after_newline = true;
                }
                '\t' if after_newline => current.first_part += &" ".repeat(SPACING_PER_LAYER),
                '\t' => first_part = false,
                _ => {
                    if after_newline {
                        current.first_part += &" ".repeat(SPACING_PER_LAYER);
                        if c == '-' {
                            current.first_part_highlight = Some(HighlightElementType::StringConstant);
                        }
                    }
                    after_newline = false;
                    if first_part {
                        current.first_part.push(c);
                    } else {
                        current.second_part.push(c);
                    }
                }
            }
        }
        if !current.first_part.is_empty() {
            print_info_list.push(current);
        }
    }
    // Align the second parts after the longest first part.
    let max_lhs_size = print_info_list
        .iter()
        .filter(|info| !info.second_part.is_empty())
        .map(|info| info.command_name.len() + info.first_part.len() + MIN_SPACING)
        .max()
        .unwrap_or(0);
    let highlighter = ShellHighlight::new(&state.out);
    for info in &print_info_list {
        let lhs_size = info.command_name.len() + info.first_part.len();
        let spaces = if info.second_part.is_empty() {
            String::new()
        } else {
            " ".repeat(max_lhs_size - lhs_size)
        };
        if !info.command_name.is_empty() {
            highlighter.print_text(
                &info.command_name,
                PrintOutput::Stdout,
                HighlightElementType::Keyword,
            );
        }
        highlighter.print_text(
            &info.first_part,
            PrintOutput::Stdout,
            info.first_part_highlight.unwrap_or(HighlightElementType::None),
        );
        state.print(format!("{spaces}{}\n", info.second_part));
    }
    if !print_extended {
        state.print("\nRun .help --all for extended information\n");
        state.print("Run .help shortcuts for keyboard shortcuts\n");
    }
    print_info_list.len()
}

/// `ShellState::GetMetadataCompletions`: the dot commands `line` is a prefix of.
pub fn metadata_completions(line: &str) -> Vec<String> {
    let line = line.as_bytes();
    METADATA_COMMANDS
        .iter()
        .filter(|command| {
            let name = command.command.as_bytes();
            name.len() + 1 >= line.len() && line[1..].iter().zip(name).all(|(a, b)| a == b)
        })
        .map(|command| format!(".{}", command.command))
        .collect()
}

/// `ShellState::FindMetadataCommand`: the first command `option` is a long enough
/// prefix of, or the error with the closest names.
pub fn find_metadata_command(option: &str) -> std::result::Result<&'static MetadataCommand, String> {
    let n = option.len();
    if let Some(command) = METADATA_COMMANDS.iter().find(|command| {
        let match_size = if command.match_size == 0 {
            n
        } else {
            command.match_size
        };
        n >= match_size && command.command.as_bytes().starts_with(option.as_bytes())
    }) {
        return Ok(command);
    }
    let command_names: Vec<String> = METADATA_COMMANDS
        .iter()
        .map(|command| format!(".{}", command.command))
        .collect();
    let candidates =
        rustdb_common::string_util::candidates_error_message(&command_names, option, "Did you mean");
    Err(format!(
        "Unknown Command Error: Unrecognized command '{option}'\n{candidates}\nRun '.help' for more information."
    ))
}
