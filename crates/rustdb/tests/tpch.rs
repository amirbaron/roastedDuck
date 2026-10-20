//! The TPC-H extension: `dbgen` generates DuckDB's data, row for row.
//!
//! The fingerprints are DuckDB 1.5.6's `bit_xor(hash(rowid, *COLUMNS(*)))` of each table,
//! which covers every value and its row position.
#![allow(clippy::unwrap_used)]

use rustdb::{Connection, Database};

fn connect() -> Connection {
    let database = Database::open_in_memory().unwrap();
    database.load_extension("tpch").unwrap();
    database.connect().unwrap()
}

/// `(row count, fingerprint)` of table `name`.
fn fingerprint(connection: &mut Connection, name: &str) -> (String, String) {
    let result = connection
        .query(&format!(
            "SELECT count(*), bit_xor(hash(rowid, *COLUMNS(*))) FROM {name}"
        ))
        .unwrap();
    (
        result.value(0, 0).to_varchar().unwrap(),
        result.value(1, 0).to_varchar().unwrap(),
    )
}

fn assert_fingerprints(connection: &mut Connection, suffix: &str, expected: &[(&str, &str, &str)]) {
    for &(table, count, hash) in expected {
        let name = format!("{table}{suffix}");
        assert_eq!(
            fingerprint(connection, &name),
            (count.to_owned(), hash.to_owned()),
            "{name}"
        );
    }
}

#[test]
fn dbgen_matches_duckdb() {
    let mut con = connect();
    con.query("CALL dbgen(sf=0.01)").unwrap();
    assert_fingerprints(
        &mut con,
        "",
        &[
            ("region", "5", "17193118495966993841"),
            ("nation", "25", "2391927269044735039"),
            ("supplier", "100", "6822349637792386231"),
            ("customer", "1500", "4900622174294079424"),
            ("part", "2000", "798918736189659585"),
            ("partsupp", "8000", "12940039762105302986"),
            ("orders", "15000", "15107791644896840936"),
            ("lineitem", "60175", "5470441481022755637"),
        ],
    );
}

#[test]
fn dbgen_generates_one_part_of_the_tables() {
    let mut con = connect();
    con.query("CALL dbgen(sf=1, children=100, step=7, suffix='_part')")
        .unwrap();
    assert_fingerprints(
        &mut con,
        "_part",
        &[
            ("supplier", "100", "6080066733237027428"),
            ("customer", "1500", "1717888789557382024"),
            ("part", "2000", "1477969532075503141"),
            ("partsupp", "8000", "8253277148567273377"),
            ("orders", "15000", "6896046611359162624"),
            ("lineitem", "60401", "6266490477032850154"),
        ],
    );
}

#[test]
fn queries_answers_and_pragma() {
    let mut con = connect();
    let queries = con.query("SELECT count(*) FROM tpch_queries()").unwrap();
    assert_eq!(queries.value(0, 0).to_varchar().unwrap(), "22");
    let answers = con
        .query("SELECT count(*), count(DISTINCT scale_factor) FROM tpch_answers()")
        .unwrap();
    assert_eq!(answers.value(0, 0).to_varchar().unwrap(), "66");
    assert_eq!(answers.value(1, 0).to_varchar().unwrap(), "3");
    con.query("CALL dbgen(sf=0.01)").unwrap();
    assert_eq!(con.query("PRAGMA tpch(1)").unwrap().row_count(), 4);
    assert_eq!(con.query("PRAGMA tpch((2 + 3) % 22 + 1)").unwrap().row_count(), 1);
    let error = con.query("PRAGMA tpch(23)").unwrap_err().to_string();
    assert!(error.contains("Out of range TPC-H query number 23"), "{error}");
}

#[test]
fn dbgen_validates_its_parameters() {
    let mut con = connect();
    let error = con
        .query("CALL dbgen(sf=0.01, children=2)")
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("Step must be defined when children are defined"),
        "{error}"
    );
    let error = con.query("CALL dbgen(sf=NULL)").unwrap_err().to_string();
    assert!(error.contains("Cannot use NULL as function argument"), "{error}");
    let error = con
        .query("CALL dbgen(sf=0, catalog='nope')")
        .unwrap_err()
        .to_string();
    assert!(error.contains("Catalog \"nope\" does not exist!"), "{error}");
}
