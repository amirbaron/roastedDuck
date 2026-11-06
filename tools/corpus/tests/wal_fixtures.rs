//! Write-ahead logs produced by DuckDB 1.5.6 (`tests/fixtures/ddl-*.wal`, from tables,
//! constraints, views, macros, enum types, sequences, indexes, ALTER and COMMENT
//! statements): every catalog entry must read into the parse tree and write back to the
//! same bytes.

#![cfg(test)]

use corpus::wal::{self, Entry};
use rustdb_serde::SerializationCompatibility;

fn check_fixture(name: &str, version: &str, expected_entries: usize) {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    let data = std::fs::read(&path).expect("fixture exists");
    let entries: Vec<Entry<'_>> = wal::entries(&data).expect("well-formed WAL");
    let compatibility = SerializationCompatibility::from_version_name(version).expect("known version");
    let mut checked = Vec::new();
    for entry in &entries {
        if let Some(outcome) = wal::check_entry(entry, &compatibility) {
            match outcome {
                Ok(kind) => checked.push(kind),
                Err(mismatch) => panic!("{name}: {} differs: {}", mismatch.kind, mismatch.detail),
            }
        }
    }
    assert_eq!(checked.len(), expected_entries, "{name}: {checked:?}");
}

#[test]
fn default_storage_version_entries_round_trip() {
    check_fixture("ddl-v0.10.2.wal", "v0.10.2", 17);
}

#[test]
fn latest_storage_version_entries_round_trip() {
    check_fixture("ddl-latest.wal", "latest", 17);
}
