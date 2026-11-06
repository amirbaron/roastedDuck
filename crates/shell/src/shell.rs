//! Running SQL and dot commands (DuckDB's `tools/shell/shell.cpp`): reading input,
//! executing statements, rendering their results, and the dot commands that query the
//! database.

use std::fs::File;
use std::io::BufReader;
use std::time::Instant;

use rustdb::{
    Connection, Database, PendingExecutionResult, QueryResult, QueryResultMemoryType, Result, SqlStatement,
    StatementReturnType, Value,
};
use rustdb_ast::{ExpressionType, QueryNodeKind, ShowType, SqlStatementKind, TableRefKind};
use rustdb_common::ByteString;

use crate::highlight::HighlightElementType;
use crate::interrupt;
use crate::metadata_command;
use crate::output::{OutputTarget, PrintOutput};
use crate::printer;
use crate::progress::ProgressDisplay;
use crate::prompt::PromptContext;
use crate::renderer::{
    RenderingQueryResult, ResultSource, ScannedResult, ShellRenderer, StatePrintStream, renderer_for,
};
use crate::state::{
    Input, InputMode, MetadataResult, PagerMode, ReadLineVersion, RenderMode, ShellColumnInfo, ShellFlag,
    ShellState, ShellTableInfo, StartupText, SuccessState, exit, home_directory, sql_identifier,
    sql_identifier_bytes, sql_string, sql_string_bytes,
};

/// What `OpenDB` does when the database cannot be opened (`ShellOpenFlags`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellOpenFlags {
    ExitOnFailure,
    KeepAliveOnFailure,
}

/// The outcome of [`execute_sql_single_value`] (`ExecuteSQLSingleValueResult`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecuteSqlSingleValueResult {
    Success,
    ExecutionError,
    EmptyResult,
    MultipleRows,
    MultipleColumns,
    NullResult,
}

/// `Value::ToString`.
pub fn value_to_string(value: &Value) -> String {
    if value.is_null() {
        return "NULL".to_owned();
    }
    value.to_varchar().unwrap_or_default()
}

/// `ExecuteSQLSingleValue`: the single value `sql` returns, with the error message for
/// an execution error.
pub fn execute_sql_single_value_bytes(
    conn: &mut Connection,
    sql: &[u8],
) -> (ExecuteSqlSingleValueResult, ByteString) {
    let result = match conn.query(sql) {
        Ok(result) => result,
        Err(error) => {
            return (
                ExecuteSqlSingleValueResult::ExecutionError,
                error.to_bytes().into(),
            );
        }
    };
    if result.return_type() != StatementReturnType::QueryResult || result.row_count() == 0 {
        return (ExecuteSqlSingleValueResult::EmptyResult, ByteString::new());
    }
    if result.row_count() > 1 {
        return (ExecuteSqlSingleValueResult::MultipleRows, ByteString::new());
    }
    if result.column_count() != 1 {
        return (ExecuteSqlSingleValueResult::MultipleColumns, ByteString::new());
    }
    let value = result.value(0, 0);
    if value.is_null() {
        return (ExecuteSqlSingleValueResult::NullResult, ByteString::new());
    }
    (
        ExecuteSqlSingleValueResult::Success,
        value.to_varchar_bytes().unwrap_or_default(),
    )
}

/// The settings and queries of a prompt, read through a connection
/// (`Prompt::HandleSetting` and `Prompt::ExecuteSQL`).
#[derive(Debug)]
pub struct ConnectionPromptContext<'a>(pub &'a mut Connection);

impl PromptContext for ConnectionPromptContext<'_> {
    fn setting(&mut self, name: &[u8]) -> Result<rustdb_common::ByteString> {
        let conn = &mut *self.0;
        let human = |bytes: u64| bytes_to_human_readable(bytes, 1000);
        let status = conn.database().memory_status();
        Ok(match name {
            b"memory_limit" => human(status.memory_limit).into(),
            b"memory_usage" => human(status.memory_usage).into(),
            b"swap_usage" => human(status.swap_usage).into(),
            b"swap_max" => status.swap_max.map_or_else(|| "INF".into(), human).into(),
            b"bytes_read" => human(conn.bytes_read()).into(),
            b"bytes_written" => human(conn.bytes_written()).into(),
            _ => {
                let (database, schema) = conn.current_database_and_schema();
                match name {
                    b"current_database" => database,
                    b"current_schema" => schema,
                    b"current_database_and_schema" if schema == "main" => database,
                    b"current_database_and_schema" => rustdb_common::byte_format!("{database}.{schema}"),
                    _ => {
                        return Err(rustdb::Error::internal(rustdb_common::byte_format!(
                            "Unsupported setting {name}"
                        )));
                    }
                }
            }
        })
    }

    fn execute_sql(&mut self, query: &[u8]) -> ByteString {
        match execute_sql_single_value_bytes(self.0, query) {
            (ExecuteSqlSingleValueResult::Success, value) => value,
            (ExecuteSqlSingleValueResult::EmptyResult, _) => "#EMPTY#".into(),
            (ExecuteSqlSingleValueResult::MultipleRows, _) => "#MULTIPLE_ROWS#".into(),
            (ExecuteSqlSingleValueResult::MultipleColumns, _) => "#MULTIPLE_COLUMNS#".into(),
            (ExecuteSqlSingleValueResult::NullResult, _) => "#NULL#".into(),
            (ExecuteSqlSingleValueResult::ExecutionError, _) => "#ERROR".into(),
        }
    }
}

/// `StringUtil::BytesToHumanReadableString`.
pub fn bytes_to_human_readable(bytes: u64, multiplier: u64) -> String {
    const UNITS: [[&str; 6]; 2] = [
        ["bytes", "KiB", "MiB", "GiB", "TiB", "PiB"],
        ["bytes", "kB", "MB", "GB", "TB", "PB"],
    ];
    let units = &UNITS[usize::from(multiplier == 1000)];
    let mut array = [0u64; 6];
    array[0] = bytes;
    for i in 1..6 {
        array[i] = array[i - 1] / multiplier;
        array[i - 1] %= multiplier;
    }
    for i in (1..6).rev() {
        if array[i] != 0 {
            let fractional = (array[i - 1] * 10) / multiplier;
            return format!("{}.{} {}", array[i], fractional, units[i]);
        }
    }
    format!("{}{}", array[0], if bytes == 1 { " byte" } else { " bytes" })
}

/// `StringUtil::CharacterIsSpace` (the shell's `IsSpace`).
pub fn is_space(c: u8) -> bool {
    crate::renderer::is_space(c)
}

/// `_all_whitespace`: whether `z` holds only whitespace and comments.
pub fn all_whitespace(z: impl AsRef<[u8]>) -> bool {
    let z = z.as_ref();
    let mut i = 0;
    while i < z.len() {
        if is_space(z[i]) {
            i += 1;
            continue;
        }
        if z[i] == b'/' && z.get(i + 1) == Some(&b'*') {
            i += 2;
            while i < z.len() && (z[i] != b'*' || z.get(i + 1) != Some(&b'/')) {
                i += 1;
            }
            if i >= z.len() {
                return false;
            }
            i += 2;
            continue;
        }
        if z[i] == b'-' && z.get(i + 1) == Some(&b'-') {
            i += 2;
            while i < z.len() && z[i] != b'\n' {
                i += 1;
            }
            if i >= z.len() {
                return true;
            }
            i += 1;
            continue;
        }
        return false;
    }
    true
}

/// `skipDollarQuotedString`: the position of the `$` closing a `$delimiter$` string
/// started before `start`, if it is closed.
fn skip_dollar_quoted_string(sql: &[u8], mut pos: usize, delimiter: &[u8]) -> Option<usize> {
    while pos < sql.len() {
        if sql[pos] == b'$' {
            pos += 1;
            let start = pos;
            while pos < sql.len() && sql[pos] != b'$' {
                pos += 1;
            }
            if pos >= sql.len() {
                return None;
            }
            if &sql[start..pos] == delimiter {
                return Some(pos);
            }
            // Not the delimiter: continue at this dollar.
            pos = start - 1;
        }
        pos += 1;
    }
    None
}

/// The state of [`sql_is_complete`] (`SQLParseState`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SqlParseState {
    Semicolon,
    Whitespace,
    Normal,
}

/// `ShellState::SQLIsComplete`: whether `sql` ends in a semicolon outside strings and
/// comments.
pub fn sql_is_complete(sql: impl AsRef<[u8]>) -> bool {
    let z = sql.as_ref();
    let mut state = SqlParseState::Normal;
    let mut i = 0;
    while i < z.len() {
        let next_state = match z[i] {
            b';' => SqlParseState::Semicolon,
            b' ' | b'\r' | b'\t' | b'\n' | b'\x0c' => SqlParseState::Whitespace,
            b'/' if z.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i < z.len() && (z[i] != b'*' || z.get(i + 1) != Some(&b'/')) {
                    i += 1;
                }
                if i >= z.len() {
                    // An unterminated comment.
                    return false;
                }
                i += 1;
                SqlParseState::Whitespace
            }
            b'-' if z.get(i + 1) == Some(&b'-') => {
                while i < z.len() && z[i] != b'\n' {
                    i += 1;
                }
                if i >= z.len() {
                    // A comment to the end: complete if a semicolon preceded it.
                    return state == SqlParseState::Semicolon;
                }
                SqlParseState::Whitespace
            }
            b'$' => {
                // Is this a `$delimiter$` string?
                let mut next_dollar = 0;
                let mut idx = 1;
                while i + idx < z.len() {
                    let c = z[i + idx];
                    if c == b'$' {
                        next_dollar = idx;
                        break;
                    }
                    let identifier =
                        c.is_ascii_alphabetic() || c == b'_' || c >= 0x80 || (idx > 1 && c.is_ascii_digit());
                    if !identifier {
                        break;
                    }
                    idx += 1;
                }
                if next_dollar == 0 {
                    SqlParseState::Normal
                } else {
                    let delimiter = &z[i + 1..i + next_dollar];
                    match skip_dollar_quoted_string(z, i + next_dollar + 1, delimiter) {
                        Some(end) => {
                            i = end;
                            SqlParseState::Whitespace
                        }
                        // An unterminated dollar-quoted string.
                        None => return false,
                    }
                }
            }
            quote @ (b'"' | b'\'') => {
                i += 1;
                while i < z.len() && z[i] != quote {
                    i += 1;
                }
                if i >= z.len() {
                    // An unterminated string.
                    return false;
                }
                SqlParseState::Whitespace
            }
            _ => SqlParseState::Normal,
        };
        if next_state != SqlParseState::Whitespace {
            state = next_state;
        }
        i += 1;
    }
    state == SqlParseState::Semicolon
}

/// `resolve_backslashes`: C escapes (`\n`, `\t`, `\NNN` octal, ...) in a dot command
/// argument.
/// Resolves shell escapes without imposing UTF-8 on the decoded bytes.
pub fn resolve_backslashes_bytes(bytes: &[u8]) -> Vec<u8> {
    let mut result = Vec::with_capacity(bytes.len());
    let mut pos = 0;
    while pos < bytes.len() {
        let mut c = bytes[pos];
        if c == b'\\' && pos + 1 < bytes.len() {
            pos += 1;
            c = bytes[pos];
            c = match c {
                b'a' => 7,
                b'b' => 8,
                b't' => b'\t',
                b'n' => b'\n',
                b'v' => 0x0b,
                b'f' => 0x0c,
                b'r' => b'\r',
                b'0'..=b'7' => {
                    let mut value = c - b'0';
                    for _ in 0..2 {
                        match bytes.get(pos + 1) {
                            Some(&digit @ b'0'..=b'7') => {
                                pos += 1;
                                value = (value << 3).wrapping_add(digit - b'0');
                            }
                            _ => break,
                        }
                    }
                    value
                }
                // `\"`, `\'` and `\\` are the character itself, as is any other.
                _ => c,
            };
        }
        result.push(c);
        pos += 1;
    }
    result
}

/// The time spent by the process in user and system mode, in seconds (`getrusage`).
fn resource_usage() -> (f64, f64) {
    // SAFETY: `getrusage` writes the struct passed.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: as above.
    unsafe {
        libc::getrusage(libc::RUSAGE_SELF, &raw mut usage);
    }
    let seconds = |time: libc::timeval| time.tv_sec as f64 + time.tv_usec as f64 * 0.000_001;
    (seconds(usage.ru_utime), seconds(usage.ru_stime))
}

/// `beginTimer` / `endTimer` of `.timer on`.
#[derive(Debug)]
struct Timer {
    start: Instant,
    user: f64,
    system: f64,
}

impl Timer {
    fn begin(enabled: bool) -> Option<Self> {
        enabled.then(|| {
            let (user, system) = resource_usage();
            Self {
                start: Instant::now(),
                user,
                system,
            }
        })
    }

    fn end(timer: Option<Self>) {
        let Some(timer) = timer else {
            return;
        };
        let real = timer.start.elapsed().as_millis() as f64 * 0.001;
        let (user, system) = resource_usage();
        ShellState::printf_stdout(format!(
            "Run Time (s): real {real:.3} user {:.6} sys {:.6}\n",
            user - timer.user,
            system - timer.system
        ));
    }
}

/// The shell: its state, and the database and connection it runs statements on
/// (`ShellState::db` and `ShellState::conn`).
#[derive(Debug)]
pub struct Shell {
    pub db: Option<Database>,
    pub conn: Option<Connection>,
    pub state: ShellState,
}

impl Shell {
    pub fn new(state: ShellState) -> Self {
        Self {
            db: None,
            conn: None,
            state,
        }
    }

    /// The connection, opening the database first if needed.
    /// The connection; DuckDB's shell dereferences its null `conn` after an `.open`
    /// whose in-memory fallback failed too, which is an internal error.
    pub fn conn(&mut self) -> Result<&mut Connection> {
        self.conn.as_mut().ok_or_else(|| {
            rustdb::Error::internal_with_stack_trace("Attempted to dereference unique_ptr that is NULL!")
        })
    }

    /// `Connection::Query` on the connection.
    pub fn query(&mut self, sql: impl AsRef<[u8]>) -> Result<rustdb::MaterializedResult> {
        self.conn()?.query(sql)
    }

    /// `ShellState::AddError`.
    pub fn add_error(&mut self) {
        self.state.n_err += 1;
    }

    /// `PrintDatabaseError`.
    pub fn print_database_error(&mut self, error: impl AsRef<[u8]>) {
        self.state.print_database_error(error);
    }

    /// Opens the database at `filename` and connects to it, logging warnings to the
    /// shell (`RegisterShellLogger`).
    fn connect(&mut self, filename: impl AsRef<[u8]>) -> Result<()> {
        let mut config = self.state.config.clone();
        // `ShellExtension::Load` sets the API of the database.
        config.set_option_by_name("duckdb_api", Value::varchar(crate::extension::DUCKDB_API_CLI))?;
        let db = Database::open(filename, &config)?;
        crate::extension::register_shell_logger(&db, &self.state.out)?;
        let conn = db.connect()?;
        interrupt::set_connection(Some(&conn.client_handle()));
        self.conn = Some(conn);
        self.db = Some(db);
        Ok(())
    }

    /// Closes the connection and the database.
    pub fn close_db(&mut self) {
        interrupt::set_connection(None);
        self.conn = None;
        self.db = None;
    }

    /// `OpenDB`: opens the database if it is not open, loads the shell extension and
    /// applies safe mode and the progress bar.
    /// The in-memory fallback of `KeepAliveOnFailure` failing too is an error, and leaves
    /// no database open.
    pub fn open_db(&mut self, flags: ShellOpenFlags) -> Result<()> {
        if self.db.is_some() {
            return Ok(());
        }
        let filename = self.state.db_filename.clone();
        if let Err(error) = self.connect(&filename) {
            self.print_database_error(error.to_bytes());
            if flags == ShellOpenFlags::KeepAliveOnFailure {
                self.connect(":memory:")?;
            } else {
                exit(1);
            }
        }
        if let (Some(db), Some(conn)) = (&self.db, &mut self.conn)
            && let Err(error) = crate::extension::load_shell_extension(db, conn, &self.state.last_result)
        {
            self.state.print_database_error(error.to_bytes());
        }
        if self.state.safe_mode {
            self.execute_query("SET enable_external_access=false");
            self.execute_query("SET lock_configuration=true");
        }
        if self.state.stdout_is_console {
            self.execute_query("PRAGMA enable_progress_bar");
            self.execute_query("PRAGMA enable_print_progress_bar");
        }
        Ok(())
    }

    /// `EnableSafeMode` (`-safe` and `.safe_mode`): an open database loses external
    /// access at once.
    pub fn enable_safe_mode(&mut self) {
        self.state.safe_mode = true;
        if self.db.is_some() {
            self.execute_query("SET enable_external_access=false");
            self.execute_query("SET lock_configuration=true");
        }
    }

    /// `ShellLoadHistory`, with the line editor.
    pub fn load_history(&mut self, path: &str) {
        if self.state.rl_version == ReadLineVersion::Linenoise {
            crate::linenoise::history_load(path);
        }
    }

    /// `ShellSetHistoryMaxLength(2000)` and `ShellSaveHistory`, with the line editor.
    pub fn save_history(&mut self, path: &str) {
        if self.state.rl_version == ReadLineVersion::Linenoise {
            crate::linenoise::history_set_max_len(2000);
            crate::linenoise::history_save(path);
        }
    }

    /// `ShellAddHistory`, with the line editor.
    fn add_history(&self, line: impl AsRef<[u8]>) {
        if self.state.rl_version == ReadLineVersion::Linenoise {
            crate::linenoise::history_add(line);
        }
    }

    /// `ExecuteQuery`: runs `query`, reporting a failure to stderr.
    pub fn execute_query(&mut self, query: &str) -> SuccessState {
        match self.query(query) {
            Ok(_) => SuccessState::Success,
            Err(error) => {
                self.state
                    .print_err(format!("Failed to execute query \"{query}\": {error}\n"));
                SuccessState::Failure
            }
        }
    }

    /// `RunTableDumpQuery`: prints the SQL `select` returns, a statement per row.
    pub fn run_table_dump_query(&mut self, select: impl AsRef<[u8]>) {
        let result = match self.query(select) {
            Ok(result) => result,
            Err(error) => {
                self.state.print(format!("/**** ERROR: {error} *****/\n"));
                self.add_error();
                return;
            }
        };
        for row in 0..result.row_count() {
            let text = value_to_string(&result.value(0, row));
            self.state.print(&text);
            // A trailing `--` comment would swallow the semicolon.
            if text.contains("--") {
                self.state.print("\n;\n");
            } else {
                self.state.print(";\n");
            }
        }
    }

    /// `TableColumnList`: the column names of `table`.
    fn table_column_list(&mut self, table: &str) -> Vec<String> {
        let query = format!("PRAGMA table_info={}", sql_string(table));
        match self.query(&query) {
            Ok(result) => (0..result.row_count())
                .map(|row| value_to_string(&result.value(1, row)))
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    /// `getTableSchema`: the schema of base table `table`, `main` first.
    fn table_schema(&mut self, table: &str) -> String {
        let query = format!(
            "SELECT table_schema FROM information_schema.tables WHERE table_name = {} AND table_type='BASE TABLE' \
             ORDER BY (table_schema='main') DESC LIMIT 1",
            sql_string(table)
        );
        match self.query(&query) {
            Ok(result) => (0..result.row_count())
                .map(|row| value_to_string(&result.value(0, row)))
                .next_back()
                .unwrap_or_default(),
            Err(_) => String::new(),
        }
    }

    /// `RunSchemaDumpQuery`: prints the schema of the tables `query` lists and their
    /// rows as INSERT statements.
    pub fn run_schema_dump_query(&mut self, query: impl AsRef<[u8]>) -> Result<()> {
        let result = self.query(query).map_err(|error| {
            let message = error.to_bytes();
            rustdb::Error::invalid_input(rustdb_common::byte_format!(
                "Attempting to fetch from an unsuccessful query result\nError: {message}"
            ))
        })?;
        for row in 0..result.row_count() {
            let table = value_to_string(&result.value(0, row));
            let table_type = value_to_string(&result.value(1, row));
            let sql = value_to_string(&result.value(2, row));
            self.state.print(ShellState::schema_line(&sql, ";\n"));
            if table_type != "table" {
                continue;
            }
            // Dump the contents of the table.
            let schema = self.table_schema(&table);
            let qualified_name = format!("{}.{}", sql_identifier(&schema), sql_identifier(&table));
            let table_columns = self.table_column_list(&qualified_name);
            if table_columns.is_empty() {
                self.add_error();
                break;
            }
            let destination = if schema.is_empty() {
                sql_identifier(&table)
            } else {
                qualified_name.clone()
            };
            let columns: Vec<String> = table_columns
                .iter()
                .map(|column| sql_identifier(column))
                .collect();
            let select = format!("SELECT {} FROM {qualified_name}", columns.join(", "));
            let saved_dest_table = std::mem::replace(&mut self.state.dest_table, destination.into());
            let saved_mode = self.state.mode;
            self.state.mode = RenderMode::Insert;
            self.state.c_mode = RenderMode::Insert;
            let result = self.execute_sql(&select);
            self.state.dest_table = saved_dest_table;
            self.state.mode = saved_mode;
            if result != SuccessState::Success {
                self.add_error();
            }
        }
        Ok(())
    }

    /// Runs `statement` and returns its result: streaming unless `materialize`
    /// (`Connection::SendQuery`). On a terminal with the progress bar on, the progress of
    /// a long query is shown while it runs.
    fn send_statement(&mut self, statement: SqlStatement, materialize: bool) -> Result<QueryResult> {
        let Self { db, conn, state } = self;
        let Some(conn) = conn.as_mut() else {
            return Err(rustdb::Error::internal_with_stack_trace(
                "Attempted to dereference unique_ptr that is NULL!",
            ));
        };
        let query = statement.query.clone();
        // A materialized result is buffer-managed, a streaming one is not
        // (`ShellState::ExecuteStatement`'s `QueryParameters`).
        let memory_type = if materialize {
            QueryResultMemoryType::BufferManaged
        } else {
            QueryResultMemoryType::InMemory
        };
        let pending = conn.pending_query_with(&query, statement, !materialize, memory_type)?;
        if state.stdout_is_console
            && let Some(wait_time) = progress_bar_wait_time(conn)
            // Direct statements such as SET never start a backend progress bar.
            && conn.client_handle().query_progress().percentage > -1.0
        {
            let mut display = ProgressDisplay::new(wait_time);
            loop {
                match conn.execute_task(&pending)? {
                    PendingExecutionResult::ResultReady | PendingExecutionResult::ExecutionFinished => break,
                    PendingExecutionResult::ResultNotReady | PendingExecutionResult::NoTasksAvailable => {
                        let progress = conn.client_handle().query_progress();
                        display.update(state, db.as_ref(), progress.percentage);
                    }
                    PendingExecutionResult::Blocked => conn.wait_for_task(&pending)?,
                }
            }
            display.finish(state, db.as_ref());
        }
        conn.execute_pending(&pending)
    }

    /// `UseDescribeRenderMode`: whether `statement` is `DESCRIBE table` or
    /// `DESCRIBE query`, with the name to show.
    fn describe_table_name(statement: &SqlStatement) -> Option<rustdb_common::ByteString> {
        let SqlStatementKind::Select(select) = &statement.kind else {
            return None;
        };
        let QueryNodeKind::Select(select_node) = &select.node.kind else {
            return None;
        };
        if select_node.select_list.len() != 1
            || select_node.select_list[0].expression_type != ExpressionType::Star
        {
            return None;
        }
        let TableRefKind::Show(show) = &select_node.from_table.kind else {
            return None;
        };
        if show.show_type != ShowType::Describe {
            return None;
        }
        if !show.table_name.is_empty() {
            return Some(show.table_name.clone());
        }
        if let Some(query) = &show.query
            && let QueryNodeKind::Select(show_select) = &query.kind
            && let TableRefKind::BaseTable(base_table) = &show_select.from_table.kind
        {
            return Some(base_table.table_name.clone());
        }
        Some("Describe".into())
    }

    /// `ExecuteStatement`: runs a statement and renders its result.
    pub fn execute_statement(&mut self, statement: SqlStatement) -> Result<SuccessState> {
        if !statement.named_param_map.is_empty() {
            self.print_database_error(
                "Prepared statement parameters cannot be used directly\nTo use prepared statement parameters, use \
                 PREPARE to prepare a statement, followed by EXECUTE",
            );
            return Ok(SuccessState::Failure);
        }
        let mut renderer = renderer_for(&self.state, self.state.c_mode);
        let materialize = renderer.require_materialized_result();
        let result = match self.send_statement(statement, materialize) {
            Ok(result) => result,
            Err(error) => {
                self.print_database_error(error.to_bytes());
                return Ok(SuccessState::Failure);
            }
        };
        let (source, return_type) = match result {
            QueryResult::Materialized(result) => {
                let return_type = result.return_type();
                (
                    ResultSource::Materialized(ScannedResult::new(result)),
                    return_type,
                )
            }
            QueryResult::Stream(stream) => {
                let return_type = stream.return_type();
                (ResultSource::Stream(stream), return_type)
            }
        };
        if return_type == StatementReturnType::ChangedRows {
            let first_chunk = match &source {
                ResultSource::Materialized(scanned) => scanned.fetch()?,
                ResultSource::Stream(stream) => self.conn()?.fetch(stream)?,
            };
            if let Some(chunk) = first_chunk.filter(|chunk| chunk.len() == 1) {
                let row_changes = chunk.column(0).get_value(0);
                if let Some(changes) = changed_row_count(self.conn()?, &row_changes) {
                    self.state.last_changes = changes;
                    self.state.total_changes = self.state.total_changes.wrapping_add(changes);
                }
            }
        }
        if return_type != StatementReturnType::QueryResult {
            // Only queries render results.
            if let ResultSource::Stream(stream) = &source {
                self.conn()?.close_query(stream.id());
            }
            return Ok(SuccessState::Success);
        }
        if let ResultSource::Materialized(scanned) = &source {
            self.state.set_last_result(Some(scanned.clone()));
        }
        self.render_query_result(renderer.as_mut(), source, PagerMode::Automatic)
    }

    /// `ExecuteSQL`: runs every statement of `sql`, stopping at the first that fails.
    pub fn execute_sql(&mut self, sql: impl AsRef<[u8]>) -> SuccessState {
        let sql = sql.as_ref();
        // DuckDB dereferences the connection outside its error handling.
        if let Err(error) = self.conn() {
            fatal(&error);
        }
        match self.execute_sql_statements(sql) {
            Ok(state) => state,
            Err(error) => {
                self.print_database_error(error.to_bytes());
                SuccessState::Failure
            }
        }
    }

    fn execute_sql_statements(&mut self, sql: &[u8]) -> Result<SuccessState> {
        let statements = self.conn()?.parse_statements_bytes(sql)?;
        for statement in statements {
            // The statement's text, without leading whitespace.
            let start = usize::try_from(statement.location).unwrap_or(0);
            let length = usize::try_from(statement.length).unwrap_or(0);
            let text = sql
                .get(start..(start + length).min(sql.len()))
                .unwrap_or_default();
            let text = &text[text.iter().take_while(|byte| is_space(**byte)).count()..];
            if self.state.has_flag(ShellFlag::Echo) {
                self.state.print(rustdb_common::byte_format!(
                    "{}\n",
                    if text.is_empty() { sql } else { text }
                ));
            }
            self.state.c_mode = self.state.mode;
            if statement.statement_type() == rustdb::StatementType::ExplainStatement {
                self.state.c_mode = RenderMode::Explain;
            }
            if self.state.mode == RenderMode::Duckbox
                && let Some(name) = Self::describe_table_name(&statement)
            {
                self.state.describe_table_name = name;
                self.state.c_mode = RenderMode::Describe;
            }
            let rc = self.execute_statement(statement)?;
            if rc != SuccessState::Success {
                return Ok(rc);
            }
        }
        Ok(SuccessState::Success)
    }

    /// `RenderQuery`: runs `query` and renders it with `renderer`.
    pub fn render_query(
        &mut self,
        renderer: &mut dyn ShellRenderer,
        query: impl AsRef<[u8]>,
        pager: PagerMode,
    ) -> Result<SuccessState> {
        let mut statements = match self.conn()?.parse_statements_bytes(query.as_ref()) {
            Ok(statements) => statements,
            Err(error) => {
                self.print_database_error(error.to_bytes());
                return Ok(SuccessState::Failure);
            }
        };
        let Some(last) = statements.pop() else {
            return Ok(SuccessState::Success);
        };
        let result = self
            .conn()?
            .execute_statements(statements)
            .and_then(|_| self.send_statement(last, false));
        let source = match result {
            Ok(QueryResult::Materialized(result)) => ResultSource::Materialized(ScannedResult::new(result)),
            Ok(QueryResult::Stream(stream)) => ResultSource::Stream(stream),
            Err(error) => {
                self.print_database_error(error.to_bytes());
                return Ok(SuccessState::Failure);
            }
        };
        self.render_query_result(renderer, source, pager)
    }

    /// `RenderQueryResult`: analyzes the result, starts the pager if the result is long
    /// or wide, and renders it.
    pub fn render_query_result(
        &mut self,
        renderer: &mut dyn ShellRenderer,
        source: ResultSource,
        pager_overwrite: PagerMode,
    ) -> Result<SuccessState> {
        let mut result = RenderingQueryResult::new(source);
        self.conn()?;
        let (Some(conn), state) = (self.conn.as_mut(), &mut self.state) else {
            return Ok(SuccessState::Failure);
        };
        renderer.analyze(state, &mut result, conn)?;
        if interrupt::seen_interrupt() {
            return Ok(SuccessState::Failure);
        }
        let mut pager = pager_overwrite;
        if pager == PagerMode::Automatic
            && state.should_use_pager()
            && renderer.should_use_pager(state, &mut result, conn, state.pager_mode)?
        {
            pager = PagerMode::On;
        }
        let pager_started = pager == PagerMode::On && state.setup_pager();
        let mut out = StatePrintStream::new(state.out.clone());
        let rendered = renderer.render_query_result(&mut out, state, &mut result, conn);
        if pager_started {
            state.finish_pager();
        }
        rendered
    }

    /// `ImportData` (`.import FILE TABLE`): creates or appends to TABLE with a reader of
    /// FILE.
    pub fn import_data<S: AsRef<[u8]>>(&mut self, args: &[S]) -> Result<bool> {
        let args: Vec<ByteString> = args.iter().map(|arg| arg.as_ref().into()).collect();
        if self.state.safe_mode {
            self.state.print_err(".import cannot be used in -safe mode\n");
            return Ok(false);
        }
        let mut table_name = ByteString::new();
        let mut file_name = ByteString::new();
        let mut generic_parameters = GenericParameters::default();
        let mut function = String::new();
        let mut i = 1;
        while i < args.len() {
            let mut z = args[i].as_bytes();
            if z.starts_with(b"--") {
                z = &z[1..];
            }
            if !z.starts_with(b"-") {
                if file_name.is_empty() {
                    file_name = z.into();
                } else if table_name.is_empty() {
                    table_name = z.into();
                } else {
                    self.state.print(rustdb_common::byte_format!(
                        "ERROR: extra argument: \"{z}\".  Usage:\n"
                    ));
                    metadata_command::print_help(&self.state, Some(b"import"));
                    return Ok(false);
                }
            } else if z == b"-v" {
                // Verbose: ignored.
            } else if z == b"-ascii" {
                self.state
                    .print_err("-ascii mode is no longer supported for .import");
                exit(1);
            } else if z == b"-csv" {
                "read_csv".clone_into(&mut function);
            } else if z == b"-parquet" {
                "read_parquet".clone_into(&mut function);
            } else if z == b"-json" {
                "read_json".clone_into(&mut function);
            } else {
                let name = &z[1..];
                if i + 1 >= args.len() {
                    self.state.print(rustdb_common::byte_format!(
                        "ERROR: expected an argument for generic parameter: \"{name}\".  Usage:\n"
                    ));
                    metadata_command::print_help(&self.state, Some(b"import"));
                    return Ok(false);
                }
                i += 1;
                generic_parameters.set(name, &args[i]);
            }
            i += 1;
        }
        if table_name.is_empty() {
            let missing = if file_name.is_empty() { "FILE" } else { "TABLE" };
            self.state
                .print(format!("ERROR: missing {missing} argument. Usage:\n"));
            metadata_command::print_help(&self.state, Some(b"import"));
            return Ok(false);
        }
        if function.is_empty() {
            // The reader of the file's extension (`function_map`, in its iteration
            // order), else `read_csv`.
            const FUNCTIONS: [(&str, &str); 9] = [
                (".xlsx", "read_xlsx"),
                (".avro", "read_avro"),
                (".ndjson", "read_json"),
                (".jsonl", "read_json"),
                (".json", "read_json"),
                (".tbl", "read_csv"),
                (".tsv", "read_csv"),
                (".csv", "read_csv"),
                (".parquet", "read_parquet"),
            ];
            FUNCTIONS
                .iter()
                .find(|(extension, _)| {
                    ["", ".gz", ".zst"].iter().any(|compression| {
                        file_name.ends_with(format!("{extension}{compression}").as_bytes())
                    })
                })
                .map_or("read_csv", |(_, reader)| reader)
                .clone_into(&mut function);
        }
        if function == "read_csv" && !generic_parameters.contains("ignore_errors") {
            generic_parameters.set("ignore_errors", "true");
        }
        ShellState::clear_interrupt();
        let conn = self.conn()?;
        let need_commit = conn.is_auto_commit();
        if need_commit {
            conn.query("BEGIN TRANSACTION")?;
        }
        // `Connection::TableInfo(table_name)`: in the default schema.
        let table_exists = conn.table_info("", "main", &table_name)?.is_some();
        let mut import_query = if table_exists {
            rustdb_common::byte_format!("INSERT INTO {} ", sql_identifier_bytes(&table_name))
        } else {
            rustdb_common::byte_format!("CREATE TABLE {} AS ", sql_identifier_bytes(&table_name))
        };
        import_query +=
            rustdb_common::byte_format!("SELECT * FROM {function}({}", sql_string_bytes(&file_name));
        for (name, value) in generic_parameters.iter() {
            import_query +=
                rustdb_common::byte_format!(", {}={}", sql_identifier_bytes(name), sql_string_bytes(value));
        }
        import_query.push(b')');
        match self.query(&import_query) {
            Ok(_) => {
                if need_commit {
                    self.query("COMMIT")?;
                }
                Ok(true)
            }
            Err(error) => {
                if need_commit {
                    self.query("ROLLBACK")?;
                }
                self.print_database_error(rustdb_common::byte_format!(
                    "Failed To Import Error: Failed to import from file '{file_name}'\n"
                ));
                self.print_database_error(error.to_bytes());
                Ok(false)
            }
        }
    }

    /// `OpenDatabase` (`.open ?OPTIONS? ?FILE?`).
    pub fn open_database<S: AsRef<[u8]>>(&mut self, args: &[S]) -> Result<bool> {
        let args: Vec<ByteString> = args.iter().map(|arg| arg.as_ref().into()).collect();
        if self.state.safe_mode {
            self.state.print_err(".open cannot be used in -safe mode\n");
            return Ok(false);
        }
        let mut new_filename = ByteString::new();
        let mut new_flag = false;
        let mut has_sql = false;
        self.state.db_filename.clear();
        self.state.config.access_mode = rustdb::AccessMode::ReadWrite;
        let mut name_index = 1;
        while name_index < args.len() && args[name_index].starts_with(b"-") {
            let z = args[name_index].as_bytes();
            if option_match_bytes(z, b"new") {
                new_flag = true;
            } else if option_match_bytes(z, b"readonly") {
                self.state.config.access_mode = rustdb::AccessMode::ReadOnly;
            } else if option_match_bytes(z, b"nofollow") {
                // Accepted for compatibility.
            } else if option_match_bytes(z, b"sql") {
                if has_sql {
                    self.state.print_err("Error: --sql provided multiple times\n");
                    return Ok(false);
                }
                if name_index + 1 >= args.len() {
                    self.state.print_err("Error: missing SQL query after --sql\n");
                    return Ok(false);
                }
                name_index += 1;
                let query = args[name_index].clone();
                let (result, value) = execute_sql_single_value_bytes(self.conn()?, &query);
                let error = match result {
                    ExecuteSqlSingleValueResult::ExecutionError => Some(rustdb_common::byte_format!(
                        "Error: failed to evaluate --sql query '{query}': {value}\n"
                    )),
                    ExecuteSqlSingleValueResult::EmptyResult => {
                        Some("Error: --sql query returned no rows, expected single value\n".into())
                    }
                    ExecuteSqlSingleValueResult::MultipleRows => {
                        Some("Error: --sql query returned multiple rows, expected single value\n".into())
                    }
                    ExecuteSqlSingleValueResult::MultipleColumns => {
                        Some("Error: --sql query returned multiple columns, expected single value\n".into())
                    }
                    ExecuteSqlSingleValueResult::NullResult => {
                        Some("Error: --sql query returned a null value\n".into())
                    }
                    ExecuteSqlSingleValueResult::Success => None,
                };
                if let Some(error) = error {
                    self.state.print_err(error);
                    return Ok(false);
                }
                new_filename = value;
                has_sql = true;
            } else {
                self.state
                    .print_err(rustdb_common::byte_format!("unknown option: {z}\n"));
                return Ok(false);
            }
            name_index += 1;
        }
        if has_sql && args.len() > name_index {
            self.state
                .print_err("Error: cannot use both --sql and a FILE argument\n");
            return Ok(false);
        }
        if !has_sql && args.len() > name_index {
            new_filename.clone_from(&args[name_index]);
        }
        // Close the open database.
        self.close_db();
        if !new_filename.is_empty() {
            if new_flag && let Ok(path) = rustdb_function::file_system::native_path(&new_filename) {
                let _ = std::fs::remove_file(path);
            }
            self.state.db_filename = new_filename.clone();
            self.open_db(ShellOpenFlags::KeepAliveOnFailure)?;
            if self.db.is_none() {
                self.state.print_err(rustdb_common::byte_format!(
                    "Error: cannot open '{new_filename}'\n"
                ));
            }
        }
        if self.db.is_none() {
            // Fall back to an in-memory database.
            self.state.db_filename.clear();
            self.open_db(ShellOpenFlags::ExitOnFailure)?;
        }
        Ok(true)
    }

    /// `ReadFromFile` (`.read FILE`).
    pub fn read_from_file(&mut self, file: impl AsRef<[u8]>) -> bool {
        let file = file.as_ref();
        if self.state.safe_mode {
            self.state.print_err(".read cannot be used in -safe mode\n");
            return false;
        }
        let path = rustdb_function::file_system::native_path(file).ok();
        let opened = path
            .as_ref()
            .and_then(|path| std::fs::metadata(path).ok())
            .filter(std::fs::Metadata::is_file)
            .and_then(|_| path.as_ref().and_then(|path| File::open(path).ok()));
        let Some(handle) = opened else {
            self.state
                .print_err(rustdb_common::byte_format!("Error: cannot open \"{file}\"\n"));
            return false;
        };
        let saved_input = std::mem::replace(
            &mut self.state.input,
            Input::Reader(BufReader::new(Box::new(handle))),
        );
        let saved_lineno = self.state.lineno;
        let rc = self.process_input(InputMode::File);
        self.state.input = saved_input;
        self.state.lineno = saved_lineno;
        rc == 0
    }

    /// `DisplaySchemas` (`.schema ?--indent? ?PATTERN?`).
    pub fn display_schemas<S: AsRef<[u8]>>(&mut self, args: &[S]) -> Result<bool> {
        let args: Vec<ByteString> = args.iter().map(|arg| arg.as_ref().into()).collect();
        let mut name: Option<&[u8]> = None;
        let mut debug = false;
        let mut mode = RenderMode::Semi;
        for arg in &args[1..] {
            if option_match_bytes(arg, b"indent") {
                mode = RenderMode::Pretty;
            } else if option_match_bytes(arg, b"debug") {
                debug = true;
            } else if name.is_none() {
                name = Some(arg.as_bytes());
            } else {
                self.state.print_err("Usage: .schema ?--indent? ?LIKE-PATTERN?\n");
                return Ok(false);
            }
        }
        let mut renderer = renderer_for(&self.state, mode);
        renderer.settings_mut().show_header = false;
        let mut select = ByteString::from("SELECT sql FROM sqlite_master WHERE ");
        if let Some(name) = name {
            let glob = name.iter().any(|c| matches!(c, b'*' | b'?' | b'['));
            select += if name.contains(&b'.') {
                "lower(printf('%s.%s',sname,tbl_name))"
            } else {
                "lower(tbl_name)"
            };
            select += if glob { " GLOB " } else { " LIKE " };
            select += &sql_string_bytes(name);
            if !glob {
                select += " ESCAPE '\\' ";
            }
            select += " AND ";
        }
        select += "type!='meta' AND sql IS NOT NULL ORDER BY name";
        let rc = if debug {
            self.state.print(rustdb_common::byte_format!("SQL: {select};\n"));
            SuccessState::Success
        } else {
            self.render_query(renderer.as_mut(), &select, PagerMode::Off)?
        };
        if rc == SuccessState::Failure {
            self.state.print_err("Error: querying schema information\n");
            return Ok(false);
        }
        Ok(true)
    }

    /// `DisplayTables` (`.tables ?PATTERN?`): the tables and views as boxes of their
    /// columns.
    pub fn display_tables<S: AsRef<[u8]>>(&mut self, args: &[S]) -> MetadataResult {
        let args: Vec<ByteString> = args.iter().map(|arg| arg.as_ref().into()).collect();
        if args.len() > 2 {
            return MetadataResult::PrintUsage;
        }
        let filter_pattern = args.get(1).cloned().unwrap_or_default();
        let mut schema_filter = ByteString::new();
        let mut table_filter = rustdb_common::byte_format!("%{filter_pattern}%");
        if let Ok(components) = parse_qualified_name_components(&filter_pattern)
            && components.len() >= 2
        {
            // "schema.table" or "schema.%".
            schema_filter = rustdb_common::byte_format!("%{}%", components[0]);
            table_filter = rustdb_common::byte_format!("%{}%", components[1]);
        }
        let name_filter = rustdb_common::byte_format!(
            " AND columns.table_name ILIKE {}",
            sql_string_bytes(&table_filter)
        );
        let schema_filter_str = if schema_filter.is_empty() {
            ByteString::new()
        } else {
            rustdb_common::byte_format!(
                " AND columns.schema_name ILIKE {}",
                sql_string_bytes(&schema_filter)
            )
        };
        let query = rustdb_common::byte_format!(
            r"
SELECT columns.database_name, columns.schema_name, columns.table_name, list(
	struct_pack(column_name, data_type, is_primary_key := c.column_index IS NOT NULL) order by column_index),
	t.estimated_size AS estimated_size, t.table_oid AS table_oid
FROM duckdb_columns() columns
LEFT JOIN duckdb_tables() t USING (table_oid)
LEFT JOIN (
	SELECT table_oid, UNNEST(constraint_column_indexes)+1 column_index
	FROM duckdb_constraints()
	WHERE constraint_type='PRIMARY KEY') c
USING (table_oid, column_index)
WHERE NOT columns.internal{schema_filter_str}{name_filter}
GROUP BY ALL;
"
        );
        let result = match self.query(&query) {
            Ok(result) => result,
            Err(error) => {
                self.print_database_error(error.to_bytes());
                return MetadataResult::Fail;
            }
        };
        let mut tables = Vec::with_capacity(result.row_count());
        for row in 0..result.row_count() {
            let mut table = ShellTableInfo {
                database_name: value_to_string(&result.value(0, row)).into(),
                schema_name: value_to_string(&result.value(1, row)).into(),
                table_name: value_to_string(&result.value(2, row)).into(),
                ..ShellTableInfo::default()
            };
            let columns = result.value(3, row);
            for entry in columns.children().unwrap_or_default() {
                let fields = entry.children().unwrap_or_default();
                let field = |index: usize| fields.get(index).cloned().unwrap_or_default();
                table.columns.push(ShellColumnInfo {
                    column_name: value_to_string(&field(0)).into(),
                    column_type: value_to_string(&field(1)),
                    is_primary_key: field(2).as_bool().unwrap_or(false),
                    ..ShellColumnInfo::default()
                });
            }
            let estimated_size = result.value(4, row);
            if !estimated_size.is_null() {
                table.estimated_size = estimated_size.as_i128().and_then(|size| u64::try_from(size).ok());
            }
            table.is_view = result.value(5, row).is_null();
            tables.push(table);
        }
        self.state.render_table_metadata(tables);
        MetadataResult::Success
    }

    /// `DisplayEntries` (`.indexes ?TABLE?`, and the table names of `.tables` in older
    /// versions): names printed in columns.
    pub fn display_entries<S: AsRef<[u8]>>(
        &mut self,
        args: &[S],
        entry_type: char,
    ) -> Result<MetadataResult> {
        let args: Vec<ByteString> = args.iter().map(|arg| arg.as_ref().into()).collect();
        if args.len() > 2 {
            return Ok(MetadataResult::PrintUsage);
        }
        let filter_pattern = args.get(1).cloned().unwrap_or_else(|| ByteString::from("%"));
        let mut schema_filter = ByteString::new();
        let mut table_filter = filter_pattern.clone();
        if let Ok(components) = parse_qualified_name_components(&filter_pattern)
            && components.len() >= 2
        {
            schema_filter.clone_from(&components[0]);
            table_filter.clone_from(&components[1]);
            if table_filter.is_empty() {
                table_filter = "%".into();
            }
        }
        let (query, bind_values) = if entry_type == 't' {
            let (schema_filter_str, name_filter) = if schema_filter.is_empty() {
                (ByteString::new(), "WHERE ao.name LIKE ?1")
            } else {
                (
                    ByteString::from("\n  WHERE schema_name LIKE ?1"),
                    "WHERE ao.name LIKE ?2",
                )
            };
            let query = rustdb_common::byte_format!(
                r"
WITH all_objects AS (
  SELECT schema_name, table_name as name FROM duckdb_tables{schema_filter_str}
  UNION ALL
  SELECT schema_name, view_name as name FROM duckdb_views{schema_filter_str}
),
name_counts AS (
  SELECT name, COUNT(*) as count FROM all_objects
  GROUP BY name
),
disambiguated AS (
  SELECT
    CASE
      WHEN nc.count > 1 THEN ao.schema_name || '.' || ao.name
      ELSE ao.name
    END as display_name
  FROM all_objects ao
  JOIN name_counts nc ON ao.name = nc.name
  {name_filter}
)
SELECT DISTINCT display_name FROM disambiguated ORDER BY display_name
"
            );
            let values = if schema_filter.is_empty() {
                vec![filter_pattern]
            } else {
                vec![schema_filter, table_filter]
            };
            (query, values)
        } else {
            let query = ByteString::from(
                "\nSELECT name FROM\nsqlite_schema\nWHERE type='index' AND tbl_name LIKE ?1",
            );
            (
                query,
                vec![args.get(1).cloned().unwrap_or_else(|| ByteString::from("%"))],
            )
        };
        let prepared = match self.conn()?.prepare(&query) {
            Ok(prepared) => prepared,
            Err(error) => {
                self.print_database_error(error.to_bytes());
                return Ok(MetadataResult::Fail);
            }
        };
        let mut values: rustdb_common::CaseInsensitiveMap<Value> =
            rustdb_common::CaseInsensitiveMap::default();
        for (index, value) in bind_values.into_iter().enumerate() {
            values.insert((index + 1).to_string(), Value::try_varchar(&value)?);
        }
        let mut names = Vec::new();
        match self.conn()?.execute_prepared(&prepared, &values, false) {
            Ok(QueryResult::Materialized(result)) => {
                for row in 0..result.row_count() {
                    names.push(value_to_string(&result.value(0, row)));
                }
            }
            Ok(QueryResult::Stream(stream)) => match self.conn()?.materialize(&stream) {
                Ok(result) => {
                    for row in 0..result.row_count() {
                        names.push(value_to_string(&result.value(0, row)));
                    }
                }
                Err(error) => self.print_database_error(error.to_bytes()),
            },
            Err(error) => self.print_database_error(error.to_bytes()),
        }
        // Pretty-print the names in columns.
        if !names.is_empty() {
            let maxlen = names.iter().map(String::len).max().unwrap_or(0);
            let print_columns = (80 / (maxlen + 2)).max(1);
            let print_rows = names.len().div_ceil(print_columns);
            for i in 0..print_rows {
                let mut j = i;
                while j < names.len() {
                    let prefix = if j < print_rows { "" } else { "  " };
                    self.state.print(format!(
                        "{prefix}{}{}",
                        names[j],
                        " ".repeat(maxlen - names[j].len())
                    ));
                    j += print_rows;
                }
                self.state.print("\n");
            }
        }
        Ok(MetadataResult::Success)
    }

    /// `ShowDatabases` (`.databases`).
    pub fn show_databases(&mut self) -> Result<SuccessState> {
        self.open_db(ShellOpenFlags::ExitOnFailure)?;
        // (`Query` dereferences the connection: an error when there is none.)
        let result = match self.conn()?.query("SELECT name, file FROM pragma_database_list") {
            Ok(result) => result,
            Err(error) => {
                self.print_database_error(error.to_bytes());
                return Ok(SuccessState::Failure);
            }
        };
        let mut table = ShellTableInfo {
            table_name: "databases".into(),
            ..ShellTableInfo::default()
        };
        for row in 0..result.row_count() {
            let file = result.value(1, row);
            table.columns.push(ShellColumnInfo {
                column_name: value_to_string(&result.value(0, row)).into(),
                column_type: if file.is_null() {
                    "(memory)".to_owned()
                } else {
                    value_to_string(&file)
                },
                ..ShellColumnInfo::default()
            });
        }
        self.state.render_table_metadata(vec![table]);
        Ok(SuccessState::Success)
    }

    /// `DoMetaCommand`: runs a dot command; 1 on error, 2 to exit, 0 otherwise.
    pub fn do_meta_command_bytes(&mut self, line: &[u8]) -> i32 {
        let raw_args = split_meta_command_bytes(line);
        if raw_args.is_empty() {
            return 0;
        }
        self.state.clear_temp_file();
        let rc = match metadata_command::find_metadata_command_bytes(&raw_args[0]) {
            Err(error) => {
                self.print_database_error(&error);
                1
            }
            Ok(command) => {
                let result = if let Some(callback) = command.callback_bytes {
                    if command.argument_count == 0 || command.argument_count == raw_args.len() {
                        callback(self, &raw_args)
                    } else {
                        Ok(MetadataResult::PrintUsage)
                    }
                } else {
                    match command.callback {
                        None => {
                            self.state.print_err(format!(
                                "Command \"{}\" is unsupported in the current version of the CLI\n",
                                command.command
                            ));
                            Ok(MetadataResult::Fail)
                        }
                        Some(callback)
                            if command.argument_count == 0 || command.argument_count == raw_args.len() =>
                        {
                            // Explicit compatibility adapter until this callback's
                            // state and engine interfaces also own bytes.
                            let args: Vec<String> = raw_args
                                .iter()
                                .map(|arg| String::from_utf8_lossy(arg).into_owned())
                                .collect();
                            callback(self, &args)
                        }
                        Some(_) => Ok(MetadataResult::PrintUsage),
                    }
                };
                let result = match result {
                    Ok(MetadataResult::PrintUsage) => {
                        let error = format!(
                            "Invalid Command Error: Invalid usage of command '.{}'\n\nUsage: '.{} {}'",
                            String::from_utf8_lossy(&raw_args[0]),
                            command.command,
                            command.usage
                        );
                        self.print_database_error(&error);
                        MetadataResult::Fail
                    }
                    Ok(result) => result,
                    Err(error) => {
                        self.print_database_error(error.to_bytes());
                        MetadataResult::Fail
                    }
                };
                result as i32
            }
        };
        if self.state.out_count > 0 {
            self.state.out_count -= 1;
            if self.state.out_count == 0 {
                self.state.reset_output();
            }
        }
        rc
    }

    /// `RunOneSqlLine`: runs SQL text read from the input; 1 on error.
    fn run_one_sql_line(&mut self, mode: InputMode, sql: &[u8]) -> usize {
        if mode == InputMode::Standard && !sql.is_empty() && !sql.starts_with(b"\x03") {
            self.add_history(sql);
        }
        let timer = Timer::begin(self.state.enable_timer);
        let success = self.execute_sql(sql);
        Timer::end(timer);
        if success != SuccessState::Success {
            return 1;
        }
        if self.state.has_flag(ShellFlag::CountChanges) {
            self.state.print(format!(
                "changes: {:3}   total_changes: {}\n",
                self.state.last_changes, self.state.total_changes
            ));
        }
        0
    }

    /// `RunInitialCommand`: a dot command or SQL of `-c`, `-cmd` or `-ui`.
    pub fn run_initial_command(&mut self, sql: impl AsRef<[u8]>, bail: bool) -> i32 {
        let sql = sql.as_ref();
        if sql.starts_with(b".") {
            let rc = self.do_meta_command_bytes(sql);
            if rc != 0 && bail {
                return if rc == 2 { 0 } else { rc };
            }
        } else {
            let timer = Timer::begin(self.state.enable_timer);
            let result = self.execute_sql(sql);
            Timer::end(timer);
            if result == SuccessState::Failure && bail {
                return 1;
            }
        }
        0
    }

    /// `OneInputLine`: the next line of input, `None` at the end. Interactive input is
    /// read with the line editor and a prompt.
    fn one_input_line(&mut self, is_continuation: bool) -> Option<Vec<u8>> {
        match &mut self.state.input {
            Input::Reader(reader) => return ShellState::read_line(reader),
            Input::Stdin => return ShellState::read_line(&mut std::io::stdin().lock()),
            Input::Interactive => {}
        }
        if self.state.rl_version == ReadLineVersion::Linenoise {
            let prompt = if is_continuation {
                self.state.continue_prompt.clone()
            } else {
                self.generate_prompt()
            };
            let mut completion = |line: &[u8]| linenoise_completion(self.conn.as_mut(), line);
            let line = crate::linenoise::linenoise(&prompt, Some(&mut completion))?;
            return Some(line);
        }
        // Without the line editor: print the prompt and read a line.
        if is_continuation {
            self.state.print(&self.state.continue_prompt);
        } else {
            self.print_prompt();
        }
        printer::flush_stdout();
        ShellState::read_line(&mut std::io::stdin().lock())
    }

    /// The main prompt with its color codes (`Prompt::GeneratePrompt`); DuckDB does not
    /// catch an error of it.
    fn generate_prompt(&mut self) -> rustdb_common::ByteString {
        let prompt = self.state.main_prompt.clone();
        let generated = self
            .conn()
            .and_then(|conn| prompt.generate_prompt(&mut ConnectionPromptContext(conn)));
        generated.unwrap_or_else(|error| fatal(&error))
    }

    /// `Prompt::PrintPrompt` of the main prompt.
    fn print_prompt(&mut self) {
        let prompt = self.state.main_prompt.clone();
        let out = self.state.out.clone();
        let printed = self.conn().and_then(|conn| {
            prompt.print_prompt(&mut ConnectionPromptContext(conn), &out, PrintOutput::Stdout)
        });
        if let Err(error) = printed {
            fatal(&error);
        }
    }

    /// `ProcessInput`: reads and runs input until its end, an `.exit`, or an error when
    /// bailing; returns 1 if there were errors.
    pub fn process_input(&mut self, mut mode: InputMode) -> i32 {
        let mut sql = Vec::new();
        let mut err_count = 0usize;
        let mut num_ctrl_c = 0usize;
        self.state.lineno = 0;
        let interactive = |shell: &Self| {
            matches!(shell.state.input, Input::Interactive) && shell.state.stdin_is_interactive
        };
        while err_count == 0 || !self.state.bail_on_error(mode) || interactive(self) {
            self.state.out.flush();
            let Some(line) = self.one_input_line(!sql.is_empty()) else {
                // The end of the input.
                if interactive(self) {
                    ShellState::printf_stdout("\n");
                }
                break;
            };
            // Input after an interrupted query: messages print again.
            if interrupt::seen_interrupt() {
                if !matches!(self.state.input, Input::Interactive) {
                    break;
                }
                ShellState::clear_interrupt();
            }
            if line.starts_with(b"\x03") {
                // Ctrl-C clears the statement; twice on an empty line shows a hint.
                if sql.is_empty() && line.len() == 1 && self.state.stdin_is_interactive {
                    num_ctrl_c += 1;
                    if num_ctrl_c >= 2 {
                        self.state.print("Interrupted, use Ctrl+D to exit\n");
                    }
                }
                sql.clear();
                continue;
            }
            num_ctrl_c = 0;
            if mode == InputMode::DuckdbRc && !line.starts_with(b".startup_text") {
                if self.state.startup_text == StartupText::All {
                    self.state.print_highlighted(
                        &rustdb_common::byte_format!(
                            "-- Loading resources from {}\n",
                            self.state.duckdb_rc_path
                        ),
                        PrintOutput::Stderr,
                        HighlightElementType::StartupText,
                    );
                    self.state.displayed_loading_resources_message = true;
                }
                mode = InputMode::File;
            }
            self.state.lineno += 1;
            if sql.is_empty() && all_whitespace(&line) {
                if self.state.has_flag(ShellFlag::Echo) {
                    ShellState::printf_stdout(&line);
                    ShellState::printf_stdout(b"\n");
                }
                continue;
            }
            if sql.is_empty() && (line.starts_with(b".") || line.starts_with(b"#")) {
                if self.state.has_flag(ShellFlag::Echo) {
                    ShellState::printf_stdout(&line);
                    ShellState::printf_stdout(b"\n");
                }
                if line.starts_with(b".") {
                    if mode == InputMode::Standard {
                        self.add_history(&line);
                    }
                    let rc = self.do_meta_command_bytes(&line);
                    if rc == 2 {
                        // Exit requested.
                        break;
                    } else if rc != 0 {
                        err_count += 1;
                    }
                }
                continue;
            }
            let prior_length = sql.len();
            if sql.is_empty() {
                let start = line
                    .iter()
                    .position(|&byte| !is_space(byte))
                    .unwrap_or(line.len());
                sql.extend_from_slice(&line[start..]);
            } else {
                sql.push(b'\n');
                sql.extend_from_slice(&line);
            }
            if !sql.is_empty() && sql[prior_length..].contains(&b';') && sql_is_complete(&sql) {
                err_count += self.run_one_sql_line(mode, &sql);
                sql.clear();
                if self.state.out_count > 0 {
                    self.state.reset_output();
                    self.state.out_count = 0;
                } else {
                    self.state.clear_temp_file();
                }
            } else if !sql.is_empty() && all_whitespace(&sql) {
                if self.state.has_flag(ShellFlag::Echo) {
                    ShellState::printf_stdout(&sql);
                    ShellState::printf_stdout(b"\n");
                }
                sql.clear();
            }
        }
        if !sql.is_empty() && !all_whitespace(&sql) {
            err_count += self.run_one_sql_line(mode, &sql);
        }
        i32::from(err_count > 0)
    }

    /// `ProcessFile`: runs the commands of `file`; a missing default init file is not an
    /// error.
    pub fn process_file(&mut self, file: impl AsRef<[u8]>, mode: InputMode, default_duckdb_rc: bool) -> bool {
        let file = file.as_ref();
        let saved_lineno = self.state.lineno;
        let rc = match rustdb_function::file_system::native_path(file)
            .ok()
            .and_then(|path| File::open(path).ok())
        {
            Some(handle) => {
                let saved_input = std::mem::replace(
                    &mut self.state.input,
                    Input::Reader(BufReader::new(Box::new(handle))),
                );
                let rc = self.process_input(mode);
                self.state.input = saved_input;
                rc
            }
            None if mode != InputMode::DuckdbRc || !default_duckdb_rc => {
                self.print_database_error(rustdb_common::byte_format!(
                    "IO Error: Failed to open file \"{file}\""
                ));
                1
            }
            None => 0,
        };
        self.state.lineno = saved_lineno;
        rc == 0
    }

    /// `ProcessDuckDBRC`: the init file, `~/.duckdbrc` by default.
    pub fn process_duckdbrc(&mut self, file: Option<&[u8]>) -> bool {
        let (path, is_default) = if let Some(file) = file {
            (ByteString::from(file), false)
        } else {
            let home = home_directory();
            if home.is_empty() {
                self.state
                    .print_err("-- warning: cannot find home directory; cannot read ~/.duckdbrc\n");
                return true;
            }
            (rustdb_common::byte_format!("{home}/.duckdbrc"), true)
        };
        self.state.duckdb_rc_path.clone_from(&path);
        self.process_file(&path, InputMode::DuckdbRc, is_default)
    }

    /// `SetOutputFile` (`.output`, `.once`, `.excel`): output to a file, a pipe or the
    /// system viewer.
    pub fn set_output_file<S: AsRef<[u8]>>(&mut self, args: &[S], output_mode: char) -> bool {
        let args: Vec<ByteString> = args.iter().map(|arg| arg.as_ref().into()).collect();
        let state = &mut self.state;
        if state.safe_mode {
            state.print_err(".output/.once/.excel cannot be used in -safe mode\n");
            return false;
        }
        let mut file = ByteString::new();
        let mut editor_mode = None;
        let mut bom = false;
        let once = match output_mode {
            'e' => {
                editor_mode = Some('x');
                true
            }
            'o' => true,
            _ => false,
        };
        for arg in &args[1..] {
            if arg.starts_with(b"-") {
                let z = arg
                    .strip_prefix(b"-")
                    .filter(|z| z.starts_with(b"-"))
                    .unwrap_or(arg);
                if z == b"-bom" {
                    bom = true;
                } else if output_mode != 'e' && z == b"-x" {
                    editor_mode = Some('x');
                } else if output_mode != 'e' && z == b"-e" {
                    editor_mode = Some('e');
                } else {
                    state.print(rustdb_common::byte_format!(
                        "ERROR: unknown option: \"{arg}\".  Usage:\n"
                    ));
                    metadata_command::print_help(state, Some(args[0].as_bytes()));
                    return false;
                }
            } else if file.is_empty() {
                file.clone_from(arg);
            } else {
                state.print(rustdb_common::byte_format!(
                    "ERROR: extra parameter: \"{arg}\".  Usage:\n"
                ));
                metadata_command::print_help(state, Some(args[0].as_bytes()));
                return false;
            }
        }
        if file.is_empty() {
            file = "stdout".into();
        }
        state.out_count = if once { 2 } else { 0 };
        state.reset_output();
        if let Some(editor_mode) = editor_mode {
            state.do_xdg_open = true;
            state.push_output_mode();
            if editor_mode == 'x' {
                // A spreadsheet: CSV.
                state.new_temp_file("csv");
                state.clear_flag(ShellFlag::Echo);
                state.mode = RenderMode::Csv;
                state.col_separator = crate::state::SEP_COMMA.into();
                state.row_separator = crate::state::SEP_CRLF.into();
            } else {
                state.new_temp_file("txt");
            }
            file = state.temp_file.clone().into();
        }
        if let Some(command) = file.strip_prefix(b"|") {
            let child = std::process::Command::new("/bin/sh")
                .arg("-c")
                .arg(match rustdb_function::file_system::native_path(command) {
                    Ok(command) => command,
                    Err(_) => return false,
                })
                .stdin(std::process::Stdio::piped())
                .spawn();
            let Some((child, stdin)) = child.ok().and_then(|mut child| {
                let stdin = child.stdin.take()?;
                Some((child, stdin))
            }) else {
                state.print_err(rustdb_common::byte_format!(
                    "Error: cannot open pipe \"{command}\"\n"
                ));
                return false;
            };
            let mut stdin = std::io::BufWriter::new(stdin);
            if bom {
                use std::io::Write;
                let _ = stdin.write_all(b"\xef\xbb\xbf");
            }
            state.out.replace(OutputTarget::Pipe { child, stdin });
        } else {
            let Some(target) = state.open_output_file(&file) else {
                if file == "off" {
                    state.print_err(rustdb_common::byte_format!("Error: cannot write to \"{file}\"\n"));
                }
                return false;
            };
            state.out.replace(target);
            if bom {
                state.print(b"\xef\xbb\xbf");
            }
        }
        state.outfile = file;
        state.stdout_is_console = false;
        true
    }

    /// `ChangeDirectory` (`.cd`).
    pub fn change_directory(&mut self, path: impl AsRef<[u8]>) -> SuccessState {
        let path = path.as_ref();
        let expanded = crate::state::expand_path_bytes(path);
        let result = rustdb_function::file_system::native_path(&expanded)
            .ok()
            .and_then(|path| std::env::set_current_dir(path).ok());
        if result.is_none() {
            self.state.print_err(rustdb_common::byte_format!(
                "Cannot change to directory \"{path}\"\n"
            ));
            return SuccessState::Failure;
        }
        SuccessState::Success
    }

    /// `RenderLastResult` (`.last`): the last result again, without limits.
    pub fn render_last_result(&mut self) -> Result<MetadataResult> {
        let Some(last_result) = self.state.last_result() else {
            return Ok(MetadataResult::Success);
        };
        let mut renderer = renderer_for(&self.state, self.state.c_mode);
        renderer.remove_render_limits();
        let result = self.render_query_result(
            renderer.as_mut(),
            ResultSource::Materialized(last_result),
            PagerMode::Automatic,
        )?;
        Ok(if result == SuccessState::Failure {
            MetadataResult::Fail
        } else {
            MetadataResult::Success
        })
    }
}

/// `linenoise_completion`: dot commands complete from their names, SQL from
/// `sql_auto_complete`; failures complete nothing.
fn linenoise_completion(
    conn: Option<&mut Connection>,
    line: &[u8],
) -> Vec<crate::linenoise::CompletionCandidate> {
    use crate::linenoise::CompletionCandidate;
    let text = String::from_utf8_lossy(line);
    if text.starts_with('.') {
        return metadata_command::metadata_completions(&text)
            .into_iter()
            .map(|completion| CompletionCandidate {
                completion: completion.into_bytes(),
                start: 0,
                completion_type: "keyword".to_owned(),
                score: 0,
                extra_char: 0,
            })
            .collect();
    }
    if text.starts_with('#') {
        return Vec::new();
    }
    let Some(conn) = conn else {
        return Vec::new();
    };
    let Ok(result) = conn.query(format!("CALL sql_auto_complete({})", sql_string(&text))) else {
        return Vec::new();
    };
    let mut candidates = Vec::with_capacity(result.row_count());
    for row in 0..result.row_count() {
        let extra = result.value(4, row);
        let extra_char = if extra.is_null() {
            0
        } else {
            match value_to_string(&extra).as_bytes() {
                [c] => *c,
                _ => 0,
            }
        };
        let number = |column: usize| {
            result
                .value(column, row)
                .as_i128()
                .and_then(|value| u64::try_from(value).ok())
        };
        candidates.push(CompletionCandidate {
            completion: value_to_string(&result.value(0, row)).into_bytes(),
            start: number(1).unwrap_or(0) as usize,
            completion_type: value_to_string(&result.value(2, row)),
            score: number(3).unwrap_or(0),
            extra_char,
        });
    }
    candidates
}

/// When the progress bar of a query prints, the time after which it shows
/// (`enable_progress_bar`, `enable_progress_bar_print` and `progress_bar_time`).
fn progress_bar_wait_time(conn: &Connection) -> Option<std::time::Duration> {
    let enabled =
        |name: &str| conn.current_setting(name).ok().and_then(|value| value.as_bool()) == Some(true);
    if !enabled("enable_progress_bar") || !enabled("enable_progress_bar_print") {
        return None;
    }
    let wait_time = conn
        .current_setting("progress_bar_time")
        .ok()
        .and_then(|value| value.as_i128())
        .and_then(|millis| u64::try_from(millis).ok())
        .unwrap_or(2000);
    Some(std::time::Duration::from_millis(wait_time))
}

/// An error DuckDB's shell does not catch: `main` reports it and exits with code 1.
pub fn fatal(error: &rustdb::Error) -> ! {
    crate::printer::write_stderr(format!("Exited due to error: {error}").as_bytes());
    exit(1)
}

/// The generic parameters of `.import` (an `unordered_map<string, string>`), iterated in
/// libc++'s order.
#[derive(Debug, Default)]
struct GenericParameters {
    order: rustdb_common::libcxx::UnorderedOrder<ByteString>,
    values: Vec<ByteString>,
}

impl GenericParameters {
    /// `generic_parameters[name] = value`.
    fn set(&mut self, name: impl AsRef<[u8]>, value: impl AsRef<[u8]>) {
        let name = name.as_ref();
        let value = value.as_ref();
        let hash = rustdb_common::libcxx::string_hash(name);
        let (inserted, index) = self.order.insert(hash, ByteString::from(name), |a, b| a == b);
        if inserted {
            self.values.push(ByteString::from(value));
        } else {
            self.values[index] = value.into();
        }
    }

    fn contains(&self, name: impl AsRef<[u8]>) -> bool {
        self.order
            .keys()
            .iter()
            .any(|key| key.as_bytes() == name.as_ref())
    }

    fn iter(&self) -> impl Iterator<Item = (&ByteString, &ByteString)> {
        self.order
            .order()
            .into_iter()
            .map(|index| (&self.order.keys()[index], &self.values[index]))
    }
}

/// The number of changed rows a statement reports (`DefaultTryCastAs(BIGINT)`).
fn changed_row_count(conn: &Connection, value: &Value) -> Option<u64> {
    if value.is_null() {
        return None;
    }
    let count = conn.cast_value(value, &rustdb::LogicalType::BIGINT).ok()?;
    count.as_i128().map(|count| count as u64)
}

/// `optionMatch`: `-name` or `--name`.
fn option_match_bytes(arg: &[u8], option: &[u8]) -> bool {
    let Some(rest) = arg.strip_prefix(b"-") else {
        return false;
    };
    rest.strip_prefix(b"-").unwrap_or(rest) == option
}

/// `QualifiedName::ParseComponents`: the dot-separated parts of a name, with double
/// quotes protecting dots.
pub fn parse_qualified_name_components(
    input: impl AsRef<[u8]>,
) -> std::result::Result<Vec<ByteString>, ByteString> {
    let input = input.as_ref();
    let mut result = Vec::new();
    let mut entry = ByteString::new();
    let mut bytes = input.iter().copied().peekable();
    while let Some(byte) = bytes.next() {
        match byte {
            b'"' => {
                loop {
                    match bytes.next() {
                        None => {
                            return Err(rustdb_common::byte_format!(
                                "Unterminated quote in qualified name! (input: {input})"
                            ));
                        }
                        Some(b'"') if bytes.peek() == Some(&b'"') => {
                            bytes.next();
                            entry.push(b'"');
                        }
                        Some(b'"') => break,
                        Some(byte) => entry.push(byte),
                    }
                }
                if !matches!(bytes.peek(), None | Some(b'.')) {
                    return Err(rustdb_common::byte_format!(
                        "Expected dot or end of string after quoted name! (input: {input})"
                    ));
                }
            }
            b'.' => result.push(std::mem::take(&mut entry)),
            _ => entry.push(byte),
        }
    }
    result.push(entry);
    Ok(result)
}

/// Splits a dot command into its arguments (`DoMetaCommand`): whitespace-separated, with
/// quoted arguments, and C escapes resolved outside single quotes.
/// The shell tokenizer owns C-string argument bytes, including escape-decoded bytes.
pub fn split_meta_command_bytes(bytes: &[u8]) -> Vec<Vec<u8>> {
    let mut args = Vec::new();
    // Skip the initial dot.
    let mut pos = 1;
    while pos < bytes.len() {
        while pos < bytes.len() && is_space(bytes[pos]) {
            pos += 1;
        }
        if pos >= bytes.len() {
            break;
        }
        let mut arg = Vec::new();
        if bytes[pos] == b'\'' || bytes[pos] == b'"' {
            // A quoted argument, up to the next quote.
            let quote = bytes[pos];
            pos += 1;
            while pos < bytes.len() && bytes[pos] != quote {
                if bytes[pos] == b'\\' && quote == b'"' && pos + 1 < bytes.len() {
                    // Keep escaped characters.
                    arg.push(bytes[pos]);
                    pos += 1;
                }
                arg.push(bytes[pos]);
                pos += 1;
            }
            if pos < bytes.len() {
                pos += 1;
            }
            args.push(if quote == b'"' {
                resolve_backslashes_bytes(&arg)
            } else {
                arg
            });
        } else {
            // An unquoted argument, up to the next space.
            while pos < bytes.len() && !is_space(bytes[pos]) {
                arg.push(bytes[pos]);
                pos += 1;
            }
            args.push(resolve_backslashes_bytes(&arg));
        }
    }
    for arg in &mut args {
        if let Some(nul) = arg.iter().position(|&byte| byte == 0) {
            arg.truncate(nul);
        }
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_meta_arguments_preserve_bytes_and_c_string_termination() {
        assert_eq!(
            split_meta_command_bytes(b".print \"caf\xe9\""),
            vec![b"print".to_vec(), b"caf\xe9".to_vec()]
        );
        assert_eq!(
            split_meta_command_bytes(b".print \"caf\\351\" 'caf\\351' final\\000ignored tail"),
            vec![
                b"print".to_vec(),
                b"caf\xe9".to_vec(),
                b"caf\\351".to_vec(),
                b"final".to_vec(),
                b"tail".to_vec()
            ]
        );
    }

    #[test]
    fn input_and_completeness_do_not_decode_utf8() {
        let mut reader = std::io::Cursor::new(b"caf\xe9\r\n".as_slice());
        assert_eq!(ShellState::read_line(&mut reader), Some(b"caf\xe9".to_vec()));
        assert_eq!(ShellState::read_line(&mut reader), None);
        assert!(all_whitespace(b"-- caf\xe9\n"));
        assert!(sql_is_complete(b"SELECT 1; -- caf\xff"));
        assert!(!sql_is_complete(b"SELECT $caf\xe9$ unfinished"));
    }

    #[test]
    fn completeness() {
        assert!(sql_is_complete("SELECT 1;"));
        assert!(!sql_is_complete("SELECT ';"));
        assert!(sql_is_complete("SELECT 1; -- comment"));
        assert!(!sql_is_complete("SELECT $$;"));
        assert!(sql_is_complete("SELECT $a$ ; $a$;"));
        assert!(!sql_is_complete("SELECT /* ; */"));
    }

    #[test]
    fn meta_command_arguments() {
        assert_eq!(
            split_meta_command_bytes(b".mode csv"),
            [b"mode".to_vec(), b"csv".to_vec()]
        );
        assert_eq!(
            split_meta_command_bytes(b".print 'a b' \"c\\td\""),
            [b"print".to_vec(), b"a b".to_vec(), b"c\td".to_vec()]
        );
        assert_eq!(
            split_meta_command_bytes(b".separator \\t"),
            [b"separator".to_vec(), b"\t".to_vec()]
        );
    }

    #[test]
    fn qualified_names() {
        assert_eq!(parse_qualified_name_components("a.b").unwrap(), ["a", "b"]);
        assert_eq!(parse_qualified_name_components("\"a.b\"").unwrap(), ["a.b"]);
        assert_eq!(parse_qualified_name_components("s.").unwrap(), ["s", ""]);
    }

    #[test]
    fn whitespace() {
        assert!(all_whitespace("  -- comment"));
        assert!(all_whitespace("/* x */ "));
        assert!(!all_whitespace("/* x"));
        assert!(!all_whitespace("SELECT"));
    }
}
