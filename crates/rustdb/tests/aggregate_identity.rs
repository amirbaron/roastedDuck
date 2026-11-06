//! Independent frozen C++ public plans and values: aliases, bind data, filters,
//! DISTINCT, grouped and window aggregates. Identical results alone missed the
//! duplicate states; exact complete physical plans check actual aggregate sharing.
#![allow(
    clippy::unwrap_used,
    reason = "Frozen oracle fixtures and diagnostic assertions"
)]
use rustdb::{Database, Result};

#[test]
fn aggregate_sharing_matches_frozen_cpp_plans_and_values() -> Result<()> {
    let mut connection = Database::open_in_memory()?.connect()?;
    connection.query("CREATE TABLE src AS SELECT i%3 AS g,i AS v FROM range(20)r(i)")?;
    let mut plan_differences = Vec::new();
    for line in include_str!("data/aggregate_identity_1.5.6.tsv").lines() {
        let fields: Vec<_> = line.split('\t').collect();
        let [name, sql, expected_plan, expected_rows, expected_names] = fields.as_slice() else {
            panic!("invalid aggregate oracle fixture")
        };
        let result = connection.query(*sql)?;
        let rows: Vec<Vec<String>> = (0..result.row_count())
            .map(|row| {
                (0..result.column_count())
                    .map(|column| {
                        let value = result.value(column, row);
                        if value.is_null() {
                            "\0".to_owned()
                        } else {
                            value.to_varchar().unwrap()
                        }
                    })
                    .collect()
            })
            .collect();
        let expected: Vec<Vec<&str>> = expected_rows
            .split('\x1e')
            .map(|row| row.split('\x1f').collect())
            .collect();
        assert_eq!(rows, expected, "{name} values");
        let names: Vec<&[u8]> = result
            .names()
            .iter()
            .map(rustdb_common::ByteString::as_bytes)
            .collect();
        assert_eq!(
            names,
            expected_names
                .split('\x1f')
                .map(str::as_bytes)
                .collect::<Vec<_>>(),
            "{name} column names"
        );
        let plan = connection
            .query(format!("EXPLAIN(FORMAT JSON){sql}"))?
            .value(1, 0);
        let text = plan.as_str().unwrap().replace('\'', "''");
        let canonical = connection.query(format!("SELECT json('{text}')"))?.value(0, 0);
        if canonical.as_str() != Some(*expected_plan) {
            eprintln!(
                "{name} plan\nACTUAL {}\nEXPECTED {expected_plan}",
                canonical.as_str().unwrap()
            );
            plan_differences.push(name.to_string());
        }
    }
    assert!(
        plan_differences.is_empty(),
        "different complete plans: {plan_differences:?}"
    );
    Ok(())
}
