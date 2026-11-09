//! Runs the cases through roastedduck's casts and compares with DuckDB's results.
//!
//! Every case is cast as a constant (`ValueCast::cast_with`) under `CAST` and `TRY_CAST`.
//! In addition, the rows of each (source, target) pair are cast together as one flat
//! vector under `TRY_CAST` and compared row by row.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::panic::{AssertUnwindSafe, catch_unwind};

use roastedduck_function::cast::{CastFunctionSet, CastParameters, ValueCast, cast_vector};
use roastedduck_types::{LogicalType, LogicalTypeId, Value, Vector};

use super::oracle::{Case, Outcome, Record};
use super::types::{self, Family, GridType};

/// Mismatch examples kept per pair.
const EXAMPLES_PER_PAIR: usize = 4;

/// Whether roastedduck's error says the cast (or a part of it) is not implemented yet.
pub fn is_unimplemented(message: &str) -> bool {
    message.contains("Unimplemented type for cast") || message.starts_with("Not implemented Error")
}

/// What roastedduck produced for one statement.
enum Actual {
    Outcome(Outcome),
    /// The cast succeeded but the result cannot be rendered as VARCHAR yet.
    Unrenderable(String),
}

/// Runs `f`, turning a panic into an error message.
fn guarded<T>(f: impl FnOnce() -> T) -> Result<T, String> {
    catch_unwind(AssertUnwindSafe(f)).map_err(|payload| {
        let message = payload
            .downcast_ref::<&str>()
            .map(|s| (*s).to_owned())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_default();
        format!("PANIC: {message}")
    })
}

/// Renders a cast result like `CAST(<result> AS VARCHAR)` does in the oracle script.
fn render(set: &CastFunctionSet, value: &Value) -> Actual {
    if value.is_null() {
        return Actual::Outcome(Outcome::Null);
    }
    if value.logical_type().id() == LogicalTypeId::Varchar {
        return Actual::Outcome(Outcome::Value(value.as_bytes().unwrap_or_default().to_vec()));
    }
    let text = guarded(|| value.cast_with(set, &LogicalType::VARCHAR, &mut CastParameters::new()));
    match text {
        Ok(Ok(text)) if text.is_null() => Actual::Outcome(Outcome::Null),
        Ok(Ok(text)) => Actual::Outcome(Outcome::Value(text.as_bytes().unwrap_or_default().to_vec())),
        Ok(Err(error)) if is_unimplemented(&error.to_string()) => {
            Actual::Unrenderable(format!("{} -> VARCHAR: {error}", value.logical_type()).replace('\n', "\\n"))
        }
        Ok(Err(error)) => Actual::Outcome(Outcome::Error(error.to_string())),
        Err(panic) => Actual::Outcome(Outcome::Error(panic)),
    }
}

fn parameters(try_cast: bool) -> CastParameters {
    if try_cast {
        CastParameters::try_cast()
    } else {
        CastParameters::new()
    }
}

fn cast_value(set: &CastFunctionSet, source: &Value, target: &GridType, try_cast: bool) -> Actual {
    match guarded(|| source.cast_with(set, &target.ty, &mut parameters(try_cast))) {
        Ok(Ok(value)) => render(set, &value),
        Ok(Err(error)) => Actual::Outcome(Outcome::Error(error.to_string())),
        Err(panic) => Actual::Outcome(Outcome::Error(panic)),
    }
}

/// Builds the source value of a case the way the oracle does: by casting its text.
fn source_value(set: &CastFunctionSet, case: &Case) -> Result<Value, String> {
    let ty = &case.source.ty;
    match (case.source.family, &case.text) {
        (Family::Null, _) => Ok(Value::null(LogicalType::SQLNULL)),
        (_, None) => Ok(Value::null(ty.clone())),
        (Family::Varchar, Some(text)) => Ok(Value::varchar(text)),
        (_, Some(text)) => {
            match guarded(|| Value::varchar(text).cast_with(set, ty, &mut CastParameters::new())) {
                Ok(Ok(value)) => Ok(value),
                Ok(Err(error)) => Err(error.to_string()),
                Err(panic) => Err(panic),
            }
        }
    }
}

/// How one statement compared.
enum Comparison {
    Match,
    Blocked(String),
    Mismatch(String),
}

fn compare(expected: &Outcome, actual: Actual) -> Comparison {
    match actual {
        Actual::Outcome(actual) if &actual == expected => Comparison::Match,
        Actual::Outcome(actual) => Comparison::Mismatch(actual.describe()),
        Actual::Unrenderable(reason) if matches!(expected, Outcome::Value(_)) => Comparison::Blocked(reason),
        Actual::Unrenderable(_) => Comparison::Mismatch("<a non-NULL value>".to_owned()),
    }
}

/// A case whose roastedduck result differs from DuckDB's.
#[derive(Debug)]
pub struct Mismatch {
    pub case: Case,
    /// roastedduck reported the cast as not implemented.
    pub unimplemented: bool,
    pub detail: String,
}

/// Per (source, target) pair counts.
#[derive(Debug, Default)]
pub struct PairStats {
    pub cases: usize,
    pub matched: usize,
    pub unimplemented: usize,
    pub wrong: usize,
    pub blocked: usize,
    pub vector_rows: usize,
    pub vector_mismatches: usize,
    pub examples: Vec<String>,
    pub blocked_reason: Option<String>,
}

impl PairStats {
    pub fn mismatches(&self) -> usize {
        self.unimplemented + self.wrong + self.vector_mismatches
    }
}

/// The outcome of checking a set of records.
#[derive(Debug, Default)]
pub struct Report {
    /// Keyed by the grid positions of (source, target).
    pub pairs: BTreeMap<(usize, usize), PairStats>,
    pub mismatches: Vec<Mismatch>,
    /// Cases that matched under both modes and in the vector path.
    pub matched_cases: Vec<Case>,
}

fn grid_position(ty: &GridType) -> usize {
    types::all().iter().position(|t| t == ty).unwrap_or(usize::MAX)
}

fn pair_key(case: &Case) -> (usize, usize) {
    (grid_position(case.source), grid_position(case.target))
}

/// Checks `records` against roastedduck's default casts.
pub fn check(records: &[Record]) -> Report {
    let set = CastFunctionSet::new();
    // DuckDB's rendering of each typed source value (the VARCHAR -> T records).
    let source_texts: HashMap<(&str, &str), &Outcome> = records
        .iter()
        .filter(|r| r.case.source.family == Family::Varchar)
        .filter_map(|r| Some(((r.case.target.sql, r.case.text.as_deref()?), &r.cast)))
        .collect();
    let mut report = Report::default();
    let mut vector_rows: BTreeMap<(usize, usize), Vec<(Value, &Record)>> = BTreeMap::new();
    for record in records {
        let case = &record.case;
        let key = pair_key(case);
        let source = source_value(&set, case).and_then(|value| {
            check_source(&set, case, &value, &source_texts)?;
            Ok(value)
        });
        let stats = report.pairs.entry(key).or_default();
        stats.cases += 1;
        let source = match source {
            Ok(source) => source,
            Err(reason) => {
                stats.blocked += 1;
                let reason = reason.replace('\n', "\\n");
                stats
                    .blocked_reason
                    .get_or_insert(format!("source {}: {reason}", case.describe_text()));
                continue;
            }
        };
        let cast = compare(&record.cast, cast_value(&set, &source, case.target, false));
        let try_cast = compare(&record.try_cast, cast_value(&set, &source, case.target, true));
        let mut detail = String::new();
        let mut unimplemented = false;
        let mut blocked = None;
        for (mode, expected, comparison) in [
            ("CAST", &record.cast, cast),
            ("TRY_CAST", &record.try_cast, try_cast),
        ] {
            match comparison {
                Comparison::Match => {}
                Comparison::Blocked(reason) => blocked = Some(reason),
                Comparison::Mismatch(actual) => {
                    unimplemented |= is_unimplemented(&actual);
                    let _ = write!(
                        detail,
                        "{}{mode}: expected {} got {actual}",
                        if detail.is_empty() { "" } else { "; " },
                        expected.describe()
                    );
                }
            }
        }
        if !detail.is_empty() {
            if unimplemented {
                stats.unimplemented += 1;
            } else {
                stats.wrong += 1;
            }
            if stats.examples.len() < EXAMPLES_PER_PAIR {
                stats
                    .examples
                    .push(format!("{} => {detail}", case.describe_text()));
            }
            report.mismatches.push(Mismatch {
                case: case.clone(),
                unimplemented,
                detail,
            });
        } else if let Some(reason) = blocked {
            stats.blocked += 1;
            stats.blocked_reason.get_or_insert(reason);
        } else {
            stats.matched += 1;
            if matches!(record.try_cast, Outcome::Error(_)) {
                report.matched_cases.push(case.clone());
            } else {
                vector_rows.entry(key).or_default().push((source, record));
            }
        }
    }
    for (key, rows) in vector_rows {
        let stats = report.pairs.entry(key).or_default();
        stats.vector_rows += rows.len();
        let failures = check_vector(&set, &rows);
        let failed: Vec<&Case> = failures.iter().map(|(row, _)| &rows[*row].1.case).collect();
        for (_, record) in &rows {
            if !failed.contains(&&record.case) {
                report.matched_cases.push(record.case.clone());
            }
        }
        for (row, detail) in failures {
            let case = &rows[row].1.case;
            stats.vector_mismatches += 1;
            if stats.examples.len() < EXAMPLES_PER_PAIR {
                stats
                    .examples
                    .push(format!("{} => vector TRY_CAST: {detail}", case.describe_text()));
            }
            report.mismatches.push(Mismatch {
                case: case.clone(),
                unimplemented: is_unimplemented(&detail),
                detail: format!("vector TRY_CAST: {detail}"),
            });
        }
    }
    report
}

/// Verifies that roastedduck built the same source value as DuckDB (when it can render it).
fn check_source(
    set: &CastFunctionSet,
    case: &Case,
    value: &Value,
    source_texts: &HashMap<(&str, &str), &Outcome>,
) -> Result<(), String> {
    let Some(text) = case.text.as_deref() else {
        return Ok(());
    };
    if matches!(case.source.family, Family::Varchar | Family::Null) {
        return Ok(());
    }
    let Some(expected) = source_texts.get(&(case.source.sql, text)) else {
        return Ok(());
    };
    match render(set, value) {
        Actual::Outcome(actual) if &&actual != expected => Err(format!(
            "built as {} instead of {}",
            actual.describe(),
            expected.describe()
        )),
        _ => Ok(()),
    }
}

/// Casts the rows of one pair as a flat vector under `TRY_CAST`; returns the failing rows.
fn check_vector(set: &CastFunctionSet, rows: &[(Value, &Record)]) -> Vec<(usize, String)> {
    let Some((first, record)) = rows.first() else {
        return Vec::new();
    };
    let source_type = first.logical_type().clone();
    let target = record.case.target;
    let all_rows = |detail: String| (0..rows.len()).map(|row| (row, detail.clone())).collect();
    let built = guarded(|| {
        let mut vector = Vector::flat(source_type, rows.len());
        for (row, (value, _)) in rows.iter().enumerate() {
            vector.set_value(row, value)?;
        }
        Ok::<_, roastedduck_common::Error>(vector)
    });
    let vector = match built {
        Ok(Ok(vector)) => vector,
        Ok(Err(error)) => return all_rows(format!("building the source vector: {error}")),
        Err(panic) => return all_rows(format!("building the source vector: {panic}")),
    };
    let result = guarded(|| {
        cast_vector(
            set,
            &vector,
            &target.ty,
            rows.len(),
            &mut CastParameters::try_cast(),
        )
    });
    let result = match result {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => return all_rows(error.to_string()),
        Err(panic) => return all_rows(panic),
    };
    rows.iter()
        .enumerate()
        .filter_map(|(row, (_, record))| {
            let actual = match guarded(|| result.get_value(row)) {
                Ok(value) => render(set, &value),
                Err(panic) => Actual::Outcome(Outcome::Error(panic)),
            };
            match compare(&record.try_cast, actual) {
                Comparison::Mismatch(actual) => Some((
                    row,
                    format!("expected {} got {actual}", record.try_cast.describe()),
                )),
                Comparison::Match | Comparison::Blocked(_) => None,
            }
        })
        .collect()
}

/// How a (source, target) pair is judged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    /// Implemented in roastedduck: every mismatch is a bug.
    Implemented,
    /// Involves TIMESTAMP WITH TIME ZONE, whose casts the oracle's statically loaded ICU
    /// extension partly replaces: mismatches are reported but allowed.
    IcuDependent,
    /// Not implemented in roastedduck yet: mismatches are reported but allowed.
    Pending,
}

impl Category {
    const ALL: [Self; 3] = [Self::Implemented, Self::IcuDependent, Self::Pending];

    fn name(self) -> &'static str {
        match self {
            Self::Implemented => "implemented",
            Self::IcuDependent => "ICU-dependent",
            Self::Pending => "pending",
        }
    }
}

impl Report {
    /// The coverage report: totals, per-pair counts and mismatch examples, per category.
    /// `known` is the number of implemented-pair mismatches allowlisted as known bugs.
    pub fn render(&self, category: impl Fn(&GridType, &GridType) -> Category, known: usize) -> String {
        let all = types::all();
        let category = &category;
        let pair_name = |(s, t): (usize, usize)| format!("{} -> {}", all[s].sql, all[t].sql);
        let in_category = |cat: Category| {
            self.pairs
                .iter()
                .filter(move |&(&(s, t), _)| category(&all[s], &all[t]) == cat)
        };
        let total = |f: fn(&PairStats) -> usize| self.pairs.values().map(f).sum::<usize>();
        let mut out = String::new();
        let _ = writeln!(out, "roastedduck cast oracle report (DuckDB 1.5.6 differential)");
        let _ = writeln!(
            out,
            "pairs: {}  cases: {}  (each case runs as CAST and TRY_CAST; vector path rows: {})",
            self.pairs.len(),
            total(|s| s.cases),
            total(|s| s.vector_rows),
        );
        let _ = writeln!(
            out,
            "matched: {}  mismatched: {} (roastedduck unimplemented: {}, wrong: {}, vector path: {})  blocked: {}",
            total(|s| s.matched),
            total(|s| s.unimplemented + s.wrong),
            total(|s| s.unimplemented),
            total(|s| s.wrong),
            total(|s| s.vector_mismatches),
            total(|s| s.blocked),
        );
        for cat in Category::ALL {
            let (mut pairs, mut cases, mut matched, mut mismatches, mut blocked) = (0, 0, 0, 0, 0);
            for (_, stats) in in_category(cat) {
                pairs += 1;
                cases += stats.cases;
                matched += stats.matched;
                mismatches += stats.mismatches();
                blocked += stats.blocked;
            }
            let note = match cat {
                Category::Implemented => format!("{known} allowlisted as known bugs"),
                Category::IcuDependent | Category::Pending => "allowed".to_owned(),
            };
            let _ = writeln!(
                out,
                "{:<14} {pairs:>5} pairs {cases:>6} cases {matched:>6} matched {mismatches:>5} mismatches \
                 ({note}) {blocked:>5} blocked",
                format!("{}:", cat.name()),
            );
        }
        out.push_str(
            "\nblocked = roastedduck cannot build/verify the source value from its literal, or cannot render\n\
             the result as VARCHAR (not a mismatch of the pair itself).\n",
        );
        for cat in Category::ALL {
            let _ = writeln!(out, "\n== {} pairs ==", cat.name());
            let _ = writeln!(
                out,
                "{:<62} {:>6} {:>6} {:>6} {:>6} {:>6} {:>6}",
                "pair", "cases", "match", "unimpl", "wrong", "vector", "blockd"
            );
            for (&key, stats) in in_category(cat) {
                let _ = writeln!(
                    out,
                    "{:<62} {:>6} {:>6} {:>6} {:>6} {:>6} {:>6}",
                    pair_name(key),
                    stats.cases,
                    stats.matched,
                    stats.unimplemented,
                    stats.wrong,
                    stats.vector_mismatches,
                    stats.blocked
                );
            }
        }
        for cat in Category::ALL {
            let _ = writeln!(out, "\n== mismatch examples: {} pairs ==", cat.name());
            for (&key, stats) in in_category(cat) {
                if stats.examples.is_empty() && stats.blocked_reason.is_none() {
                    continue;
                }
                let _ = writeln!(
                    out,
                    "{} ({} mismatches, {} blocked)",
                    pair_name(key),
                    stats.mismatches(),
                    stats.blocked
                );
                for example in &stats.examples {
                    let _ = writeln!(out, "    {example}");
                }
                if let Some(reason) = &stats.blocked_reason {
                    let _ = writeln!(out, "    blocked, e.g. {reason}");
                }
            }
        }
        out
    }
}
