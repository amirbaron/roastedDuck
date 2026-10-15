//! `rustdb`: the command-line shell.
//!
//! Usage: `rustdb [DATABASE] [-csv|-list] [-c SQL]`. Without `-c`, statements are read from
//! standard input, one statement per line. The full port of DuckDB's shell (dot commands,
//! box rendering, line editing) lives in this crate as it grows.

use std::io::{self, BufRead, Write};
use std::process::ExitCode;

use rustdb::{Config, Connection, Database, MaterializedResult, StatementReturnType};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Csv,
    List,
}

struct Options {
    database: String,
    mode: Mode,
    command: Option<String>,
}

fn parse_args(args: impl Iterator<Item = String>) -> Result<Options, String> {
    let mut options = Options {
        database: String::new(),
        mode: Mode::List,
        command: None,
    };
    let mut args = args.peekable();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-csv" => options.mode = Mode::Csv,
            "-list" => options.mode = Mode::List,
            "-c" => options.command = Some(args.next().ok_or("-c requires an argument")?),
            flag if flag.starts_with('-') => return Err(format!("unknown option: {flag}")),
            path => path.clone_into(&mut options.database),
        }
    }
    Ok(options)
}

/// Quotes a CSV field like DuckDB's `-csv` mode (`PrintEscapedCSV`): fields holding a
/// control character, a quote, a non-ASCII byte or the separator become a quoted
/// identifier.
fn csv_field(text: &str) -> String {
    let needs_quote = text.contains(',')
        || text
            .bytes()
            .any(|byte| byte <= 31 || byte == b'"' || byte == b'\'' || byte >= 127);
    if needs_quote {
        format!("\"{}\"", text.replace('"', "\"\""))
    } else {
        text.to_owned()
    }
}

fn render(result: &MaterializedResult, mode: Mode, out: &mut impl Write) -> io::Result<()> {
    let separator = if mode == Mode::Csv { "," } else { "|" };
    let format = |text: &str| {
        if mode == Mode::Csv {
            csv_field(text)
        } else {
            text.to_owned()
        }
    };
    let header: Vec<String> = result.names().iter().map(|name| format(name)).collect();
    writeln!(out, "{}", header.join(separator))?;
    for row in 0..result.row_count() {
        let fields: Vec<String> = (0..result.column_count())
            .map(|column| {
                let value = result.value(column, row);
                if value.is_null() {
                    // The shell's default `.nullvalue`.
                    "NULL".to_owned()
                } else {
                    format(&value.to_varchar().unwrap_or_else(|error| error.to_string()))
                }
            })
            .collect();
        writeln!(out, "{}", fields.join(separator))?;
    }
    Ok(())
}

/// Runs each statement of `sql`, printing the results of queries (statements that
/// change rows or return nothing print nothing), and stops at the first error.
fn run(connection: &mut Connection, sql: &str, mode: Mode) -> bool {
    let stdout = io::stdout();
    let outcome = connection.extract_statements(sql).and_then(|statements| {
        for statement in statements {
            let query = statement.query.clone();
            let result = connection.execute_statement(&query, statement)?;
            if result.return_type() == StatementReturnType::QueryResult
                && render(&result, mode, &mut stdout.lock()).is_err()
            {
                return Ok(false);
            }
        }
        Ok(true)
    });
    outcome.unwrap_or_else(|error| {
        eprintln!("{error}");
        false
    })
}

fn main() -> ExitCode {
    let options = match parse_args(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("Error: {message}");
            return ExitCode::FAILURE;
        }
    };
    let connection = Database::open(&options.database, &Config::default()).and_then(|db| db.connect());
    let mut connection = match connection {
        Ok(connection) => connection,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };
    if let Some(sql) = options.command {
        return if run(&mut connection, &sql, options.mode) {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        };
    }
    // Statements run once a line completes them with `;`.
    let mut ok = true;
    let mut pending = String::new();
    for line in io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        pending.push_str(&line);
        pending.push('\n');
        if pending.trim_end().ends_with(';') {
            ok &= run(&mut connection, &pending, options.mode);
            pending.clear();
        }
    }
    if !pending.trim().is_empty() {
        ok &= run(&mut connection, &pending, options.mode);
    }
    if ok { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}
