//! `rustdb`: the command-line shell.
//!
//! Usage: `rustdb [DATABASE] [-csv|-list] [-c SQL]`. Without `-c`, statements are read from
//! standard input, one statement per line. The full port of DuckDB's shell (dot commands,
//! box rendering, line editing) lives in this crate as it grows.

use std::io::{self, BufRead, Write};
use std::process::ExitCode;

use rustdb::{Config, Connection, Database, MaterializedResult};

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

/// Quotes a CSV field when needed (RFC 4180 style, like DuckDB's `-csv` mode).
fn csv_field(text: &str) -> String {
    if text.contains([',', '"', '\n', '\r']) {
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
                    String::new()
                } else {
                    format(&value.to_varchar().unwrap_or_else(|error| error.to_string()))
                }
            })
            .collect();
        writeln!(out, "{}", fields.join(separator))?;
    }
    Ok(())
}

fn run(connection: &mut Connection, sql: &str, mode: Mode) -> bool {
    let stdout = io::stdout();
    match connection.query(sql) {
        Ok(result) => render(&result, mode, &mut stdout.lock()).is_ok(),
        Err(error) => {
            eprintln!("{error}");
            false
        }
    }
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
    let mut ok = true;
    for line in io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        if !line.trim().is_empty() {
            ok &= run(&mut connection, &line, options.mode);
        }
    }
    if ok { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}
