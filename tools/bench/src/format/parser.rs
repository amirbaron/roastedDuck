//! Port of `InterpretedBenchmark::ProcessFile` and `LoadBenchmark`.
//!
//! Quirks of the C++ parser are kept on purpose so that a benchmark means the same thing
//! to both engines: commands and their arguments (including file paths) are lowercased,
//! `${KEY}` replacements are applied to each line when it is read, multi-line SQL
//! sections are joined with spaces, and a `template` directive ends processing of the
//! current file.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::Path;

use super::csv::parse_answer_file;
use super::{BENCHMARK_DIRECTORY, Benchmark, Cache, ExpectedResult, Queries, ResultMode, Storage};

/// `STANDARD_VECTOR_SIZE`: the default Arrow batch size of `resultmode arrow`.
const DEFAULT_ARROW_BATCH_SIZE: u64 = 2048;
/// Guards against `include` cycles.
const MAX_INCLUDE_DEPTH: usize = 32;

/// Inputs that are not part of the benchmark file itself.
#[derive(Clone, Copy, Debug)]
pub struct ParseOptions<'a> {
    /// The benchmark root: relative paths (benchmark files, templates, includes, SQL and
    /// answer files) resolve against it, like the runner's working directory.
    pub root: &'a Path,
    /// Overrides for `argument` directives (the runner's `--<name> <value>`).
    pub arguments: &'a BTreeMap<String, String>,
}

/// A malformed benchmark file, with its location when known.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    message: String,
}

impl ParseError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ParseError {}

/// Parses the benchmark at `name` (a path relative to [`ParseOptions::root`], e.g.
/// `benchmark/tpch/sf1/q01.benchmark`).
pub fn parse_benchmark(name: &str, options: &ParseOptions<'_>) -> Result<Benchmark, ParseError> {
    let mut parser = Parser::new(options);
    parser.process_file(name, 0)?;
    parser.finish(name)
}

/// The SQL sections that `load`/`run`/... commands fill.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Section {
    Load,
    Reload,
    Init,
    Run,
    Cleanup,
}

impl Section {
    fn from_command(command: &str) -> Option<Self> {
        Some(match command {
            "load" => Self::Load,
            "reload" => Self::Reload,
            "init" => Self::Init,
            "run" => Self::Run,
            "cleanup" => Self::Cleanup,
            _ => return None,
        })
    }
}

/// Parser state; mirrors the members of `InterpretedBenchmark`.
struct Parser<'a> {
    options: &'a ParseOptions<'a>,
    /// `replacement_mapping`, in insertion order.
    replacements: Vec<(String, String)>,
    handled_arguments: BTreeSet<String>,
    sections: BTreeMap<Section, String>,
    cache: Cache,
    extensions: Vec<String>,
    load_extensions: Vec<String>,
    asserts: Vec<ExpectedResult>,
    results: Vec<ExpectedResult>,
    retry_load: u32,
    display_name: Option<String>,
    display_group: Option<String>,
    subgroup: Option<String>,
    description: Option<String>,
    storage: Storage,
    result_mode: ResultMode,
    require_reinit: bool,
}

impl<'a> Parser<'a> {
    fn new(options: &'a ParseOptions<'a>) -> Self {
        Self {
            options,
            replacements: vec![("BENCHMARK_DIR".to_owned(), BENCHMARK_DIRECTORY.to_owned())],
            handled_arguments: BTreeSet::new(),
            sections: BTreeMap::new(),
            cache: Cache::default(),
            extensions: Vec::new(),
            load_extensions: Vec::new(),
            asserts: Vec::new(),
            results: Vec::new(),
            retry_load: 0,
            display_name: None,
            display_group: None,
            subgroup: None,
            description: None,
            storage: Storage::default(),
            result_mode: ResultMode::default(),
            require_reinit: false,
        }
    }

    fn set_replacement(&mut self, key: &str, value: String) {
        match self.replacements.iter_mut().find(|(k, _)| k == key) {
            Some(entry) => entry.1 = value,
            None => self.replacements.push((key.to_owned(), value)),
        }
    }

    fn replacement(&self, key: &str) -> Option<&str> {
        self.replacements
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    fn read_text(&self, path: &str) -> Result<String, std::io::Error> {
        std::fs::read_to_string(self.options.root.join(path))
    }

    #[allow(clippy::too_many_lines)]
    fn process_file(&mut self, path: &str, depth: usize) -> Result<(), ParseError> {
        if depth > MAX_INCLUDE_DEPTH {
            return Err(ParseError::new(format!("{path}: includes nested too deeply")));
        }
        let text = self
            .read_text(path)
            .map_err(|e| ParseError::new(format!("could not read benchmark file {path}: {e}")))?;
        let mut reader = LineReader::new(path, &text);
        while let Some(line) = reader.read_line(&self.replacements) {
            if line.is_empty() {
                continue;
            }
            if line.starts_with('#') {
                if depth == 0 && self.description.is_none() {
                    self.description = header_field(&line, "description").map(str::to_owned);
                }
                continue;
            }
            let lowered = line.to_ascii_lowercase();
            let splits = split_char(&lowered, ' ');
            let command = splits[0];
            if let Some(section) = Section::from_command(command) {
                if self.sections.contains_key(&section) {
                    return Err(
                        reader.error(format!("Multiple calls to {command} in the same benchmark file"))
                    );
                }
                let mut query = String::new();
                while let Some(line) = reader.read_line(&self.replacements) {
                    if line.is_empty() {
                        break;
                    }
                    query.push_str(&line);
                    query.push(' ');
                }
                if let Some(file) = splits.get(1).filter(|file| !file.is_empty()) {
                    query = self.read_text(file).map_err(|e| {
                        reader.error(format!("Failed to read {command} from file {file}: {e}"))
                    })?;
                }
                let query = trim(&query);
                if query.is_empty() {
                    return Err(reader.error(format!("Encountered an empty {command} node!")));
                }
                self.sections.insert(section, query.to_owned());
                continue;
            }
            match command {
                "require" => {
                    let load_only = match splits.len() {
                        2 => false,
                        3 if splits[2] == "load_only" => true,
                        3 => {
                            return Err(reader.error("require only supports load_only as a second parameter"));
                        }
                        _ => return Err(reader.error("require requires a single parameter")),
                    };
                    let list = if load_only {
                        &mut self.load_extensions
                    } else {
                        &mut self.extensions
                    };
                    if !list.iter().any(|e| e == splits[1]) {
                        list.push(splits[1].to_owned());
                    }
                }
                "resultmode" => self.result_mode = parse_result_mode(&splits, &reader)?,
                "cache" => {
                    let file = match splits.as_slice() {
                        [_, file] => file,
                        [_, file, "no_connect"] => {
                            self.cache.no_connect = true;
                            file
                        }
                        _ => {
                            return Err(reader.error("cache requires a db file, and optionally a no_connect"));
                        }
                    };
                    let file = replace_all(file, &self.replacements);
                    // The name is already lowercased, as in DuckDB.
                    #[allow(clippy::case_sensitive_file_extension_comparisons)]
                    if file.ends_with(".csv") || file.ends_with(".parquet") || file.ends_with(".csv.gz") {
                        self.cache.file = Some(file);
                    } else {
                        self.cache.database = Some(file);
                    }
                }
                "cache_file" => match splits.as_slice() {
                    [_, file] => self.cache.file = Some(replace_all(file, &self.replacements)),
                    _ => return Err(reader.error("cache_file requires a single file")),
                },
                "storage" => {
                    self.storage.persistent = match splits.get(1) {
                        Some(&"transient") => false,
                        Some(&"persistent") => true,
                        Some(_) => return Err(reader.error("Invalid argument for storage")),
                        None => return Err(reader.error("storage requires at least one parameter")),
                    };
                    if splits.len() == 3 {
                        self.storage.version = Some(splits[2].to_owned());
                    }
                }
                "require_reinit" => {
                    if splits.len() != 1 {
                        return Err(reader.error("require_reinit does not take any parameters"));
                    }
                    self.require_reinit = true;
                }
                "name" | "group" | "subgroup" => {
                    if splits.len() == 1 {
                        return Err(reader.error(format!("{command} requires a parameter")));
                    }
                    let value = Some(trim(&line[command.len() + 1..]).to_owned());
                    match command {
                        "name" => self.display_name = value,
                        "group" => self.display_group = value,
                        _ => self.subgroup = value,
                    }
                }
                "assert" => {
                    let header = column_header(&splits, &reader, "assert")?;
                    let sql = read_until_separator(&mut reader, &self.replacements)?;
                    let expected = read_inline_result(&mut reader, &self.replacements, Some(sql), header)?;
                    self.asserts.push(expected);
                }
                "result" | "result_query" => self.process_result(command, &splits, &mut reader)?,
                "retry" => {
                    if splits.len() != 3 {
                        return Err(reader.error(format!("{command} requires two parameters")));
                    }
                    if splits[1] != "load" {
                        return Err(reader.error("Only retry load is supported"));
                    }
                    self.retry_load = splits[2]
                        .parse()
                        .map_err(|_| reader.error(format!("invalid retry count {}", splits[2])))?;
                }
                "template" => {
                    let Some(template) = splits.get(1) else {
                        return Err(reader.error("template requires a file"));
                    };
                    while let Some(line) = reader.read_line(&self.replacements) {
                        if line.is_empty() {
                            break;
                        }
                        let [key, value] = split_char(&line, '=')[..] else {
                            return Err(reader.error("Expected a template parameter in the form of X=Y"));
                        };
                        self.set_replacement(key, value.to_owned());
                    }
                    // Restart the load from the template; the rest of this file is ignored.
                    return self.process_file(template, depth + 1);
                }
                "argument" => {
                    let [_, name, default] = splits[..] else {
                        return Err(
                            reader.error(format!("{command} requires two parameters (name and default)"))
                        );
                    };
                    if !self.handled_arguments.insert(name.to_owned()) {
                        // Already defined: ignore this definition.
                        continue;
                    }
                    let value = self
                        .options
                        .arguments
                        .get(name)
                        .cloned()
                        .unwrap_or_else(|| default.to_owned());
                    self.set_replacement(name, value);
                }
                "include" => {
                    let [_, file] = splits[..] else {
                        return Err(reader.error("include requires a single argument"));
                    };
                    self.process_file(file, depth + 1)?;
                }
                _ => return Err(reader.error(format!("unrecognized command {command}"))),
            }
        }
        Ok(())
    }

    /// `result <columns|file> [arg=value ...]` and `result_query <columns|file> ...`.
    fn process_result(
        &mut self,
        command: &str,
        splits: &[&str],
        reader: &mut LineReader<'_>,
    ) -> Result<(), ParseError> {
        let header = column_header(splits, reader, "result")?;
        let is_file = header.chars().any(|c| c != 'i');
        let mut matches_condition = true;
        for condition in &splits[2..] {
            if !condition.contains('=') {
                return Err(reader.error("result with condition - only = is supported currently"));
            }
            let [argument, value] = split_char(condition, '=')[..] else {
                return Err(reader.error("result with condition must have one equality"));
            };
            let Some(actual) = self.replacement(argument) else {
                return Err(reader.error(format!("Condition argument {argument} not found in benchmark")));
            };
            if actual != value {
                matches_condition = false;
                break;
            }
        }
        let query = if command == "result_query" {
            Some(read_until_separator(reader, &self.replacements)?)
        } else {
            None
        };
        let expected = if is_file {
            if !matches_condition {
                return Ok(());
            }
            let file = replace_all(header, &self.replacements);
            let text = self
                .read_text(&file)
                .map_err(|e| reader.error(format!("could not read result file {file}: {e}")))?;
            let answer = parse_answer_file(&text).map_err(|e| reader.error(format!("{file}: {e}")))?;
            ExpectedResult {
                query,
                column_count: answer.column_count,
                rows: answer.rows,
            }
        } else {
            read_inline_result(reader, &self.replacements, query, header)?
        };
        if matches_condition {
            if !self.results.is_empty() {
                return Err(reader.error("multiple results found"));
            }
            self.results.push(expected);
        }
        Ok(())
    }

    /// The tail of `LoadBenchmark`: validates arguments and assembles the benchmark.
    fn finish(mut self, name: &str) -> Result<Benchmark, ParseError> {
        if let Some(argument) = self
            .options
            .arguments
            .keys()
            .find(|argument| !self.handled_arguments.contains(*argument))
        {
            return Err(ParseError::new(format!(
                "Invalid benchmark argument {argument}: argument was not specified in benchmark {name}"
            )));
        }
        let Some(run) = self.sections.remove(&Section::Run) else {
            return Err(ParseError::new(format!(
                "{name}: Invalid benchmark file: no \"run\" query specified"
            )));
        };
        let arguments = self
            .handled_arguments
            .iter()
            .filter_map(|argument| Some((argument.clone(), self.replacement(argument)?.to_owned())))
            .collect();
        Ok(Benchmark {
            name: name.to_owned(),
            display_name: self.display_name.unwrap_or_else(|| name.to_owned()),
            group: self.display_group.unwrap_or_else(|| group_from_path(name)),
            subgroup: self.subgroup.unwrap_or_default(),
            description: self.description,
            queries: Queries {
                load: self.sections.remove(&Section::Load),
                reload: self.sections.remove(&Section::Reload),
                init: self.sections.remove(&Section::Init),
                run,
                cleanup: self.sections.remove(&Section::Cleanup),
            },
            extensions: self.extensions,
            load_extensions: self.load_extensions,
            cache: self.cache,
            storage: self.storage,
            require_reinit: self.require_reinit,
            result_mode: self.result_mode,
            retry_load: self.retry_load,
            asserts: self.asserts,
            result: self.results.pop(),
            arguments,
        })
    }
}

fn parse_result_mode(splits: &[&str], reader: &LineReader<'_>) -> Result<ResultMode, ParseError> {
    let invalid =
        || reader.error("Invalid argument for resultmode, valid options are: streaming, arrow, materialized");
    match splits {
        [_, "streaming"] => Ok(ResultMode::Streaming),
        [_, "streaming", ..] => Err(reader.error("resultmode 'streaming' does not accept a parameter")),
        [_, "materialized"] => Ok(ResultMode::Materialized),
        [_, "materialized", ..] => Err(reader.error("resultmode 'materialized' does not accept a parameter")),
        [_, "arrow"] => Ok(ResultMode::Arrow {
            batch_size: DEFAULT_ARROW_BATCH_SIZE,
        }),
        [_, "arrow", size] => size
            .parse()
            .map(|batch_size| ResultMode::Arrow { batch_size })
            .map_err(|_| reader.error(format!("invalid arrow batch size {size}"))),
        [_, "arrow", ..] => {
            Err(reader.error("resultmode 'arrow' only takes 1 optional extra parameter (batch_size)"))
        }
        _ => Err(invalid()),
    }
}

/// The column-count header (`III`) or file name of `result`/`assert`.
fn column_header<'s>(
    splits: &[&'s str],
    reader: &LineReader<'_>,
    command: &str,
) -> Result<&'s str, ParseError> {
    match splits.get(1) {
        Some(header) if !header.is_empty() => Ok(header),
        _ => Err(reader.error(format!(
            "{command} must be followed by a column count (e.g. result III)"
        ))),
    }
}

/// Reads SQL lines up to a `----` separator (`result_query` and `assert`).
fn read_until_separator(
    reader: &mut LineReader<'_>,
    replacements: &[(String, String)],
) -> Result<String, ParseError> {
    let mut sql = String::new();
    while let Some(line) = reader.read_line(replacements) {
        if line == "----" {
            return Ok(trim(&sql).to_owned());
        }
        sql.push('\n');
        sql.push_str(&line);
    }
    Err(reader.error("result_query must be followed by a query and a result (separated by ----)"))
}

/// `ReadQueryFromReader`: tab-separated rows up to the next blank line.
fn read_inline_result(
    reader: &mut LineReader<'_>,
    replacements: &[(String, String)],
    query: Option<String>,
    header: &str,
) -> Result<ExpectedResult, ParseError> {
    let column_count = header.len();
    let mut rows = Vec::new();
    while let Some(line) = reader.read_line(replacements) {
        if line.is_empty() {
            break;
        }
        let values = split_str(&line, "\t");
        if values.len() != column_count {
            return Err(reader.error(format!("expected {column_count} values but got {}", values.len())));
        }
        rows.push(values.into_iter().map(str::to_owned).collect());
    }
    Ok(ExpectedResult {
        query,
        column_count,
        rows,
    })
}

/// `ParseGroupFromPath`: `[<parent directory>]`.
fn group_from_path(path: &str) -> String {
    let separators: Vec<usize> = path
        .char_indices()
        .filter(|&(_, c)| c == '/' || c == '\\')
        .map(|(i, _)| i)
        .collect();
    let group = match separators[..] {
        [] => path,
        [end] => &path[..end],
        [.., begin, end] => &path[begin + 1..end],
    };
    format!("[{group}]")
}

/// Extracts `value` from a `# key: value` header comment.
fn header_field<'l>(line: &'l str, key: &str) -> Option<&'l str> {
    let rest = line.trim_start_matches('#').trim_start();
    let value = rest.strip_prefix(key)?.strip_prefix(':')?;
    Some(value.trim())
}

/// `StringUtil::Trim`.
fn trim(s: &str) -> &str {
    s.trim_matches(|c| matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0b' | '\x0c'))
}

/// `StringUtil::Split(str, char)` (`std::getline` semantics): interior empty tokens are
/// kept, a trailing empty token is dropped.
fn split_char(s: &str, delimiter: char) -> Vec<&str> {
    let mut parts: Vec<&str> = s.split(delimiter).collect();
    if parts.last() == Some(&"") {
        parts.pop();
    }
    parts
}

/// `StringUtil::Split(str, string)`: empty tokens are dropped.
fn split_str<'s>(s: &'s str, delimiter: &str) -> Vec<&'s str> {
    let parts: Vec<&str> = s.split(delimiter).filter(|p| !p.is_empty()).collect();
    if parts.is_empty() { vec![s] } else { parts }
}

/// `ProcessReplacements`: substitutes every `${KEY}`.
fn replace_all(s: &str, replacements: &[(String, String)]) -> String {
    let mut out = s.to_owned();
    for (key, value) in replacements {
        let pattern = format!("${{{key}}}");
        if out.contains(&pattern) {
            out = out.replace(&pattern, value);
        }
    }
    out
}

/// `BenchmarkFileReader`: yields trimmed lines with replacements applied at read time.
struct LineReader<'t> {
    path: &'t str,
    lines: std::str::Lines<'t>,
    line_number: usize,
}

impl<'t> LineReader<'t> {
    fn new(path: &'t str, text: &'t str) -> Self {
        Self {
            path,
            lines: text.lines(),
            line_number: 0,
        }
    }

    fn read_line(&mut self, replacements: &[(String, String)]) -> Option<String> {
        let line = self.lines.next()?;
        self.line_number += 1;
        Some(trim(&replace_all(line, replacements)).to_owned())
    }

    fn error(&self, message: impl fmt::Display) -> ParseError {
        ParseError::new(format!("{}:{} - {message}", self.path, self.line_number))
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// A scratch benchmark root populated with files.
    struct Root(PathBuf);

    impl Root {
        fn new(files: &[(&str, &str)]) -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let dir = std::env::temp_dir().join(format!(
                "rustdb-bench-parser-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            for (path, contents) in files {
                let path = dir.join(path);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(path, contents).unwrap();
            }
            Self(dir)
        }

        fn parse_with(&self, name: &str, arguments: &[(&str, &str)]) -> Result<Benchmark, ParseError> {
            let arguments = arguments
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect();
            parse_benchmark(
                name,
                &ParseOptions {
                    root: &self.0,
                    arguments: &arguments,
                },
            )
        }

        fn parse(&self, name: &str) -> Result<Benchmark, ParseError> {
            self.parse_with(name, &[])
        }
    }

    impl Drop for Root {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const SIMPLE: &str = "# name: benchmark/micro/simple.benchmark
# description: SUM(i) over integer, grouped
# group: [aggregate]

name Integer Sum (Grouped)
group aggregate

load
CREATE TABLE integers AS
SELECT i % 5 AS i FROM range(0, 10) tbl(i);

run
SELECT i, SUM(i) FROM integers GROUP BY i ORDER BY i

result II
0\t0
1\t2
";

    #[test]
    fn parses_a_simple_benchmark() {
        let root = Root::new(&[("benchmark/micro/simple.benchmark", SIMPLE)]);
        let b = root.parse("benchmark/micro/simple.benchmark").unwrap();
        assert_eq!(b.name, "benchmark/micro/simple.benchmark");
        assert_eq!(b.display_name, "Integer Sum (Grouped)");
        assert_eq!(b.group, "aggregate");
        assert_eq!(b.subgroup, "");
        assert_eq!(b.description.as_deref(), Some("SUM(i) over integer, grouped"));
        assert_eq!(
            b.queries.load.as_deref(),
            Some("CREATE TABLE integers AS SELECT i % 5 AS i FROM range(0, 10) tbl(i);")
        );
        assert_eq!(
            b.queries.run,
            "SELECT i, SUM(i) FROM integers GROUP BY i ORDER BY i"
        );
        let result = b.result.as_ref().unwrap();
        assert_eq!(result.query, None);
        assert_eq!(result.column_count, 2);
        assert_eq!(result.rows, vec![vec!["0", "0"], vec!["1", "2"]]);
        assert!(b.cache.is_empty());
        assert!(!b.storage.persistent);
        assert_eq!(b.result_mode, ResultMode::Materialized);
        assert_eq!(b.suite(), "micro");
    }

    #[test]
    fn group_defaults_to_parent_directory() {
        let root = Root::new(&[("benchmark/micro/agg/x.benchmark", "run\nSELECT 1\n")]);
        let b = root.parse("benchmark/micro/agg/x.benchmark").unwrap();
        assert_eq!(b.group, "[agg]");
        assert_eq!(b.display_name, "benchmark/micro/agg/x.benchmark");
        assert_eq!(group_from_path("x.benchmark"), "[x.benchmark]");
        assert_eq!(group_from_path("dir/x.benchmark"), "[dir]");
    }

    const TPCH_LOAD: &str = "argument sf 1

require tpch

cache tpch_sf${sf}.duckdb

load
CALL dbgen(sf=${sf});
";

    const TPCH_TEMPLATE: &str = "# name: ${FILE_PATH}
# description: ${DESCRIPTION}

include benchmark/tpch/tpch_load.benchmark.in

name Q${QUERY_NUMBER_PADDED}
group tpch
subgroup sf${sf}

run benchmark/queries/q${QUERY_NUMBER_PADDED}.sql

result answers/sf1/q${QUERY_NUMBER_PADDED}.csv sf=1

result answers/sf100/q${QUERY_NUMBER_PADDED}.csv sf=100
";

    fn tpch_root() -> Root {
        Root::new(&[
            ("benchmark/tpch/tpch_load.benchmark.in", TPCH_LOAD),
            ("benchmark/tpch/sf1/tpch_sf1.benchmark.in", TPCH_TEMPLATE),
            (
                "benchmark/tpch/sf1/q01.benchmark",
                "# name: benchmark/tpch/sf1/q01.benchmark\n# description: Run query 01\n\n\
                 template benchmark/tpch/sf1/tpch_sf1.benchmark.in\nQUERY_NUMBER=1\nQUERY_NUMBER_PADDED=01\n\n\
                 run this is ignored after a template\n",
            ),
            ("benchmark/queries/q01.sql", "SELECT\n  42 AS answer;\n"),
            ("answers/sf1/q01.csv", "answer|x\n42|\"a|b\"\n"),
        ])
    }

    #[test]
    fn resolves_templates_includes_arguments_and_conditional_results() {
        let root = tpch_root();
        let b = root.parse("benchmark/tpch/sf1/q01.benchmark").unwrap();
        assert_eq!(b.display_name, "Q01");
        assert_eq!(b.group, "tpch");
        assert_eq!(b.subgroup, "sf1");
        assert_eq!(b.description.as_deref(), Some("Run query 01"));
        assert_eq!(b.extensions, vec!["tpch"]);
        assert_eq!(b.cache.database.as_deref(), Some("tpch_sf1.duckdb"));
        assert_eq!(b.queries.load.as_deref(), Some("CALL dbgen(sf=1);"));
        assert_eq!(b.queries.run, "SELECT\n  42 AS answer;");
        assert_eq!(b.arguments.get("sf").map(String::as_str), Some("1"));
        let result = b.result.unwrap();
        assert_eq!(result.column_count, 2);
        assert_eq!(result.rows, vec![vec!["42", "a|b"]]);
    }

    #[test]
    fn argument_overrides_change_conditions_and_replacements() {
        let root = tpch_root();
        let b = root
            .parse_with("benchmark/tpch/sf1/q01.benchmark", &[("sf", "10")])
            .unwrap();
        assert_eq!(b.cache.database.as_deref(), Some("tpch_sf10.duckdb"));
        assert_eq!(b.queries.load.as_deref(), Some("CALL dbgen(sf=10);"));
        assert_eq!(b.subgroup, "sf10");
        // Neither sf=1 nor sf=100 matches: no result check (and no missing-file error).
        assert_eq!(b.result, None);

        let err = root
            .parse_with("benchmark/tpch/sf1/q01.benchmark", &[("scale", "10")])
            .unwrap_err();
        assert!(
            err.to_string().contains("Invalid benchmark argument scale"),
            "{err}"
        );
    }

    #[test]
    fn parses_cache_storage_and_mode_directives() {
        let root = Root::new(&[
            (
                "a.benchmark",
                "storage persistent v1.2.0\ncache lineitem.csv.gz\nresultmode arrow 10000\n\
                 retry load 5\nrequire httpfs load_only\nrequire parquet\nrequire_reinit\n\
                 init\nSET threads=1\n\nreload\nSELECT 1\n\ncleanup\nDROP TABLE x\n\nrun\nSELECT 1\n",
            ),
            (
                "b.benchmark",
                "cache tpch_sf1_ingest.duckdb no_connect\nresultmode streaming\nrun\nSELECT 1\n",
            ),
        ]);
        let a = root.parse("a.benchmark").unwrap();
        assert!(a.storage.persistent);
        assert_eq!(a.storage.version.as_deref(), Some("v1.2.0"));
        assert_eq!(a.cache.file.as_deref(), Some("lineitem.csv.gz"));
        assert_eq!(a.cache.database, None);
        assert_eq!(a.result_mode, ResultMode::Arrow { batch_size: 10000 });
        assert_eq!(a.retry_load, 5);
        assert_eq!(a.load_extensions, vec!["httpfs"]);
        assert_eq!(a.extensions, vec!["parquet"]);
        assert!(a.require_reinit);
        assert_eq!(a.queries.init.as_deref(), Some("SET threads=1"));
        assert_eq!(a.queries.reload.as_deref(), Some("SELECT 1"));
        assert_eq!(a.queries.cleanup.as_deref(), Some("DROP TABLE x"));
        assert_eq!(
            a.cache.paths().collect::<Vec<_>>(),
            vec!["duckdb_benchmark_data/lineitem.csv.gz"]
        );

        let b = root.parse("b.benchmark").unwrap();
        assert_eq!(b.cache.database.as_deref(), Some("tpch_sf1_ingest.duckdb"));
        assert!(b.cache.no_connect);
        assert_eq!(b.result_mode, ResultMode::Streaming);
    }

    #[test]
    fn parses_assert_and_result_query() {
        let root = Root::new(&[(
            "a.benchmark",
            "run\nCREATE TABLE ans AS SELECT 1 AS x\n\nassert I\nSELECT count(*)\nFROM t\n----\n3\n\n\
             result_query II\nSELECT x, x + 1 FROM ans\n----\n1\t2\n",
        )]);
        let b = root.parse("a.benchmark").unwrap();
        assert_eq!(b.asserts.len(), 1);
        assert_eq!(b.asserts[0].query.as_deref(), Some("SELECT count(*)\nFROM t"));
        assert_eq!(b.asserts[0].rows, vec![vec!["3"]]);
        let result = b.result.unwrap();
        assert_eq!(result.query.as_deref(), Some("SELECT x, x + 1 FROM ans"));
        assert!(!result.needs_answer_table());
        assert_eq!(result.rows, vec![vec!["1", "2"]]);
    }

    #[test]
    fn inline_results_drop_empty_tab_fields_like_duckdb() {
        let root = Root::new(&[("a.benchmark", "run\nSELECT 1\n\nresult II\n1\t\t2\n")]);
        let b = root.parse("a.benchmark").unwrap();
        assert_eq!(b.result.unwrap().rows, vec![vec!["1", "2"]]);
    }

    #[test]
    fn rejects_malformed_files() {
        let cases = [
            ("frobnicate x\nrun\nSELECT 1\n", "unrecognized command frobnicate"),
            ("load\nSELECT 1\n", "no \"run\" query specified"),
            ("run\nSELECT 1\n\nrun\nSELECT 2\n", "Multiple calls to run"),
            ("run\n\n", "Encountered an empty run node!"),
            (
                "run\nSELECT 1\n\nresult I\n1\n\nresult I\n2\n",
                "multiple results found",
            ),
            ("run\nSELECT 1\n\nresult II\n1\n", "expected 2 values but got 1"),
            ("template x.in\nA=B=C\n", "Expected a template parameter"),
            ("storage sometimes\n", "Invalid argument for storage"),
            ("require a b\n", "require only supports load_only"),
            ("resultmode fancy\n", "Invalid argument for resultmode"),
            ("retry reload 1\n", "Only retry load is supported"),
            (
                "run\nSELECT 1\n\nresult I x=1\n1\n",
                "Condition argument x not found",
            ),
            ("run\nSELECT 1\n\nresult_query I\nSELECT 1\n", "separated by ----"),
        ];
        for (contents, expected) in cases {
            let root = Root::new(&[("a.benchmark", contents)]);
            let err = root.parse("a.benchmark").unwrap_err().to_string();
            assert!(err.contains(expected), "{contents:?}: {err}");
        }
        let root = Root::new(&[]);
        assert!(root.parse("missing.benchmark").is_err());
    }

    #[test]
    fn commands_and_paths_are_lowercased_but_names_are_not() {
        let root = Root::new(&[
            ("q.sql", "SELECT 'MiXeD'"),
            ("a.benchmark", "NAME MiXeD Name\nRUN Q.SQL\n"),
        ]);
        let b = root.parse("a.benchmark").unwrap();
        assert_eq!(b.display_name, "MiXeD Name");
        assert_eq!(b.queries.run, "SELECT 'MiXeD'");
    }

    #[test]
    fn splitting_matches_duckdb() {
        assert_eq!(split_char("a  b ", ' '), vec!["a", "", "b"]);
        assert_eq!(split_char("A=", '='), vec!["A"]);
        assert_eq!(split_str("a\t\tb", "\t"), vec!["a", "b"]);
        assert_eq!(
            replace_all("${A}/${B}", &[("A".into(), "x".into()), ("B".into(), "y".into())]),
            "x/y"
        );
    }

    /// Parses every benchmark of the reference checkout, when it is available.
    #[test]
    fn parses_duckdb_reference_benchmarks() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.context/duckdb-download/duckdb-1.5.6");
        let Ok(names) = crate::discover::discover(&root) else {
            return;
        };
        let arguments = BTreeMap::new();
        let options = ParseOptions {
            root: &root,
            arguments: &arguments,
        };
        let mut failures = Vec::new();
        for name in &names {
            if let Err(e) = parse_benchmark(name, &options) {
                failures.push(format!("{name}: {e}"));
            }
        }
        assert!(names.len() > 900, "found {} benchmarks", names.len());
        assert!(failures.is_empty(), "{failures:#?}");
        let q01 = parse_benchmark("benchmark/tpch/sf1/q01.benchmark", &options).unwrap();
        assert_eq!(q01.display_name, "Q01");
        assert_eq!(q01.result.unwrap().rows.len(), 4);
    }
}
