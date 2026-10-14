//! Differential tests of the cast matrix against DuckDB 1.5.6.
//!
//! Every case is `CAST(CAST(<literal> AS <source>) AS <target>)`, run under `CAST` and
//! `TRY_CAST`. DuckDB's result is captured as the bytes of the result rendered as VARCHAR
//! (`hex(CAST(... AS VARCHAR))`), NULL, or the error message; rustdb must reproduce it byte
//! for byte, both for constant values and for flat vectors (see `cast_oracle/check.rs`).
//!
//! # Tests
//!
//! - `checked_in_subset_matches_duckdb` (always runs): replays the checked-in sample
//!   `tests/data/cast_oracle_subset.tsv` (~2000 cases); needs no DuckDB.
//! - `full_grid_matches_duckdb` (`#[ignore]`): generates the full grid (tens of thousands
//!   of cases, `cast_oracle/grid.rs`), obtains DuckDB's results, compares everything and
//!   writes a coverage report:
//!
//!   ```text
//!   cargo test -p rustdb-function --test cast_oracle -- --ignored --nocapture
//!   ```
//!
//! # Oracle and cache
//!
//! The full test runs the DuckDB CLI at `$DUCKDB_ORACLE` (default:
//! [`oracle::DEFAULT_ORACLE`]) on generated scripts, a few thousand cases per script, in
//! parallel. Parsed results are cached in `target/cast-oracle/batch-<hash>.tsv`, keyed by
//! a hash of the generated SQL, so reruns need no CLI and grid changes only re-run the
//! affected batches. Delete `target/cast-oracle/` to refresh the cache (e.g. after
//! switching oracle binaries). Outputs of a full run, also in `target/cast-oracle/`:
//!
//! - `report.txt`: per-pair counts (cases, matches, rustdb-unimplemented and wrong
//!   results, flat-vector mismatches, blocked cases) and mismatch examples;
//! - `subset.tsv`: a freshly selected sample. To replace the checked-in subset run the
//!   full test with `CAST_ORACLE_UPDATE_SUBSET=1`.
//!
//! # Allowlists
//!
//! Pairs involving a type in [`PENDING_SOURCES`] / [`PENDING_TARGETS`] are not implemented
//! yet: their mismatches are reported but allowed. Remove a type from these lists once its
//! casts land; its cases then turn on automatically. Pairs involving TIMESTAMPTZ are
//! *ICU-dependent*: the oracle CLI has ICU statically loaded, which replaces or adds
//! TIMESTAMPTZ casts that core DuckDB lacks, so their mismatches are reported separately
//! and allowed. [`KNOWN_MISMATCHES`] lists cases of implemented pairs that are known bugs;
//! an entry whose cases all match fails the tests, so the list only shrinks.
//!
//! A case is *blocked* (neither matched nor mismatched) when rustdb cannot build the
//! source value from its literal like DuckDB does, or cannot render a successful result
//! as VARCHAR yet; those are covered by the VARCHAR -> source and target -> VARCHAR pairs.

mod cast_oracle {
    pub mod check;
    pub mod grid;
    pub mod oracle;
    pub mod types;
}

use std::collections::{BTreeSet, HashSet};
use std::path::PathBuf;

use cast_oracle::check::{self, Category, Report};
use cast_oracle::oracle::{self, OracleStats};
use cast_oracle::types::GridType;
use cast_oracle::{grid, types};

/// Source types whose casts rustdb does not implement yet (none at the moment).
const PENDING_SOURCES: &[&str] = &[];

/// Target types that rustdb cannot cast to yet, from any source (none at the moment).
const PENDING_TARGETS: &[&str] = &[];

/// Pairs involving this type depend on ICU in the oracle (see [`Category::IcuDependent`]).
const ICU_TYPE: &str = "TIMESTAMPTZ";

/// Known bugs in implemented casts: `(source, target, source literal)`, where a pattern
/// ending in `*` matches by prefix and a `None` literal is a NULL source. Remove entries
/// once their cases match DuckDB (a fully matching entry fails the tests).
const KNOWN_MISMATCHES: &[(&str, &str, Option<&str>)] = &[];

fn category(source: &GridType, target: &GridType) -> Category {
    if PENDING_SOURCES.contains(&source.sql) || PENDING_TARGETS.contains(&target.sql) {
        Category::Pending
    } else if source.sql == ICU_TYPE || target.sql == ICU_TYPE {
        Category::IcuDependent
    } else {
        Category::Implemented
    }
}

fn is_implemented(case: &oracle::Case) -> bool {
    category(case.source, case.target) == Category::Implemented
}

fn glob(pattern: &str, value: &str) -> bool {
    pattern
        .strip_suffix('*')
        .map_or(pattern == value, |prefix| value.starts_with(prefix))
}

/// The [`KNOWN_MISMATCHES`] entry covering `case`.
fn known_entry(case: &oracle::Case) -> Option<usize> {
    KNOWN_MISMATCHES.iter().position(|&(source, target, text)| {
        glob(source, case.source.sql)
            && glob(target, case.target.sql)
            && match (text, case.text.as_deref()) {
                (Some(pattern), Some(text)) => glob(pattern, text),
                (None, None) => true,
                _ => false,
            }
    })
}

fn subset_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/cast_oracle_subset.tsv")
}

/// The problems that fail a test: mismatches in implemented pairs that are not known bugs,
/// and known-bug entries whose cases all match now.
fn failures(report: &Report) -> Vec<String> {
    let mut failures: Vec<String> = report
        .mismatches
        .iter()
        .filter(|m| is_implemented(&m.case) && known_entry(&m.case).is_none())
        .map(|m| {
            format!(
                "{} -> {} {}: {}{}",
                m.case.source.sql,
                m.case.target.sql,
                m.case.describe_text(),
                if m.unimplemented { "[unimplemented] " } else { "" },
                m.detail
            )
        })
        .collect();
    let still_failing: HashSet<usize> = report
        .mismatches
        .iter()
        .filter_map(|m| known_entry(&m.case))
        .collect();
    let fixed: BTreeSet<usize> = report
        .matched_cases
        .iter()
        .filter_map(known_entry)
        .filter(|entry| !still_failing.contains(entry))
        .collect();
    failures.extend(fixed.into_iter().map(|entry| {
        format!(
            "{:?} matches DuckDB now: remove it from KNOWN_MISMATCHES",
            KNOWN_MISMATCHES[entry]
        )
    }));
    failures
}

/// Mismatches in implemented pairs covered by [`KNOWN_MISMATCHES`].
fn known_count(report: &Report) -> usize {
    report
        .mismatches
        .iter()
        .filter(|m| is_implemented(&m.case) && known_entry(&m.case).is_some())
        .count()
}

fn assert_no_failures(report: &Report) {
    let failures = failures(report);
    assert!(
        failures.is_empty(),
        "{} cast mismatches against DuckDB:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn checked_in_subset_matches_duckdb() {
    let records = oracle::read_records(&subset_path()).expect("checked-in subset");
    assert!(
        records.len() > 500,
        "subset unexpectedly small: {}",
        records.len()
    );
    let report = check::check(&records);
    assert_no_failures(&report);
}

#[test]
#[ignore = "runs the DuckDB CLI (or a warm target/cast-oracle cache); see the module docs"]
fn full_grid_matches_duckdb() {
    let mut stats = OracleStats::default();
    let phase1 = oracle::expectations(&grid::varchar_cases(), &mut stats).expect("phase 1 oracle");
    let phase2 = oracle::expectations(&grid::typed_cases(&phase1), &mut stats).expect("phase 2 oracle");
    let records = [phase1, phase2].concat();
    let report = check::check(&records);

    let dir = oracle::cache_dir();
    let text = report.render(category, known_count(&report));
    std::fs::write(dir.join("report.txt"), &text).expect("write report");
    let summary: String = text.lines().take(6).collect::<Vec<_>>().join("\n");
    println!(
        "{summary}\noracle batches: {} ({} cached)\nfull report: {}",
        stats.batches,
        stats.cached,
        dir.join("report.txt").display()
    );

    // Keep every mismatch of an implemented pair in the sample, so `cargo test` tracks them.
    let mismatching: HashSet<&oracle::Case> = report
        .mismatches
        .iter()
        .filter(|m| is_implemented(&m.case))
        .map(|m| &m.case)
        .collect();
    let subset = grid::select_subset(
        &records,
        |source, target| category(source, target) == Category::Pending,
        |case| mismatching.contains(case),
    );
    let header = format!(
        "Checked-in sample of the cast oracle grid ({} of {} cases), regenerated by\n\
         CAST_ORACLE_UPDATE_SUBSET=1 cargo test -p rustdb-function --test cast_oracle -- --ignored\n\
         Columns: source type, target type, source literal (\\N = NULL), CAST result, TRY_CAST result.",
        subset.len(),
        records.len()
    );
    oracle::write_records(&dir.join("subset.tsv"), &header, &subset).expect("write subset");
    if std::env::var_os("CAST_ORACLE_UPDATE_SUBSET").is_some() {
        oracle::write_records(&subset_path(), &header, &subset).expect("update checked-in subset");
        println!("updated {}", subset_path().display());
    }
    assert_no_failures(&report);
}

#[test]
fn grid_types_render_like_duckdb() {
    // The SQL spellings double as DuckDB's type names except for these aliases.
    for ty in types::all() {
        let expected = match ty.sql {
            "NULL" => "\"NULL\"",
            "TIMETZ" => "TIME WITH TIME ZONE",
            "TIMESTAMPTZ" => "TIMESTAMP WITH TIME ZONE",
            sql => sql,
        };
        assert_eq!(ty.ty.to_string(), expected);
    }
}
