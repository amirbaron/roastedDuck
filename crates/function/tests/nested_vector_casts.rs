//! Nested casts over multi-row flat and dictionary vectors (expectations from DuckDB 1.5.6).
#![allow(clippy::unwrap_used)] // Test helpers unwrap like the tests themselves.

use rustdb_function::cast::{CastFunctionSet, CastParameters, cast_vector};
use rustdb_types::{LogicalType, SelectionVector, Value, Vector};

fn text(s: &str) -> Value {
    Value::varchar(s)
}

fn ints(values: &[i32]) -> Value {
    Value::list(
        LogicalType::INTEGER,
        values.iter().copied().map(Value::integer).collect(),
    )
}

/// Casts `values` as one flat vector and as a (reversed) dictionary over it, checks that
/// both agree and returns the VARCHAR text of every row.
fn cast_rows(values: &[Value], target: &LogicalType, try_cast: bool) -> Vec<String> {
    let source_type = values[0].logical_type().clone();
    let mut flat = Vector::flat(source_type, values.len());
    for (row, value) in values.iter().enumerate() {
        flat.set_value(row, value).unwrap();
    }
    let reversed: Vec<u32> = (0..values.len() as u32).rev().collect();
    let dictionary = flat.slice(&SelectionVector::new(reversed));
    let mut rendered = Vec::new();
    for (source, reverse) in [(&flat, false), (&dictionary, true)] {
        let mut params = if try_cast {
            CastParameters::try_cast()
        } else {
            CastParameters::new()
        };
        let result = cast_vector(&CastFunctionSet::new(), source, target, values.len(), &mut params).unwrap();
        let mut rows: Vec<String> = (0..values.len())
            .map(|row| result.get_value(row).to_varchar().unwrap())
            .collect();
        if reverse {
            rows.reverse();
        }
        rendered.push(rows);
    }
    assert_eq!(rendered[0], rendered[1], "flat and dictionary inputs disagree");
    rendered.swap_remove(0)
}

#[test]
fn varchar_to_lists() {
    let int_list = LogicalType::list(LogicalType::INTEGER);
    let texts = [
        text("[1, 2]"),
        Value::null(LogicalType::VARCHAR),
        text("[x]"),
        text("[]"),
        text("[3,NULL]"),
    ];
    assert_eq!(
        cast_rows(&texts, &int_list, true),
        ["[1, 2]", "NULL", "[NULL]", "[]", "[3, NULL]"]
    );
}

#[test]
fn lists_to_text_and_arrays() {
    let int_list = LogicalType::list(LogicalType::INTEGER);
    let lists = [ints(&[1]), Value::null(int_list), ints(&[2, 3]), ints(&[])];
    assert_eq!(
        cast_rows(&lists, &LogicalType::VARCHAR, false),
        ["[1]", "NULL", "[2, 3]", "[]"]
    );
    let array1 = LogicalType::array(LogicalType::BIGINT, 1);
    assert_eq!(cast_rows(&lists, &array1, true), ["[1]", "NULL", "NULL", "NULL"]);
}

#[test]
fn structs_to_text_and_maps() {
    let row_type = LogicalType::struct_type(vec![
        ("a".to_owned(), LogicalType::INTEGER),
        ("b".to_owned(), LogicalType::VARCHAR),
    ]);
    let rows = [
        Value::struct_value(vec![
            ("a".to_owned(), Value::integer(1)),
            ("b".to_owned(), text("x")),
        ]),
        Value::null(row_type),
        Value::struct_value(vec![
            ("a".to_owned(), Value::integer(2)),
            ("b".to_owned(), text("y,z")),
        ]),
    ];
    assert_eq!(
        cast_rows(&rows, &LogicalType::VARCHAR, false),
        ["{'a': 1, 'b': x}", "NULL", "{'a': 2, 'b': 'y,z'}"]
    );
    let map = LogicalType::map(LogicalType::VARCHAR, LogicalType::VARCHAR);
    assert_eq!(
        cast_rows(&rows, &map, false),
        ["{a=1, b=x}", "NULL", "{a=2, b='y,z'}"]
    );
}
