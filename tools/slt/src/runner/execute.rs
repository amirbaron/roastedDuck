//! Command execution (port of `Command::Execute` and the `ExecuteInternal` implementations in
//! `test/sqlite/sqllogic_command.cpp`).
//!
//! Sequential execution takes `&mut Runner`; the threads of a concurrent loop share `&Runner`
//! and each own a connection, so only commands that support concurrency run there.

use std::collections::hash_map::RandomState;
use std::fs;
use std::hash::BuildHasher;
use std::io::{self, Read, Write};
use std::thread;

use flate2::read::MultiGzDecoder;

use super::Runner;
use crate::backend::Engine;
use crate::commands::{Command, CommandKind, ExecuteContext, LoadCommand, LoopCommand, LoopDefinition};
use crate::config::TestEnvironment;
use crate::outcome::{Failure, TestResult};
use crate::parser::{Comparison, Condition};

impl<E: Engine> Runner<'_, E> {
    /// `Command::Execute` for the main thread.
    pub(super) fn execute(&mut self, command: &Command, ctx: &mut ExecuteContext) -> TestResult {
        if self.finished() || !check_loop_condition(ctx, &command.conditions)? {
            return Ok(());
        }
        ctx.sql = self.loop_replacement(&command.sql, &ctx.running_loops)?;
        match &command.kind {
            CommandKind::Statement(statement) => {
                let result = self.run_sql(command, ctx)?;
                self.check_statement_result(statement, ctx, &result)
                    .map_err(|message| Failure::new(message).or_at(command.line))
            }
            CommandKind::Query(query) => {
                let result = self.run_sql(command, ctx)?;
                self.check_query_result(query, ctx, result)
                    .map_err(|message| Failure::new(message).or_at(command.line))
            }
            CommandKind::ResetLabel(label) => self.reset_label(label),
            CommandKind::Restart { load_extensions } => self.restart(ctx, *load_extensions),
            CommandKind::Reconnect => {
                if ctx.parallel {
                    return Err(Failure::new("Cannot reconnect in parallel"));
                }
                self.reconnect()
            }
            CommandKind::Loop(definition) if definition.definition.parallel => {
                self.execute_concurrent_loop(definition, ctx)
            }
            CommandKind::Loop(definition) => self.execute_loop(definition, ctx),
            CommandKind::Continue => {
                skip_rest_of_iteration(ctx);
                Ok(())
            }
            CommandKind::Mode(mode) => self.set_mode(mode),
            CommandKind::Sleep(duration) => {
                thread::sleep(*duration);
                Ok(())
            }
            CommandKind::Unzip { input, output } => unzip(input, output).map_err(|error| {
                Failure::new(format!(
                    "Catalog Error: Cannot unzip \"{input}\" to \"{output}\": {error}"
                ))
            }),
            CommandKind::Load(load) => self.load(load, ctx),
        }
    }

    /// Runs the command's SQL on its connection (`CommandConnection` + `ExecuteQuery`).
    fn run_sql(&mut self, command: &Command, ctx: &ExecuteContext) -> TestResult<E::Result> {
        let engine = self.engine;
        let connection = match &command.connection {
            None => self.main_connection_mut()?,
            Some(name) => {
                if !self.named_connections.contains_key(name) {
                    let connection = engine.connect(self.database()?).map_err(Failure::new)?;
                    self.named_connections.insert(name.clone(), connection);
                }
                self.named_connections
                    .get_mut(name)
                    .ok_or_else(|| Failure::new("named connection disappeared"))?
            }
        };
        Ok(engine.query(connection, &ctx.sql))
    }

    /// Non-concurrent `LoopCommand::ExecuteInternal`.
    fn execute_loop(&mut self, command: &LoopCommand, ctx: &mut ExecuteContext) -> TestResult {
        let mut definition = command.definition.clone();
        definition.index = definition.start;
        loop {
            if self.finished() {
                break;
            }
            let depth = ctx.running_loops.len();
            ctx.running_loops.push(definition.clone());
            for statement in &command.commands {
                self.execute(statement, ctx)?;
                if ctx.running_loops[depth].skipped {
                    break;
                }
            }
            ctx.running_loops.pop();
            definition.index += 1;
            if definition.index >= definition.end {
                break;
            }
        }
        Ok(())
    }

    /// Concurrent `LoopCommand::ExecuteInternal`: one thread and connection per iteration, in a
    /// random order; the first failing iteration (in launch order) fails the test.
    fn execute_concurrent_loop(&self, command: &LoopCommand, ctx: &ExecuteContext) -> TestResult {
        if ctx.running_loops.iter().any(|running| running.parallel) {
            return Err(Failure::new("Nested parallel loop commands not allowed"));
        }
        if !command.commands.iter().all(Command::supports_concurrent) {
            return Err(Failure::new("Concurrent loop is not supported over this command"));
        }
        let definition = &command.definition;
        let mut indexes = vec![definition.start];
        indexes.extend((definition.start + 1)..definition.end);
        shuffle(&mut indexes);

        let database = self.database()?;
        let outcomes: Vec<TestResult> = thread::scope(|scope| {
            let handles: Vec<_> = indexes
                .iter()
                .map(|&index| {
                    let mut running_loops = ctx.running_loops.clone();
                    running_loops.push(LoopDefinition {
                        index,
                        ..definition.clone()
                    });
                    scope.spawn(move || {
                        self.run_concurrent_iteration(database, &command.commands, running_loops)
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| {
                    handle
                        .join()
                        .unwrap_or_else(|_| Err(Failure::new("concurrent loop thread panicked")))
                })
                .collect()
        });
        outcomes.into_iter().collect()
    }

    /// `ParallelExecuteLoop`: runs the loop body once on a fresh connection.
    fn run_concurrent_iteration(
        &self,
        database: &E::Database,
        commands: &[Command],
        running_loops: Vec<LoopDefinition>,
    ) -> TestResult {
        let mut connection = self.engine.connect(database).map_err(Failure::new)?;
        let mut ctx = ExecuteContext {
            running_loops,
            parallel: true,
            ..ExecuteContext::default()
        };
        for command in commands {
            self.execute_in_thread(command, &mut ctx, &mut connection)
                .map_err(|failure| {
                    let line = failure.line.or(command.line);
                    let location = line.map_or_else(String::new, |line| format!(":{line}"));
                    Failure {
                        line,
                        message: format!("Failure at {}{location}: {}", self.file_name, failure.message),
                    }
                })?;
        }
        match ctx.error_line {
            Some(line) => Err(Failure {
                line,
                message: format!("{}: concurrent loop iteration failed", self.file_name),
            }),
            None => Ok(()),
        }
    }

    /// `Command::Execute` inside a concurrent loop thread.
    fn execute_in_thread(
        &self,
        command: &Command,
        ctx: &mut ExecuteContext,
        connection: &mut E::Connection,
    ) -> TestResult {
        if self.finished() || !check_loop_condition(ctx, &command.conditions)? {
            return Ok(());
        }
        ctx.sql = self.loop_replacement(&command.sql, &ctx.running_loops)?;
        if command.connection.is_some()
            && matches!(command.kind, CommandKind::Statement(_) | CommandKind::Query(_))
        {
            return Err(Failure::new("Named connections not supported in parallel loop"));
        }
        let check = match &command.kind {
            CommandKind::Statement(statement) => {
                let result = self.engine.query(connection, &ctx.sql);
                self.check_statement_result(statement, ctx, &result)
            }
            CommandKind::Query(query) => {
                let result = self.engine.query(connection, &ctx.sql);
                self.check_query_result(query, ctx, result)
            }
            CommandKind::ResetLabel(label) => return self.reset_label(label),
            CommandKind::Sleep(duration) => {
                thread::sleep(*duration);
                return Ok(());
            }
            CommandKind::Loop(definition) if definition.definition.parallel => {
                return Err(Failure::new("Nested parallel loop commands not allowed"));
            }
            CommandKind::Loop(definition) => return self.execute_loop_in_thread(definition, ctx, connection),
            CommandKind::Restart { .. } => return Err(Failure::new("Cannot restart database in parallel")),
            CommandKind::Reconnect => return Err(Failure::new("Cannot reconnect in parallel")),
            CommandKind::Continue
            | CommandKind::Mode(_)
            | CommandKind::Unzip { .. }
            | CommandKind::Load(_) => {
                return Err(Failure::new("Concurrent loop is not supported over this command"));
            }
        };
        if let Err(message) = check {
            eprintln!(
                "{}:{}: {message}",
                self.file_name,
                command.line.unwrap_or_default()
            );
            self.finish_processing_file();
            ctx.error_line = Some(command.line);
        }
        Ok(())
    }

    /// A sequential loop nested in a concurrent loop thread.
    fn execute_loop_in_thread(
        &self,
        command: &LoopCommand,
        ctx: &mut ExecuteContext,
        connection: &mut E::Connection,
    ) -> TestResult {
        let mut definition = command.definition.clone();
        definition.index = definition.start;
        loop {
            if self.finished() {
                break;
            }
            let depth = ctx.running_loops.len();
            ctx.running_loops.push(definition.clone());
            for statement in &command.commands {
                self.execute_in_thread(statement, ctx, connection)?;
                if ctx.running_loops[depth].skipped {
                    break;
                }
            }
            ctx.running_loops.pop();
            definition.index += 1;
            if definition.index >= definition.end {
                break;
            }
        }
        Ok(())
    }

    /// `ResetLabel::ExecuteInternal`: forgets a label's hash; resetting an unknown label fails.
    fn reset_label(&self, label: &str) -> TestResult {
        let mut labels = self
            .hash_labels
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if labels.remove(label).is_none() {
            return Err(Failure::new(format!("reset label {label}: label not found")));
        }
        Ok(())
    }

    /// `RestartCommand::ExecuteInternal`.
    fn restart(&mut self, ctx: &ExecuteContext, load_extensions: bool) -> TestResult {
        if ctx.parallel {
            return Err(Failure::new("Cannot restart database in parallel"));
        }
        let engine = self.engine;
        let session = engine.save_session(
            self.con
                .as_ref()
                .ok_or_else(|| Failure::new("no open connection"))?,
            &mut self.config,
        );
        self.load_database(&self.dbpath.clone(), load_extensions)?;
        engine
            .restore_session(self.main_connection_mut()?, session)
            .map_err(Failure::new)
    }

    /// `ModeCommand::ExecuteInternal`.
    fn set_mode(&mut self, mode: &str) -> TestResult {
        match mode {
            "output_hash" => self.modes.hash = true,
            "output_result" => self.modes.result = true,
            "no_output" => {
                self.modes.hash = false;
                self.modes.result = false;
            }
            "debug" => self.modes.debug = true,
            _ => return Err(Failure::new(format!("unrecognized mode: {mode}"))),
        }
        Ok(())
    }

    /// `LoadCommand::ExecuteInternal`.
    fn load(&mut self, load: &LoadCommand, ctx: &ExecuteContext) -> TestResult {
        let resolved_path = self.loop_replacement(&load.path, &ctx.running_loops)?;
        if !load.read_only {
            TestEnvironment::delete_database(&resolved_path);
        }
        self.dbpath.clone_from(&resolved_path);
        self.engine.set_read_only(&mut self.config, load.read_only);
        if let Some(database) = &self.db {
            match &load.version {
                None => self.engine.inherit_storage_version(&mut self.config, database),
                Some(version) => self
                    .engine
                    .set_storage_version(&mut self.config, version)
                    .map_err(|error| {
                        Failure::new(format!("Failed to load database {}: {error}", load.path))
                    })?,
            }
        }
        self.load_database(&resolved_path, true)
    }
}

/// `continue`: marks the innermost running loop iteration as finished.
fn skip_rest_of_iteration(ctx: &mut ExecuteContext) {
    if let Some(innermost) = ctx.running_loops.last_mut() {
        innermost.skipped = true;
    }
}

/// `CheckLoopCondition`: whether the loop-variable conditions allow the command to run.
fn check_loop_condition(ctx: &ExecuteContext, conditions: &[Condition]) -> TestResult<bool> {
    if conditions.is_empty() {
        return Ok(true);
    }
    if ctx.running_loops.is_empty() {
        return Err(Failure::new(
            "Binder Error: Conditions (onlyif/skipif) on loop parameters can only occur within a loop",
        ));
    }
    for condition in conditions {
        let mut holds = false;
        let mut found = false;
        for running in ctx
            .running_loops
            .iter()
            .filter(|running| running.iterator_name == condition.keyword)
        {
            found = true;
            let value = running
                .current_value()
                .ok_or_else(|| Failure::new("loop index out of range"))?;
            holds = match condition.comparison {
                Comparison::Equal => value == condition.value,
                Comparison::NotEqual => value != condition.value,
                ordering => {
                    let parse = |text: &str| {
                        crate::text::parse_leading_int(text).ok_or_else(|| {
                            Failure::new(format!("stoll: invalid loop condition value '{text}'"))
                        })
                    };
                    let (left, right) = (parse(&value)?, parse(&condition.value)?);
                    match ordering {
                        Comparison::GreaterThan => left > right,
                        Comparison::GreaterThanOrEqual => left >= right,
                        Comparison::LessThan => left < right,
                        _ => left <= right,
                    }
                }
            };
        }
        if !found {
            return Err(Failure::new(format!(
                "Binder Error: Condition in onlyif/skipif not found: {} must be a loop iterator name",
                condition.keyword
            )));
        }
        if holds == condition.skip_if {
            return Ok(false);
        }
    }
    Ok(true)
}

/// `UnzipCommand::ExecuteInternal`: gunzips `input` into `output` (created if missing, written
/// from the start without truncation, like DuckDB's file system).
fn unzip(input: &str, output: &str) -> io::Result<()> {
    let mut decoder = MultiGzDecoder::new(fs::File::open(input)?);
    let mut file = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(output)?;
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let read = decoder.read(&mut buffer)?;
        if read == 0 {
            return Ok(());
        }
        file.write_all(&buffer[..read])?;
    }
}

/// Fisher–Yates shuffle seeded from the process's random hasher state.
fn shuffle<T>(items: &mut [T]) {
    let state = RandomState::new();
    for i in (1..items.len()).rev() {
        let j = (state.hash_one(i) % (i as u64 + 1)) as usize;
        items.swap(i, j);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loop_at(name: &str, index: i64, tokens: &[&str]) -> LoopDefinition {
        LoopDefinition {
            iterator_name: name.to_owned(),
            index,
            start: 0,
            end: 10,
            parallel: false,
            tokens: tokens.iter().map(|t| (*t).to_owned()).collect(),
            skipped: false,
        }
    }

    fn condition(keyword: &str, comparison: Comparison, value: &str, skip_if: bool) -> Condition {
        Condition {
            keyword: keyword.to_owned(),
            value: value.to_owned(),
            comparison,
            skip_if,
        }
    }

    #[test]
    fn loop_conditions() {
        let ctx = ExecuteContext {
            running_loops: vec![loop_at("i", 3, &[]), loop_at("type", 1, &["int", "varchar"])],
            ..ExecuteContext::default()
        };
        let run = |conditions: &[Condition]| check_loop_condition(&ctx, conditions).unwrap();
        assert!(!run(&[condition("i", Comparison::GreaterThan, "2", true)]));
        assert!(run(&[condition("i", Comparison::LessThanOrEqual, "2", true)]));
        assert!(run(&[condition("type", Comparison::Equal, "varchar", false)]));
        assert!(!run(&[condition("type", Comparison::NotEqual, "varchar", false)]));
        assert!(check_loop_condition(&ctx, &[condition("k", Comparison::Equal, "1", true)]).is_err());
        assert!(
            check_loop_condition(
                &ExecuteContext::default(),
                &[condition("i", Comparison::Equal, "1", true)]
            )
            .is_err()
        );
    }

    #[test]
    fn shuffle_keeps_elements() {
        let mut items: Vec<i64> = (0..50).collect();
        shuffle(&mut items);
        items.sort_unstable();
        assert_eq!(items, (0..50).collect::<Vec<_>>());
    }
}
