//! `rustdb`: the command-line shell, a port of DuckDB's (`tools/shell`).

mod box_renderer;
mod command_line_option;
mod extension;
mod helpers;
mod highlight;
mod interrupt;
mod linenoise;
mod metadata_command;
mod output;
mod printer;
mod progress;
mod progress_bar;
mod prompt;
mod render_context;
mod render_table_metadata;
mod renderer;
mod shell;
mod state;
mod utf8;

use rustdb_common::ByteString;
use std::io::IsTerminal;
use std::process::ExitCode;

use highlight::HighlightElementType;
use output::PrintOutput;
use shell::{Shell, ShellOpenFlags};
use state::{BailOnError, Input, InputMode, MetadataResult, ShellState, StartupText, SuccessState};

/// An option of the command line with its arguments (`CommandLineCall`).
struct CommandLineCall {
    option: &'static command_line_option::CommandLineOption,
    arguments: Vec<ByteString>,
}

/// `RunShell`: reads the command line, opens the database, runs the init file and the
/// options, then the SQL of the command line or standard input; returns the exit code.
fn run_shell(shell: &mut Shell, args: &[ByteString]) -> i32 {
    let state = &mut shell.state;
    state.stdin_is_interactive = std::io::stdin().is_terminal();
    state.stdout_is_console = std::io::stdout().is_terminal();
    state.stderr_is_console = std::io::stderr().is_terminal();
    state.initialize();
    if let Some(program_name) = args.first() {
        program_name.clone_into(&mut state.program_name);
    }
    interrupt::install(libc::SIGINT);

    // A first pass over the command line: the database file, the SQL and the options,
    // whose pre-init callbacks run now.
    let mut extra_commands = Vec::new();
    let mut command_line_calls = Vec::new();
    let mut i = 1;
    while i < args.len() {
        let arg = &args[i];
        let Some(option_name) = arg.strip_prefix(b"-") else {
            if shell.state.db_filename.is_empty() {
                arg.clone_into(&mut shell.state.db_filename);
            } else {
                // More arguments are SQL or dot commands, and stdin is not read.
                shell.state.read_stdin = false;
                shell.state.stdin_is_interactive = false;
                extra_commands.push(arg.clone());
            }
            i += 1;
            continue;
        };
        // `--init` and `-init` are both accepted.
        let option_name = option_name.strip_prefix(b"-").unwrap_or(option_name);
        let option = match command_line_option::find_command_line_option(&shell.state, option_name) {
            Ok(option) => option,
            Err(error) => {
                shell.state.print_database_error(&error);
                return 1;
            }
        };
        let mut arguments = vec![ByteString::from(option.option)];
        for arg_idx in 0..option.argument_count {
            if i + 1 >= args.len() {
                let error = rustdb_common::byte_format!(
                    "Missing Argument Error: Argument '-{}' needs {} arguments, but got {arg_idx}\nOPTION:\n  -{} {}    \
                     {}\n\nRun '{} -help' for a list of options.\n",
                    option.option,
                    option.argument_count,
                    option.option,
                    option.arguments,
                    option.description,
                    shell.state.program_name
                );
                shell.state.print_database_error(&error);
                return 1;
            }
            i += 1;
            arguments.push(args[i].clone());
        }
        if let Some(callback) = option.pre_init_callback
            && callback(shell, &arguments) == MetadataResult::Exit
        {
            return 0;
        }
        command_line_calls.push(CommandLineCall { option, arguments });
        i += 1;
    }
    if shell.state.db_filename.is_empty() {
        shell.state.db_filename = ":memory:".into();
    }
    if let Err(error) = shell.open_db(ShellOpenFlags::ExitOnFailure) {
        shell::fatal(&error);
    }

    // The init file (`~/.duckdbrc` unless `-init` names one).
    if shell.state.run_init {
        let init_file = (!shell.state.init_file.is_empty()).then(|| shell.state.init_file.clone());
        if !shell.process_duckdbrc(init_file.as_deref()) && shell.state.bail != BailOnError::DontBailOnError {
            if !shell.state.duckdb_rc_path.is_empty() {
                let error = rustdb_common::byte_format!(
                    "Encountered errors while executing init file \"{}\". Exiting.",
                    shell.state.duckdb_rc_path
                );
                shell.state.print_database_error(&error);
            }
            return 1;
        }
    }
    shell.state.detect_dark_light_mode();

    // A second pass: the options apply after the init file, overriding it.
    for call in &command_line_calls {
        if let Some(callback) = call.option.post_init_callback
            && callback(shell, &call.arguments) == MetadataResult::Exit
        {
            return 0;
        }
    }

    let mut rc = 0;
    if !shell.state.read_stdin {
        // Every argument after the database file is a separate input.
        for command in &extra_commands {
            if command.starts_with(b".") {
                rc = shell.do_meta_command_bytes(command);
                if rc != 0 {
                    return if rc == 2 { 0 } else { rc };
                }
            } else if shell.execute_sql(command) != SuccessState::Success {
                return if rc != 0 { rc } else { 1 };
            }
        }
    } else if shell.state.stdin_is_interactive {
        print_startup_text(&shell.state);
        let history_file = std::env::var("DUCKDB_HISTORY")
            .unwrap_or_else(|_| format!("{}/.duckdb_history", state::home_directory()));
        shell.load_history(&history_file);
        shell.state.input = Input::Interactive;
        rc = shell.process_input(InputMode::Standard);
        shell.save_history(&history_file);
    } else {
        shell.state.input = Input::Stdin;
        rc = shell.process_input(InputMode::Standard);
    }
    rc
}

/// The version and hint shown when the shell starts on a terminal.
fn print_startup_text(state: &ShellState) {
    let mut startup_version = format!(
        "DuckDB {} ({}",
        rustdb::Database::library_version(),
        rustdb::Database::release_codename()
    );
    if rustdb::Database::release_codename().contains("Development") {
        startup_version += ", ";
        startup_version += rustdb::Database::source_id();
    }
    startup_version += ")\n";
    if state.startup_text != StartupText::None {
        state.print_highlighted(
            &startup_version,
            PrintOutput::Stdout,
            HighlightElementType::StartupVersion,
        );
    }
    if state.startup_text == StartupText::All {
        state.print_highlighted(
            "Enter \".help\" for usage hints.\n",
            PrintOutput::Stdout,
            HighlightElementType::StartupText,
        );
    }
}

fn main() -> ExitCode {
    rustdb_capi::register_extension_loader();
    // Preserve the OS argument bytes until the consuming operation validates them.
    let args: Vec<ByteString> = std::env::args_os()
        .map(|arg| arg.as_encoded_bytes().into())
        .collect();
    let mut shell = Shell::new(ShellState::new());
    let rc = run_shell(&mut shell, &args);
    // `RunShell`'s clean-up: SIGINT is ignored while the database closes.
    interrupt::ignore(libc::SIGINT);
    shell.state.set_table_name(None);
    shell.state.set_last_result(None);
    shell.close_db();
    shell.state.reset_output();
    shell.state.do_xdg_open = false;
    shell.state.clear_temp_file();
    printer::flush_stdout();
    ExitCode::from(rc as u8)
}
