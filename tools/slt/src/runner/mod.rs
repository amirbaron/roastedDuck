//! The sqllogictest runner (port of `SQLLogicTestRunner` in `test/sqlite/sqllogic_test_runner.cpp`).
//!
//! [`run_file`] runs one test file to completion and returns its verdict. The runner walks the
//! file record by record ([`Runner::execute_file`]), turning records into [`Command`]s that run
//! immediately or, inside loops, once the loop is complete (`execute.rs`). Result checks live in
//! `check.rs` and `require` handling in `require.rs`.

mod check;
mod execute;
mod require;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use crate::backend::{Engine, ExtensionLoad, RequireSetting};
use crate::commands::{
    Command, CommandKind, ExecuteContext, LoadCommand, LoopCommand, LoopDefinition, QueryCommand,
    StatementCommand,
};
use crate::config::TestEnvironment;
use crate::outcome::{Failure, FileResult, TestResult};
use crate::parser::{self, ExpectedResult, Parser, SortStyle, Token, TokenKind};
use crate::text;

/// Error messages that end a test early but still count it as skipped (network access).
const DEFAULT_IGNORE_ERROR_MESSAGES: [&str; 2] = ["HTTP", "Unable to connect"];
/// Error messages that fail a test even when an error is expected.
const DEFAULT_ALWAYS_FAIL_ERROR_MESSAGES: [&str; 2] = ["differs from original result!", "INTERNAL"];
/// Extensions every database loads on start (`TestConfiguration::ExtensionToBeLoadedOnLoad`).
const EXTENSIONS_LOADED_ON_START: [&str; 1] = ["core_functions"];

/// Runs the test file `file` (a path relative to the working directory, as DuckDB's `unittest`
/// names tests) on `engine` and returns its verdict.
pub fn run_file<E: Engine>(engine: &E, environment: &TestEnvironment, file: &str) -> FileResult {
    let started = Instant::now();
    let (outcome, skip_reason) = match Runner::new(engine, environment) {
        Ok(mut runner) => {
            runner.set_test_variables(file);
            let outcome = runner.execute_file(file);
            let skip_reason = runner.skip_reason();
            runner.close();
            (outcome, skip_reason)
        }
        Err(failure) => (Err(failure), None),
    };
    let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    FileResult::from_run(file.to_owned(), outcome, skip_reason, duration_ms)
}

/// Mutable flow information for one pass over a file.
#[derive(Debug, Default)]
struct FileState {
    /// `mode skip` nesting. Like DuckDB's unsigned counter it wraps around: an `unskip` without a
    /// `skip` skips everything until the next `skip`.
    skip_level: usize,
    test_expr_executed: bool,
    tags_seen: bool,
    tags: Vec<String>,
}

/// What the file loop does after a record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Flow {
    Next,
    /// `halt`: stop reading the file.
    Halt,
    /// The test was skipped (`SKIP_TEST` + return).
    Stop,
}

/// `mode output_hash|output_result|debug` and `statement debug` switches.
#[derive(Clone, Copy, Debug, Default)]
struct OutputModes {
    /// Print result hashes instead of checking results.
    hash: bool,
    /// Print query results.
    result: bool,
    debug: bool,
}

/// State of one test file run (`SQLLogicTestRunner`).
pub struct Runner<'a, E: Engine> {
    engine: &'a E,
    environment: &'a TestEnvironment,
    file_name: String,
    dbpath: String,
    loaded_databases: Vec<String>,
    config: E::Config,
    db: Option<E::Database>,
    con: Option<E::Connection>,
    named_connections: HashMap<String, E::Connection>,
    extensions: BTreeSet<String>,
    modes: OutputModes,
    finished_processing_file: AtomicBool,
    hash_threshold: i64,
    /// Loops whose bodies are being collected, innermost last.
    loop_stack: Vec<LoopCommand>,
    original_sqlite_test: bool,
    skip_reload: bool,
    environment_variables: BTreeMap<String, String>,
    local_extension_repo: String,
    ignore_error_messages: Vec<String>,
    always_fail_error_messages: Vec<String>,
    /// Query label → result hash.
    hash_labels: Mutex<HashMap<String, String>>,
    skip_reason: Mutex<Option<String>>,
    /// Files that already reported a failure (`FailureSummary::SkipLoggingSameError`).
    reported_files: Mutex<HashSet<String>>,
}

impl<'a, E: Engine> Runner<'a, E> {
    /// The runner constructor: the test configuration plus the runner's own settings.
    pub fn new(engine: &'a E, environment: &'a TestEnvironment) -> TestResult<Self> {
        let mut config = engine.test_config().map_err(Failure::new)?;
        // Native corpus runs can use the same default memory budget as a bounded
        // VM. This applies to both backends; tests may still set their own limit.
        if let Ok(limit) = std::env::var("SLT_MEMORY_LIMIT") {
            engine
                .set_option(&mut config, "memory_limit", &limit)
                .map_err(Failure::new)?;
        }
        // Autoloading mode is always NONE (no test configuration file); LOCAL_EXTENSION_REPO
        // still enables autoloading from a local repository like in DuckDB's runner.
        let local_extension_repo = std::env::var("LOCAL_EXTENSION_REPO").unwrap_or_default();
        let autoload = !local_extension_repo.is_empty();
        for (name, value) in [
            ("allow_unredacted_secrets", "true"),
            ("allow_unsigned_extensions", "true"),
            (
                "autoinstall_known_extensions",
                if autoload { "true" } else { "false" },
            ),
            (
                "autoload_known_extensions",
                if autoload { "true" } else { "false" },
            ),
        ] {
            engine
                .set_option(&mut config, name, value)
                .map_err(Failure::new)?;
        }
        Ok(Self {
            engine,
            environment,
            file_name: String::new(),
            dbpath: String::new(),
            loaded_databases: Vec::new(),
            config,
            db: None,
            con: None,
            named_connections: HashMap::new(),
            extensions: BTreeSet::new(),
            modes: OutputModes::default(),
            finished_processing_file: AtomicBool::new(false),
            hash_threshold: 0,
            loop_stack: Vec::new(),
            original_sqlite_test: false,
            skip_reload: false,
            environment_variables: BTreeMap::new(),
            local_extension_repo,
            ignore_error_messages: DEFAULT_IGNORE_ERROR_MESSAGES.map(str::to_owned).to_vec(),
            always_fail_error_messages: DEFAULT_ALWAYS_FAIL_ERROR_MESSAGES.map(str::to_owned).to_vec(),
            hash_labels: Mutex::new(HashMap::new()),
            skip_reason: Mutex::new(None),
            reported_files: Mutex::new(HashSet::new()),
        })
    }

    /// The test variables `unittest` defines for every file (`testRunner`).
    fn set_test_variables(&mut self, file: &str) {
        self.environment_variables = self.environment.test_variables();
        self.environment_variables.insert(
            "WORKING_DIR".to_owned(),
            self.environment.working_directory().to_owned(),
        );
        self.environment_variables
            .insert("TEST_NAME".to_owned(), file.to_owned());
        self.environment_variables
            .insert("TEST_NAME__NO_SLASH".to_owned(), file.replace('/', "_"));
    }

    /// The `SKIP_TEST` reason, if the test was skipped.
    pub fn skip_reason(&self) -> Option<String> {
        self.skip_reason
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn skip_test(&self, reason: String) {
        *self.skip_reason.lock().unwrap_or_else(PoisonError::into_inner) = Some(reason);
    }

    fn finished(&self) -> bool {
        self.finished_processing_file.load(Ordering::SeqCst)
    }

    fn finish_processing_file(&self) {
        self.finished_processing_file.store(true, Ordering::SeqCst);
    }

    /// The runner destructor: closes the database and deletes database files it created.
    pub fn close(mut self) {
        self.con = None;
        self.named_connections.clear();
        self.db = None;
        let test_directory = self.environment.test_directory().to_owned();
        for path in &self.loaded_databases {
            if !path.is_empty() && path.starts_with(&test_directory) {
                TestEnvironment::delete_database(path);
            }
        }
    }

    fn in_loop(&self) -> bool {
        !self.loop_stack.is_empty()
    }

    fn main_connection(&self) -> TestResult<&E::Connection> {
        self.con
            .as_ref()
            .ok_or_else(|| Failure::new("no open connection"))
    }

    fn main_connection_mut(&mut self) -> TestResult<&mut E::Connection> {
        self.con
            .as_mut()
            .ok_or_else(|| Failure::new("no open connection"))
    }

    fn database(&self) -> TestResult<&E::Database> {
        self.db.as_ref().ok_or_else(|| Failure::new("no open database"))
    }

    /// `SQLLogicTestRunner::LoadDatabase`: (re)opens `dbpath` and reconnects.
    fn load_database(&mut self, dbpath: &str, load_extensions: bool) -> TestResult {
        self.loaded_databases.push(dbpath.to_owned());
        self.db = None;
        self.con = None;
        self.named_connections.clear();
        let database = self
            .engine
            .open(dbpath, &self.config)
            .map_err(|error| Failure::new(format!("Failed to load database {dbpath}: {error}")))?;
        // `SLT_STATIC_EXTENSIONS` (comma-separated) stands in for a test configuration's
        // `statically_loaded_extensions` in validation runs.
        let configured = std::env::var("SLT_STATIC_EXTENSIONS").unwrap_or_default();
        let configured = configured
            .split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty());
        for extension in EXTENSIONS_LOADED_ON_START.into_iter().chain(configured) {
            self.engine
                .load_extension(&database, extension)
                .map_err(|error| Failure::new(format!("Failed to load database {dbpath}: {error}")))?;
        }
        self.db = Some(database);
        self.reconnect()?;
        if dbpath.is_empty() || dbpath == ":memory:" {
            // An in-memory database spills to `.tmp` in the working directory, which the
            // files running next to this one share (DuckDB's runner runs one file at a
            // time); give it one of its own.
            let temp_directory = self.environment.test_path(".tmp");
            let connection = self
                .con
                .as_mut()
                .ok_or_else(|| Failure::new("no open connection"))?;
            let sql = format!("SET temp_directory='{temp_directory}'");
            let result = self.engine.query(connection, &sql);
            if let Some(error) = crate::backend::QueryResult::error(&result) {
                return Err(Failure::new(format!("Failed to load database {dbpath}: {error}")));
            }
        }
        if load_extensions {
            // Reopened databases need the same explicit external dependencies
            // as the first require, including their main-connection state.
            let extensions: Vec<_> = self.extensions.iter().cloned().collect();
            for extension in extensions {
                self.load_extension(&extension).map_err(Failure::new)?;
            }
        }
        Ok(())
    }

    /// `SQLLogicTestRunner::Reconnect`: a fresh main connection with the runner's session setup.
    fn reconnect(&mut self) -> TestResult {
        let mut connection = self.engine.connect(self.database()?).map_err(Failure::new)?;
        if self.original_sqlite_test {
            self.engine.query(&mut connection, "SET integer_division=true");
        }
        let secret_directory = self.environment.test_path("test_secret_dir");
        self.engine.query(
            &mut connection,
            &format!("SET secret_directory='{secret_directory}'"),
        );
        self.engine.configure_test_client(&mut connection);
        if !self.local_extension_repo.is_empty() {
            let sql = format!(
                "SET autoinstall_extension_repository='{}'",
                self.local_extension_repo
            );
            self.engine.query(&mut connection, &sql);
        }
        // `on_init` + ";" + `on_new_connection`: both empty in the default configuration.
        // `SLT_ON_INIT` stands in for a test configuration's `on_init` (for instance
        // `test/configs/peg_parser_strict.json`'s) in validation runs.
        let on_init = std::env::var("SLT_ON_INIT").unwrap_or_default();
        let init = self.replace_keywords(
            &self
                .environment
                .process_path(&format!("{on_init};"), &self.file_name),
        );
        let result = self.engine.query(&mut connection, &init);
        self.con = Some(connection);
        if let Some(error) = crate::backend::QueryResult::error(&result) {
            return Err(Failure::new(format!(
                "Startup queries provided via on_init failed: {error}"
            )));
        }
        Ok(())
    }

    /// `SQLLogicTestRunner::ReplaceKeywords`: test variables, paths and `__BUILD_DIRECTORY__`.
    fn replace_keywords(&self, input: &str) -> String {
        let mut text = input.to_owned();
        for (name, value) in &self.environment_variables {
            text = text.replace(&format!("${{{name}}}"), value);
            text = text.replace(&format!("{{{name}}}"), value);
        }
        self.environment
            .process_path(&text, &self.file_name)
            .replace("__BUILD_DIRECTORY__", self.engine.build_directory())
    }

    /// `SQLLogicTestRunner::LoopReplacement`: substitutes `{name}`/`${name}` for every running
    /// loop (comma-separated names take comma-separated values).
    fn loop_replacement(&self, text: &str, loops: &[LoopDefinition]) -> TestResult<String> {
        let mut text = text.to_owned();
        for active in loops {
            let value = active.current_value().ok_or_else(|| {
                Failure::new(format!(
                    "loop {}: index {} is out of range",
                    active.iterator_name, active.index
                ))
            })?;
            let replacement = self.replace_keywords(&value);
            if active.iterator_name.contains(',') {
                let names = text::split(&active.iterator_name, ",");
                let values = text::split(&replacement, ",");
                if names.len() != values.len() {
                    return Err(Failure::new(format!(
                        "foreach loop: number of commas in loop iterator ({}) does not match number of commas in \
                         replacement ({replacement})",
                        active.iterator_name
                    )));
                }
                for (name, value) in names.iter().zip(&values) {
                    text = replace_loop_iterator(&text, name, value);
                }
            } else {
                text = replace_loop_iterator(&text, &active.iterator_name, &replacement);
            }
        }
        Ok(text)
    }

    /// Runs a command now, or adds it to the loop being collected (`ExecuteCommand`).
    fn execute_command(&mut self, command: Command) -> TestResult {
        if let Some(current) = self.loop_stack.last_mut() {
            current.commands.push(command);
            return Ok(());
        }
        self.execute(&command, &mut ExecuteContext::default())
    }

    fn start_loop(&mut self, definition: LoopDefinition) {
        self.loop_stack.push(LoopCommand {
            definition,
            commands: Vec::new(),
        });
    }

    /// `endloop`: closes the innermost loop and runs it once the outermost loop is complete.
    fn end_loop(&mut self) -> TestResult {
        let finished = self
            .loop_stack
            .pop()
            .ok_or_else(|| Failure::new("endloop without active loop!"))?;
        let command = Command::bare(CommandKind::Loop(finished));
        match self.loop_stack.last_mut() {
            Some(parent) => {
                parent.commands.push(command);
                Ok(())
            }
            None => self.execute(&command, &mut ExecuteContext::default()),
        }
    }

    /// `SQLLogicTestRunner::ExecuteFile`.
    pub fn execute_file(&mut self, script: &str) -> TestResult {
        script.clone_into(&mut self.file_name);
        if script.contains("test/sqlite/select") || script.contains("third_party/sqllogictest") {
            self.original_sqlite_test = true;
        }
        self.load_database(&self.dbpath.clone(), true)?;
        let mut parser = Parser::open(Path::new(script), script).map_err(|_| {
            Failure::new(format!(
                "Could not find test script '{script}'. Perhaps run `make sqlite`. "
            ))
        })?;
        let mut state = FileState::default();
        match Path::new(script)
            .extension()
            .and_then(|extension| extension.to_str())
        {
            Some("test_slow") => state.tags.push("slow".to_owned()),
            Some("test_coverage") => state.tags.push("coverage".to_owned()),
            _ => {}
        }
        while parser.next_statement() {
            let token = parser.tokenize()?;
            if token.kind.is_single_line() && !parser.next_line_empty_or_comment() {
                return Err(parser.fail("all test statements need to be separated by an empty line"));
            }
            if token.kind.is_test_command() {
                // Tag selection would happen here; the default configuration selects everything.
                state.test_expr_executed = true;
            }
            let Some((token, conditions)) = self.apply_skip_conditions(&mut parser, token)? else {
                continue;
            };
            if state.skip_level > 0 && token.kind != TokenKind::Mode {
                continue;
            }
            match self.handle_record(&mut parser, &mut state, &token, conditions)? {
                Flow::Next => {}
                Flow::Halt => break,
                Flow::Stop => return Ok(()),
            }
        }
        if self.in_loop() {
            return Err(parser.fail("Missing endloop!"));
        }
        Ok(())
    }

    /// Consumes `skipif`/`onlyif` lines. Returns `None` if the record is skipped for this
    /// system, otherwise the record token and its loop-variable conditions.
    fn apply_skip_conditions(
        &self,
        parser: &mut Parser,
        mut token: Token,
    ) -> TestResult<Option<(Token, Vec<parser::Condition>)>> {
        let mut conditions = Vec::new();
        while matches!(token.kind, TokenKind::SkipIf | TokenKind::OnlyIf) {
            let skip_if = token.kind == TokenKind::SkipIf;
            let Some(system) = token.params.first() else {
                return Err(parser.fail("skipif/onlyif requires a single parameter (e.g. skipif duckdb)"));
            };
            let system_name = system.to_ascii_lowercase();
            let is_system_comparison = self.original_sqlite_test
                || match parser::parse_conditions(parser, &system_name, skip_if)? {
                    Some(parsed) => {
                        conditions.extend(parsed);
                        false
                    }
                    None => true,
                };
            if is_system_comparison {
                let our_system =
                    system_name == "duckdb" || (self.original_sqlite_test && system_name == "postgresql");
                if our_system == skip_if {
                    return Ok(None);
                }
            }
            parser.next_line();
            token = parser.tokenize()?;
        }
        Ok(Some((token, conditions)))
    }

    fn handle_record(
        &mut self,
        parser: &mut Parser,
        state: &mut FileState,
        token: &Token,
        conditions: Vec<parser::Condition>,
    ) -> TestResult<Flow> {
        let params = &token.params;
        match token.kind {
            TokenKind::Statement => self.handle_statement(parser, state, params, conditions)?,
            TokenKind::Query => self.handle_query(parser, params, conditions)?,
            TokenKind::HashThreshold => {
                if params.len() != 1 {
                    return Err(parser.fail("hash-threshold requires a parameter"));
                }
                self.hash_threshold = text::parse_leading_int(&params[0])
                    .ok_or_else(|| parser.fail("hash-threshold must be a number"))?;
            }
            TokenKind::Halt => return Ok(Flow::Halt),
            TokenKind::Mode => {
                if params.len() != 1 {
                    return Err(parser.fail("mode requires one parameter"));
                }
                match params[0].as_str() {
                    "skip" => state.skip_level = state.skip_level.wrapping_add(1),
                    "unskip" => state.skip_level = state.skip_level.wrapping_sub(1),
                    other => self.execute_command(Command::bare(CommandKind::Mode(other.to_owned())))?,
                }
            }
            TokenKind::Set => self.handle_set(parser, params)?,
            TokenKind::Reset => {
                if params.len() != 2 {
                    return Err(parser.fail("Expected reset [type] [name] (e.g reset label my_label)"));
                }
                if !params[0].eq_ignore_ascii_case("label") {
                    return Err(parser.fail(&format!("unrecognized reset parameter: {}", params[0])));
                }
                self.execute_command(Command::bare(CommandKind::ResetLabel(params[1].clone())))?;
            }
            TokenKind::Loop | TokenKind::ConcurrentLoop => {
                if params.len() != 3 {
                    return Err(
                        parser.fail("Expected loop [iterator_name] [start] [end] (e.g. loop i 1 300)")
                    );
                }
                let bound = |text: &str| {
                    text::parse_leading_int(text)
                        .filter(|value| i32::try_from(*value).is_ok())
                        .ok_or_else(|| parser.fail("loop_start and loop_end must be a number"))
                };
                let start = bound(&params[1])?;
                let end = bound(&params[2])?;
                self.start_loop(LoopDefinition {
                    iterator_name: params[0].clone(),
                    index: start,
                    start,
                    end,
                    parallel: token.kind == TokenKind::ConcurrentLoop,
                    tokens: Vec::new(),
                    skipped: false,
                });
            }
            TokenKind::Foreach | TokenKind::ConcurrentForeach => {
                if params.len() < 2 {
                    return Err(parser.fail(
                        "expected foreach [iterator_name] [m1] [m2] [etc...] (e.g. foreach type integer smallint \
                         float)",
                    ));
                }
                let mut tokens = Vec::new();
                for parameter in &params[1..] {
                    if !parser::expand_foreach_token(parameter, &mut tokens) {
                        tokens.push(parameter.clone());
                    }
                }
                self.start_loop(LoopDefinition {
                    iterator_name: params[0].clone(),
                    index: 0,
                    start: 0,
                    end: tokens.len() as i64,
                    parallel: token.kind == TokenKind::ConcurrentForeach,
                    tokens,
                    skipped: false,
                });
            }
            TokenKind::EndLoop => self.end_loop()?,
            TokenKind::Require => return self.handle_require(parser, params),
            TokenKind::TestEnv => self.handle_test_env(parser, state, params)?,
            TokenKind::RequireEnv => return self.handle_require_env(parser, state, params),
            TokenKind::Load => self.handle_load(parser, params)?,
            TokenKind::Restart => {
                let load_extensions = !(params.len() == 1 && params[0] == "no_extension_load");
                self.execute_command(Command::bare(CommandKind::Restart { load_extensions }))?;
            }
            TokenKind::Reconnect => self.execute_command(Command::bare(CommandKind::Reconnect))?,
            TokenKind::Sleep => {
                if params.len() != 2 {
                    return Err(parser.fail("sleep requires two parameter (e.g. sleep 1 second)"));
                }
                let duration = parse_sleep(&params[0], &params[1]).map_err(Failure::new)?;
                self.execute_command(Command::bare(CommandKind::Sleep(duration)))?;
            }
            TokenKind::Unzip => self.handle_unzip(parser, params)?,
            TokenKind::Tags => {
                if state.test_expr_executed {
                    return Err(parser.fail("tags expression must precede test commands"));
                }
                if state.tags_seen {
                    return Err(parser.fail("tags may be only specified once"));
                }
                state.tags_seen = true;
                if params.is_empty() {
                    return Err(parser.fail("tags requires >= 1 argument, e.g.: <tag1> [tag2 .. tagN]"));
                }
                state.tags.splice(0..0, params.iter().cloned());
            }
            TokenKind::Continue => {
                if !self.in_loop() {
                    return Err(parser.fail("continue cannot be called outside of a loop"));
                }
                let mut command = Command::bare(CommandKind::Continue);
                command.conditions = conditions;
                self.execute_command(command)?;
            }
            TokenKind::Invalid | TokenKind::SkipIf | TokenKind::OnlyIf => {}
        }
        Ok(Flow::Next)
    }

    fn handle_statement(
        &mut self,
        parser: &mut Parser,
        state: &mut FileState,
        params: &[String],
        conditions: Vec<parser::Condition>,
    ) -> TestResult {
        let Some(kind) = params.first() else {
            return Err(parser.fail("statement requires at least one parameter (statement ok/error)"));
        };
        let original_output_result_mode = self.modes.result;
        let expected = match kind.as_str() {
            "ok" => ExpectedResult::Success,
            "error" => ExpectedResult::Error,
            "maybe" => ExpectedResult::Unknown,
            "debug" => {
                self.modes.result = true;
                ExpectedResult::DontCare
            }
            "debug_skip" => {
                self.modes.result = true;
                state.skip_level = state.skip_level.wrapping_add(1);
                ExpectedResult::DontCare
            }
            _ => return Err(parser.fail("statement argument should be 'ok' or 'error")),
        };
        let line = parser.current_line() + 1;
        parser.next_line();
        let statement_text = parser.extract_statement();
        if statement_text.is_empty() {
            return Err(parser.fail("Unexpected empty statement text"));
        }
        let expected_error = parser.extract_expected_error(expected, self.original_sqlite_test)?;
        let command = Command {
            line: Some(line),
            connection: params.get(1).cloned(),
            sql: self.replace_keywords(&statement_text),
            conditions,
            kind: CommandKind::Statement(StatementCommand {
                expected,
                expected_error,
            }),
        };
        self.execute_command(command)?;
        self.modes.result = original_output_result_mode;
        Ok(())
    }

    fn handle_query(
        &mut self,
        parser: &mut Parser,
        params: &[String],
        conditions: Vec<parser::Condition>,
    ) -> TestResult {
        let Some(column_text) = params.first() else {
            return Err(parser.fail("query requires at least one parameter (query III)"));
        };
        if let Some(bad) = column_text.chars().find(|c| !matches!(c, 'T' | 'I' | 'R')) {
            return Err(parser.fail(&format!(
                "unknown type character '{bad}' in string, expected T, I or R only"
            )));
        }
        let line = parser.current_line() + 1;
        parser.next_line();
        let statement_text = parser.extract_statement();
        let sql = self.replace_keywords(&statement_text);
        let values = parser.extract_expected_result();
        let (sort_style, connection) = match params.get(1) {
            Some(param) => match SortStyle::parse(param) {
                Some(style) => (style, None),
                None => (SortStyle::NoSort, Some(param.clone())),
            },
            None => (SortStyle::NoSort, None),
        };
        let command = Command {
            line: Some(line),
            connection,
            sql,
            conditions,
            kind: CommandKind::Query(QueryCommand {
                expected_column_count: column_text.len(),
                sort_style,
                values,
                label: params.get(2).cloned(),
            }),
        };
        self.execute_command(command)
    }

    fn handle_set(&mut self, parser: &Parser, params: &[String]) -> TestResult {
        let Some(name) = params.first() else {
            return Err(
                parser.fail("set requires at least 1 parameter (e.g. set ignore_error_messages HTTP Error)")
            );
        };
        match name.as_str() {
            "ignore_error_messages" => self.ignore_error_messages = parse_message_list(&params[1..]),
            "always_fail_error_messages" => {
                self.always_fail_error_messages = parse_message_list(&params[1..]);
            }
            "seed" => {
                if params.len() != 2 {
                    return Err(parser.fail("set seed requires a single seed value"));
                }
                let seed: f64 = params[1]
                    .trim_matches(|c: char| c.is_ascii() && text::is_space(c as u8))
                    .parse()
                    .map_err(|_| parser.fail("set seed requires a floating point parameter"))?;
                let seed = rustdb::Value::double(seed)
                    .to_varchar()
                    .map_err(|error| Failure::new(error.to_string()))?;
                let engine = self.engine;
                let result = engine.query(self.main_connection_mut()?, &format!("SELECT SETSEED({seed})"));
                if let Some(error) = crate::backend::QueryResult::error(&result) {
                    return Err(parser.fail(&format!("Failed to set seed: {error}")));
                }
                self.skip_reload = true;
            }
            other => return Err(parser.fail(&format!("unrecognized set parameter: {other}"))),
        }
        Ok(())
    }

    fn handle_require(&mut self, parser: &Parser, params: &[String]) -> TestResult<Flow> {
        if self.check_require(parser, params)? == require::RequireResult::Present {
            return Ok(Flow::Next);
        }
        let param = &params[0];
        if self.environment.is_required(param) {
            return Err(parser.fail(&format!("require {param}: FAILED")));
        }
        self.skip_test(format!("require {param}"));
        Ok(Flow::Stop)
    }

    fn handle_test_env(&mut self, parser: &Parser, state: &mut FileState, params: &[String]) -> TestResult {
        if self.in_loop() {
            return Err(parser.fail("test-env cannot be called in a loop"));
        }
        if params.len() != 2 {
            return Err(parser.fail("test-env requires 2 arguments: <env name> <default env val>"));
        }
        let name = &params[0];
        let value = self
            .environment
            .test_variables()
            .remove(name)
            .unwrap_or_else(|| params[1].clone());
        add_env_tags(&mut state.tags, name, Some(&value));
        self.environment_variables.insert(name.clone(), value);
        Ok(())
    }

    fn handle_require_env(
        &mut self,
        parser: &Parser,
        state: &mut FileState,
        params: &[String],
    ) -> TestResult<Flow> {
        if self.in_loop() {
            return Err(parser.fail("require-env cannot be called in a loop"));
        }
        if params.len() != 1 && params.len() != 2 {
            return Err(
                parser.fail("require-env requires 1 argument: <env name> [optional: <expected env val>]")
            );
        }
        let name = &params[0];
        let mut actual = std::env::var(name).ok();
        if actual.is_none()
            && name == "LOCAL_EXTENSION_REPO"
            && self
                .engine
                .setting(&self.config, RequireSetting::AutoloadKnownExtensions)
                != 0
        {
            actual = Some(format!("{}/repository", self.engine.build_directory()));
        }
        let Some(actual) = actual else {
            self.skip_test(format!("require-env {name}"));
            return Ok(Flow::Stop);
        };
        if let Some(expected) = params.get(1) {
            if &actual != expected {
                self.skip_test(format!("require-env {name} {expected}"));
                return Ok(Flow::Stop);
            }
            state.tags.push(format!("env[{name}]={expected}"));
        }
        if self.environment_variables.contains_key(name) {
            return Err(parser.fail(&format!("Environment variable '{name}' has already been defined")));
        }
        self.environment_variables.insert(name.clone(), actual);
        add_env_tags(&mut state.tags, name, params.get(1).map(String::as_str));
        Ok(Flow::Next)
    }

    fn handle_load(&mut self, parser: &Parser, params: &[String]) -> TestResult {
        let read_only = match params.get(1) {
            None => false,
            Some(mode) if mode.eq_ignore_ascii_case("readonly") => true,
            Some(mode) if mode.eq_ignore_ascii_case("readwrite") => false,
            Some(mode) => {
                return Err(parser.fail(&format!(
                    "parameter to 'load' is invalid, received '{mode}', accepted options are 'readonly' and \
                     'readwrite"
                )));
            }
        };
        let path = params
            .first()
            .map(|path| self.replace_keywords(path))
            .unwrap_or_default();
        self.execute_command(Command::bare(CommandKind::Load(LoadCommand {
            path,
            read_only,
            version: params.get(2).cloned(),
        })))
    }

    fn handle_unzip(&mut self, parser: &Parser, params: &[String]) -> TestResult {
        if params.len() != 1 && params.len() != 2 {
            return Err(parser.fail(
                "unzip requires 1 argument: <path/to/file.db.gz> [optional: <path/to/unzipped_file.db>, default: \
                 __TEST_DIR__/<file.db>]",
            ));
        }
        let input = self.replace_keywords(&params[0]);
        // DuckDB checks the literal, case-sensitive ".gz" suffix.
        #[allow(clippy::case_sensitive_file_extension_comparisons, reason = "matches DuckDB")]
        if !input.ends_with(".gz") {
            return Err(parser.fail("unzip: input has not a GZIP extension"));
        }
        let file_start = input.rfind('/').map_or(0, |slash| slash + 1);
        let file_name = input.get(file_start..input.len() - 3).unwrap_or_default();
        let default_output = self.replace_keywords(&format!("__TEST_DIR__/{file_name}"));
        let output = match params.get(1).map(|path| self.replace_keywords(path)) {
            Some(path) if path != "NULL" => path,
            _ => default_output,
        };
        self.execute_command(Command::bare(CommandKind::Unzip { input, output }))
    }

    /// `SQLLogicTestRunner::LoadExtension` in the default (non-autoloading) test mode.
    fn load_extension(&mut self, name: &str) -> Result<ExtensionLoad, String> {
        let database = self.db.as_ref().ok_or("no open database")?;
        let loaded = self.engine.load_extension(database, name)?;
        if loaded != ExtensionLoad::NotLoaded {
            return Ok(loaded);
        }
        // The linked helper excludes externally supplied modules. Recognize an
        // actual preload, or load an explicitly configured trusted test module
        // at this require directive, preserving initially unloaded test states.
        // LOAD may initialize connection-local crypto state. Use the test's
        // main connection, as an explicit LOAD statement in the file would.
        let engine = self.engine;
        let connection = self.con.as_mut().ok_or("no open connection")?;
        let quoted_name = name.replace('\'', "''");
        let query =
            format!("SELECT 1 FROM duckdb_extensions() WHERE extension_name='{quoted_name}' AND loaded");
        let is_loaded = |connection: &mut E::Connection| -> Result<bool, String> {
            let result = engine.query(connection, &query);
            if let Some(error) = crate::backend::QueryResult::error(&result) {
                return Err(error.to_owned());
            }
            Ok(crate::backend::QueryResult::row_count(&result) > 0)
        };
        if is_loaded(connection)? {
            return Ok(ExtensionLoad::Loaded);
        }
        let Ok(paths) = std::env::var("SLT_REQUIRE_EXTENSION_PATHS") else {
            return Ok(ExtensionLoad::NotLoaded);
        };
        let paths: BTreeMap<String, String> = serde_json::from_str(&paths)
            .map_err(|error| format!("Invalid SLT_REQUIRE_EXTENSION_PATHS: {error}"))?;
        let Some(path) = paths.get(name) else {
            return Ok(ExtensionLoad::NotLoaded);
        };
        let result = engine.query(connection, &format!("LOAD '{}'", path.replace('\'', "''")));
        if let Some(error) = crate::backend::QueryResult::error(&result) {
            return Err(error.to_owned());
        }
        if !is_loaded(connection)? {
            return Err(format!(
                "Configured extension '{name}' did not register as loaded"
            ));
        }
        Ok(ExtensionLoad::Loaded)
    }
}

impl<E: Engine> std::fmt::Debug for Runner<'_, E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Runner")
            .field("engine", &self.engine.name())
            .field("file_name", &self.file_name)
            .field("dbpath", &self.dbpath)
            .finish_non_exhaustive()
    }
}

/// `StringReplaceLoopIterator`: replaces `${name}` and `{name}`.
fn replace_loop_iterator(text: &str, name: &str, replacement: &str) -> String {
    let iterator = format!("{{{name}}}");
    text.replace(&format!("${iterator}"), replacement)
        .replace(&iterator, replacement)
}

/// `add_env_tag`.
fn add_env_tags(tags: &mut Vec<String>, name: &str, value: Option<&str>) {
    tags.push(format!("env[{name}]"));
    if let Some(value) = value {
        tags.push(format!("env[{name}]={value}"));
    }
}

/// `set ignore_error_messages ...`: a comma-separated list whose items may contain spaces.
/// Replicates DuckDB's parser exactly, including its use of the comma *position* as a length.
fn parse_message_list(params: &[String]) -> Vec<String> {
    let mut messages: Vec<String> = Vec::new();
    let mut insert = |message: String| {
        if !message.is_empty() && !messages.contains(&message) {
            messages.push(message);
        }
    };
    if params.is_empty() {
        return messages;
    }
    let mut current: Vec<u8> = Vec::new();
    let mut token_index = 0;
    let mut offset = 0;
    while token_index < params.len() {
        let bytes = params[token_index].as_bytes();
        let comma = bytes[offset.min(bytes.len())..]
            .iter()
            .position(|&b| b == b',')
            .map(|position| position + offset);
        match comma {
            None => {
                current.extend_from_slice(&bytes[offset.min(bytes.len())..]);
                current.push(b' ');
                token_index += 1;
                offset = 0;
            }
            Some(comma) => {
                let end = (offset + comma).min(bytes.len());
                current.extend_from_slice(&bytes[offset..end]);
                insert(text::trim(&String::from_utf8_lossy(&current)));
                current.clear();
                offset = comma + 1;
            }
        }
    }
    insert(text::trim(&String::from_utf8_lossy(&current)));
    messages
}

/// `sleep <n> <unit>` (`std::stoull` + `SleepCommand::ParseUnit`).
fn parse_sleep(amount: &str, unit: &str) -> Result<Duration, String> {
    let amount = text::parse_leading_int(amount)
        .and_then(|value| u64::try_from(value).ok())
        .ok_or_else(|| format!("stoull: invalid sleep duration '{amount}'"))?;
    Ok(match unit {
        "second" | "seconds" | "sec" => Duration::from_secs(amount),
        "millisecond" | "milliseconds" | "milli" => Duration::from_millis(amount),
        "microsecond" | "microseconds" | "micro" => Duration::from_micros(amount),
        "nanosecond" | "nanoseconds" | "nano" => Duration::from_nanos(amount),
        _ => {
            return Err(
                "Unrecognized sleep mode - expected second/millisecond/microescond/nanosecond".to_owned(),
            );
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| (*v).to_owned()).collect()
    }

    #[test]
    fn message_lists_follow_duckdb_parsing() {
        assert_eq!(parse_message_list(&strings(&["HTTP", "Error"])), ["HTTP Error"]);
        assert_eq!(parse_message_list(&strings(&["a,", "b"])), ["a", "b"]);
        assert_eq!(
            parse_message_list(&strings(&["one", "two,three"])),
            ["one two", "three"]
        );
        assert_eq!(parse_message_list(&[]), Vec::<String>::new());
    }

    #[test]
    fn loop_iterators_replace_both_syntaxes() {
        assert_eq!(
            replace_loop_iterator("SELECT ${i}, {i}, {j}", "i", "7"),
            "SELECT 7, 7, {j}"
        );
    }

    #[test]
    fn sleep_units() {
        assert_eq!(parse_sleep("2", "seconds"), Ok(Duration::from_secs(2)));
        assert_eq!(parse_sleep("5", "milli"), Ok(Duration::from_millis(5)));
        assert!(parse_sleep("1", "fortnight").is_err());
        assert!(parse_sleep("x", "sec").is_err());
    }
}
