//! Expected cast results: the record format shared by the checked-in subset and the cache,
//! oracle script generation, running the DuckDB CLI and caching its parsed output.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::{fs, thread};

use super::types::{self, Family, GridType};

/// The DuckDB CLI used when `DUCKDB_ORACLE` is not set.
pub const DEFAULT_ORACLE: &str = "/Users/amirbaron/conductor/workspaces/duckdb-rust/ottawa/.context/duckdb-download/duckdb-1.5.6/build/release/duckdb";

/// Cases per oracle script (and per cache file).
const BATCH_SIZE: usize = 2500;

/// The observable result of one cast statement: the bytes of the result rendered as
/// VARCHAR, SQL NULL, or the final error message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Value(Vec<u8>),
    Null,
    Error(String),
}

impl Outcome {
    /// Short human-readable form for reports.
    pub fn describe(&self) -> String {
        match self {
            Self::Value(bytes) => format!("'{}'", String::from_utf8_lossy(bytes)),
            Self::Null => "NULL".to_owned(),
            Self::Error(message) => message.replace('\n', "\\n"),
        }
    }

    fn encode(&self) -> String {
        match self {
            Self::Null => "null".to_owned(),
            Self::Value(bytes) => match std::str::from_utf8(bytes) {
                Ok(text) => format!("value:{}", escape(text)),
                Err(_) => format!("bytes:{}", hex(bytes)),
            },
            Self::Error(message) => format!("error:{}", escape(message)),
        }
    }

    fn decode(field: &str) -> Option<Self> {
        if field == "null" {
            return Some(Self::Null);
        }
        if let Some(text) = field.strip_prefix("value:") {
            return Some(Self::Value(unescape(text)?.into_bytes()));
        }
        if let Some(digits) = field.strip_prefix("bytes:") {
            return Some(Self::Value(unhex(digits)?));
        }
        Some(Self::Error(unescape(field.strip_prefix("error:")?)?))
    }
}

/// One cast case: `CAST(CAST(<text> AS <source>) AS <target>)`, where a `None` text is a
/// NULL source.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Case {
    pub source: &'static GridType,
    pub target: &'static GridType,
    pub text: Option<String>,
}

impl Case {
    /// The SQL expression producing the source value.
    fn source_sql(&self) -> String {
        match (&self.text, self.source.family) {
            (_, Family::Null) => "NULL".to_owned(),
            (None, _) => format!("CAST(NULL AS {})", self.source.sql),
            (Some(text), _) => format!("CAST('{}' AS {})", text.replace('\'', "''"), self.source.sql),
        }
    }

    /// The source text as shown in reports.
    pub fn describe_text(&self) -> String {
        self.text
            .as_ref()
            .map_or_else(|| "NULL".to_owned(), |text| format!("'{}'", text.escape_debug()))
    }
}

/// A case with DuckDB's results under `CAST` and `TRY_CAST`.
#[derive(Clone, Debug)]
pub struct Record {
    pub case: Case,
    pub cast: Outcome,
    pub try_cast: Outcome,
}

impl Record {
    /// One tab-separated line: `source, target, text, cast, try_cast`.
    pub fn to_line(&self) -> String {
        let text = self.case.text.as_deref().map_or_else(|| "\\N".to_owned(), escape);
        format!(
            "{}\t{}\t{}\t{}\t{}",
            self.case.source.sql,
            self.case.target.sql,
            text,
            self.cast.encode(),
            self.try_cast.encode()
        )
    }

    pub fn from_line(line: &str) -> Option<Self> {
        let mut fields = line.split('\t');
        let source = types::lookup(fields.next()?)?;
        let target = types::lookup(fields.next()?)?;
        let text = match fields.next()? {
            "\\N" => None,
            text => Some(unescape(text)?),
        };
        let cast = Outcome::decode(fields.next()?)?;
        let try_cast = Outcome::decode(fields.next()?)?;
        if fields.next().is_some() {
            return None;
        }
        Some(Self {
            case: Case { source, target, text },
            cast,
            try_cast,
        })
    }
}

/// Reads a record file (`#` lines are comments).
pub fn read_records(path: &Path) -> Result<Vec<Record>, String> {
    let content = fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    content
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.is_empty() && !line.starts_with('#'))
        .map(|(number, line)| {
            Record::from_line(line)
                .ok_or_else(|| format!("{}:{}: malformed record", path.display(), number + 1))
        })
        .collect()
}

/// Writes a record file with a comment header.
pub fn write_records(path: &Path, header: &str, records: &[Record]) -> Result<(), String> {
    let mut content = String::new();
    for line in header.lines() {
        let _ = writeln!(content, "# {line}");
    }
    for record in records {
        content.push_str(&record.to_line());
        content.push('\n');
    }
    write_atomically(path, content.as_bytes())
}

fn write_atomically(path: &Path, content: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, content).map_err(|e| format!("cannot write {}: {e}", temporary.display()))?;
    fs::rename(&temporary, path).map_err(|e| format!("cannot rename to {}: {e}", path.display()))
}

/// Escapes backslashes, tabs, newlines and carriage returns.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out
}

fn unescape(text: &str) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        out.push(match chars.next()? {
            '\\' => '\\',
            't' => '\t',
            'n' => '\n',
            'r' => '\r',
            _ => return None,
        });
    }
    Some(out)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02X}");
        out
    })
}

fn unhex(digits: &str) -> Option<Vec<u8>> {
    if !digits.len().is_multiple_of(2) {
        return None;
    }
    (0..digits.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(digits.get(i..i + 2)?, 16).ok())
        .collect()
}

/// The workspace `target` directory (honours `CARGO_TARGET_DIR`).
pub fn target_dir() -> PathBuf {
    let dir = std::env::var_os("CARGO_TARGET_DIR").map_or_else(
        || Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target"),
        PathBuf::from,
    );
    dir.canonicalize().unwrap_or(dir)
}

/// Where parsed oracle output, reports and the regenerated subset live.
pub fn cache_dir() -> PathBuf {
    target_dir().join("cast-oracle")
}

/// The DuckDB CLI: `$DUCKDB_ORACLE`, else [`DEFAULT_ORACLE`].
pub fn oracle_cli() -> PathBuf {
    std::env::var_os("DUCKDB_ORACLE").map_or_else(|| PathBuf::from(DEFAULT_ORACLE), PathBuf::from)
}

/// The oracle script of a batch: per case a marker and the statement, under `CAST` and
/// `TRY_CAST`. Results are hex-encoded so every output is one byte-exact line.
fn script(cases: &[Case]) -> String {
    let mut sql = String::from("SET TimeZone='UTC';\n.nullvalue NULL\n");
    for (id, case) in cases.iter().enumerate() {
        let source = case.source_sql();
        let target = case.target.sql;
        let _ = writeln!(sql, "SELECT '@@C {id}';");
        let _ = writeln!(sql, "SELECT hex(CAST(CAST({source} AS {target}) AS VARCHAR));");
        let _ = writeln!(sql, "SELECT '@@T {id}';");
        let _ = writeln!(
            sql,
            "SELECT hex(CAST(TRY_CAST({source} AS {target}) AS VARCHAR));"
        );
    }
    sql
}

/// 64-bit FNV-1a: a stable cache key for a script.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// Parses one statement's output: a hex line, `NULL`, or an error block (message, then
/// optionally a blank line and the `LINE n:` context, which is dropped).
fn parse_output(lines: &[&str]) -> Outcome {
    if let [line] = lines {
        if *line == "NULL" {
            return Outcome::Null;
        }
        if let Some(bytes) = unhex(line) {
            return Outcome::Value(bytes);
        }
    }
    let block = lines.join("\n");
    let block = block.trim_end_matches('\n');
    let message = block.rfind("\n\nLINE ").map_or(block, |end| &block[..end]);
    Outcome::Error(message.to_owned())
}

/// Parses the CLI output of a batch script into per-case `(cast, try_cast)` outcomes.
fn parse_script_output(output: &str, count: usize) -> Result<Vec<(Outcome, Outcome)>, String> {
    let mut blocks: HashMap<(bool, usize), Vec<&str>> = HashMap::new();
    let mut current: Option<(bool, usize)> = None;
    for line in output.lines() {
        let marker = line
            .strip_prefix("@@C ")
            .map(|id| (false, id))
            .or_else(|| line.strip_prefix("@@T ").map(|id| (true, id)));
        if let Some((is_try, id)) = marker
            && let Ok(id) = id.parse()
        {
            current = Some((is_try, id));
            blocks.insert((is_try, id), Vec::new());
            continue;
        }
        match current {
            Some(key) => blocks.entry(key).or_default().push(line),
            None => return Err(format!("unexpected output before the first case: {line}")),
        }
    }
    (0..count)
        .map(|id| {
            let outcome = |is_try| {
                blocks
                    .get(&(is_try, id))
                    .map(|lines| parse_output(lines))
                    .ok_or_else(|| format!("no output for case {id} (CLI crashed?)"))
            };
            Ok((outcome(false)?, outcome(true)?))
        })
        .collect()
}

/// Runs the CLI on `script`, with stdout and stderr in one file to keep their order.
fn run_cli(cli: &Path, script: &str, work: &Path) -> Result<String, String> {
    let script_path = work.with_extension("sql");
    let output_path = work.with_extension("out");
    fs::write(&script_path, script).map_err(|e| format!("cannot write {}: {e}", script_path.display()))?;
    let stdin = fs::File::open(&script_path).map_err(|e| e.to_string())?;
    let stdout = fs::File::create(&output_path).map_err(|e| e.to_string())?;
    let stderr = stdout.try_clone().map_err(|e| e.to_string())?;
    Command::new(cli)
        .args(["-list", "-noheader"])
        .stdin(Stdio::from(stdin))
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .status()
        .map_err(|e| {
            format!(
                "cannot run the DuckDB oracle {}: {e} (set DUCKDB_ORACLE)",
                cli.display()
            )
        })?;
    let output = fs::read(&output_path).map_err(|e| e.to_string())?;
    let _ = fs::remove_file(&script_path);
    let _ = fs::remove_file(&output_path);
    Ok(String::from_utf8_lossy(&output).into_owned())
}

/// Statistics of an oracle run.
#[derive(Debug, Default)]
pub struct OracleStats {
    pub batches: usize,
    pub cached: usize,
}

/// DuckDB's results for `cases`, from the cache under [`cache_dir`] when the generated
/// script is unchanged, else by running the CLI (batches run in parallel).
pub fn expectations(cases: &[Case], stats: &mut OracleStats) -> Result<Vec<Record>, String> {
    let dir = cache_dir();
    fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let batches: Vec<&[Case]> = cases.chunks(BATCH_SIZE).collect();
    let keyed: Vec<(PathBuf, String)> = batches
        .iter()
        .map(|batch| {
            let sql = script(batch);
            (dir.join(format!("batch-{:016x}.tsv", fnv1a(sql.as_bytes()))), sql)
        })
        .collect();
    stats.batches += batches.len();
    let cli = oracle_cli();
    let parallelism = thread::available_parallelism().map_or(4, usize::from);
    let mut results: Vec<Option<Result<Vec<Record>, String>>> = vec![None; batches.len()];
    for (index, (path, _)) in keyed.iter().enumerate() {
        if let Ok(records) = read_records(path)
            && records.len() == batches[index].len()
            && records
                .iter()
                .zip(batches[index])
                .all(|(record, case)| &record.case == case)
        {
            stats.cached += 1;
            results[index] = Some(Ok(records));
        }
    }
    let pending: Vec<usize> = (0..batches.len()).filter(|&i| results[i].is_none()).collect();
    for wave in pending.chunks(parallelism) {
        let computed: Vec<(usize, Result<Vec<Record>, String>)> = thread::scope(|scope| {
            let handles: Vec<_> = wave
                .iter()
                .map(|&index| {
                    let (path, sql) = &keyed[index];
                    let batch = batches[index];
                    let cli = &cli;
                    scope.spawn(move || {
                        let output = run_cli(cli, sql, path)?;
                        let outcomes = parse_script_output(&output, batch.len())?;
                        let records: Vec<Record> = batch
                            .iter()
                            .zip(outcomes)
                            .map(|(case, (cast, try_cast))| Record {
                                case: case.clone(),
                                cast,
                                try_cast,
                            })
                            .collect();
                        write_records(path, "parsed DuckDB oracle output (cache)", &records)?;
                        Ok(records)
                    })
                })
                .collect();
            wave.iter()
                .zip(handles)
                .map(|(&index, handle)| {
                    let result = handle
                        .join()
                        .unwrap_or_else(|_| Err("oracle thread panicked".to_owned()));
                    (index, result)
                })
                .collect()
        });
        for (index, result) in computed {
            results[index] = Some(result);
        }
    }
    let mut records = Vec::with_capacity(cases.len());
    for result in results.into_iter().flatten() {
        records.extend(result?);
    }
    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_round_trip() {
        let record = Record {
            case: Case {
                source: types::get("VARCHAR"),
                target: types::get("INTEGER"),
                text: Some("a\tb\\n\n'".to_owned()),
            },
            cast: Outcome::Error("Conversion Error: x\ny".to_owned()),
            try_cast: Outcome::Value(vec![0xff, b'a']),
        };
        let parsed = Record::from_line(&record.to_line()).expect("round trip");
        assert_eq!(parsed.case, record.case);
        assert_eq!(parsed.cast, record.cast);
        assert_eq!(parsed.try_cast, record.try_cast);
    }

    #[test]
    fn parses_cli_blocks() {
        let output =
            "@@C 0\nConversion Error: bad\n\nLINE 1: SELECT ...\n     ^\n@@T 0\nNULL\n@@C 1\n\n@@T 1\n3132\n";
        let parsed = parse_script_output(output, 2).expect("parse");
        assert_eq!(
            parsed[0],
            (Outcome::Error("Conversion Error: bad".into()), Outcome::Null)
        );
        assert_eq!(
            parsed[1],
            (Outcome::Value(Vec::new()), Outcome::Value(b"12".to_vec()))
        );
    }
}
