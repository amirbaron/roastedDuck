//! Nested, ENUM, BLOB, BIT and UUID casts, with expectations taken from DuckDB 1.5.6.
#![allow(clippy::unwrap_used)] // Test helpers unwrap like the tests themselves.

use rustdb_function::cast::{CastFunctionSet, CastParameters, ValueCast};
use rustdb_types::{LogicalType, Value};

/// The VARCHAR text of `value` cast to `target`, or the error message.
fn cast(value: &Value, target: &LogicalType, try_cast: bool) -> String {
    let mut params = if try_cast {
        CastParameters::try_cast()
    } else {
        CastParameters::new()
    };
    match value.cast_with(&CastFunctionSet::new(), target, &mut params) {
        Ok(result) => result.to_varchar().unwrap_or_else(|e| e.to_string()),
        Err(error) => error.to_string(),
    }
}

fn text(s: &str) -> Value {
    Value::varchar(s)
}

/// `CAST(text AS ty)` as a value of `ty`, for building sources.
fn typed(s: &str, ty: &LogicalType) -> Value {
    text(s)
        .cast_with(&CastFunctionSet::new(), ty, &mut CastParameters::new())
        .unwrap()
}

fn ints(values: &[i32]) -> Value {
    Value::list(
        LogicalType::INTEGER,
        values.iter().copied().map(Value::integer).collect(),
    )
}

fn ab_struct() -> LogicalType {
    LogicalType::struct_type(vec![
        ("a".to_owned(), LogicalType::INTEGER),
        ("b".to_owned(), LogicalType::VARCHAR),
    ])
}

#[test]
fn varchar_to_list() {
    let int_list = LogicalType::list(LogicalType::INTEGER);
    assert_eq!(cast(&text("[1, NULL, 3]"), &int_list, false), "[1, NULL, 3]");
    assert_eq!(
        cast(
            &text("[a, 'b,c', NULL, \"d\"]"),
            &LogicalType::list(LogicalType::VARCHAR),
            false
        ),
        "[a, 'b,c', NULL, d]"
    );
    assert_eq!(
        cast(
            &text("[[1, 2], [3]]"),
            &LogicalType::list(int_list.clone()),
            false
        ),
        "[[1, 2], [3]]"
    );
    assert_eq!(
        cast(&text("[1, x]"), &int_list, false),
        "Conversion Error: Could not convert string 'x' to INT32"
    );
    assert_eq!(cast(&text("[1, x]"), &int_list, true), "[1, NULL]");
    assert_eq!(
        cast(&text("[1, 2"), &int_list, false),
        "Conversion Error: Type VARCHAR with value '[1, 2' can't be cast to the destination type INTEGER[]"
    );
    assert_eq!(cast(&text("[1, 2"), &int_list, true), "NULL");
}

#[test]
fn varchar_to_struct_map_array() {
    let row = ab_struct();
    assert_eq!(
        cast(&text("{a: 1, b: hello}"), &row, false),
        "{'a': 1, 'b': hello}"
    );
    assert_eq!(cast(&text("{b: x}"), &row, false), "{'a': NULL, 'b': x}");
    assert_eq!(cast(&text("(1, x)"), &row, false), "{'a': 1, 'b': x}");
    assert_eq!(
        cast(&text("{c: 1}"), &row, false),
        "Conversion Error: Type VARCHAR with value '{c: 1}' can't be cast to the destination type STRUCT(a INTEGER, b VARCHAR)"
    );
    let map = LogicalType::map(LogicalType::INTEGER, LogicalType::VARCHAR);
    assert_eq!(cast(&text("{1=a, 2=NULL}"), &map, false), "{1=a, 2=NULL}");
    assert_eq!(
        cast(&text("{1=a, 1=b}"), &map, false),
        "Invalid Input Error: Map keys must be unique."
    );
    assert_eq!(
        cast(&text("{1=a, 1=b}"), &map, true),
        "Invalid Input Error: Map keys must be unique."
    );
    assert_eq!(
        cast(&text("{NULL=a}"), &map, false),
        "Conversion Error: Type VARCHAR with value '{NULL=a}' can't be cast to the destination type MAP(INTEGER, VARCHAR)"
    );
    assert_eq!(
        cast(&text("{x=a}"), &map, false),
        "Conversion Error: Could not convert string 'x' to INT32"
    );
    assert_eq!(cast(&text("{x=a}"), &map, true), "NULL");
    let array = LogicalType::array(LogicalType::INTEGER, 2);
    assert_eq!(cast(&text("[1, 2]"), &array, false), "[1, 2]");
    assert_eq!(
        cast(&text("[1, 2, 3]"), &array, false),
        "Conversion Error: Type VARCHAR with value '[1, 2, 3]' can't be cast to the destination type INTEGER[2], the size of the array must match the destination type"
    );
    assert_eq!(cast(&text("[1, 2, 3]"), &array, true), "NULL");
}

#[test]
fn list_and_array_casts() {
    assert_eq!(
        cast(&ints(&[1, 2, 3]), &LogicalType::list(LogicalType::VARCHAR), false),
        "[1, 2, 3]"
    );
    let tiny = LogicalType::list(LogicalType::TINYINT);
    assert_eq!(
        cast(&ints(&[1, 2, 300]), &tiny, false),
        "Conversion Error: Type INT32 with value 300 can't be cast because the value is out of range for the destination type INT8"
    );
    assert_eq!(cast(&ints(&[1, 2, 300]), &tiny, true), "[1, 2, NULL]");
    let array2 = LogicalType::array(LogicalType::INTEGER, 2);
    assert_eq!(cast(&ints(&[1, 2]), &array2, false), "[1, 2]");
    assert_eq!(
        cast(&ints(&[1, 2, 3]), &array2, false),
        "Conversion Error: Cannot cast list with length 3 to array with length 2"
    );
    assert_eq!(cast(&ints(&[1, 2, 3]), &array2, true), "NULL");
    let array = Value::array(LogicalType::INTEGER, vec![Value::integer(1), Value::integer(2)]);
    assert_eq!(
        cast(&array, &LogicalType::list(LogicalType::BIGINT), false),
        "[1, 2]"
    );
    let array3 = LogicalType::array(LogicalType::INTEGER, 3);
    assert_eq!(
        cast(&array, &array3, false),
        "Conversion Error: Cannot cast array of size 2 to array of size 3"
    );
    assert_eq!(cast(&array, &array3, true), "NULL");
    assert_eq!(
        cast(
            &Value::list(LogicalType::INTEGER, vec![]),
            &LogicalType::VARCHAR,
            false
        ),
        "[]"
    );
}

#[test]
fn struct_and_map_casts() {
    let row = Value::struct_value(vec![
        ("a".to_owned(), Value::integer(1)),
        ("b".to_owned(), text("x")),
    ]);
    let swapped = LogicalType::struct_type(vec![
        ("b".to_owned(), LogicalType::VARCHAR),
        ("a".to_owned(), LogicalType::BIGINT),
    ]);
    assert_eq!(cast(&row, &swapped, false), "{'b': x, 'a': 1}");
    let partial = LogicalType::struct_type(vec![
        ("a".to_owned(), LogicalType::BIGINT),
        ("c".to_owned(), LogicalType::VARCHAR),
    ]);
    assert_eq!(cast(&row, &partial, false), "{'a': 1, 'c': NULL}");
    let unrelated = LogicalType::struct_type(vec![("c".to_owned(), LogicalType::BIGINT)]);
    assert_eq!(
        cast(&row, &unrelated, false),
        "Binder Error: STRUCT to STRUCT cast must have at least one matching member"
    );
    let unnamed_type = LogicalType::struct_type(vec![
        (String::new(), LogicalType::INTEGER),
        (String::new(), LogicalType::VARCHAR),
    ]);
    let unnamed = Value::from_children(unnamed_type, vec![Value::integer(1), text("x")]);
    let named = LogicalType::struct_type(vec![
        ("a".to_owned(), LogicalType::BIGINT),
        ("b".to_owned(), LogicalType::VARCHAR),
    ]);
    assert_eq!(cast(&unnamed, &named, false), "{'a': 1, 'b': x}");
    let numbers = Value::struct_value(vec![
        ("a".to_owned(), Value::integer(1)),
        ("b".to_owned(), Value::integer(2)),
    ]);
    let map = LogicalType::map(LogicalType::VARCHAR, LogicalType::BIGINT);
    assert_eq!(cast(&numbers, &map, false), "{a=1, b=2}");
    let unnamed_numbers = Value::from_children(
        LogicalType::struct_type(vec![
            (String::new(), LogicalType::INTEGER),
            (String::new(), LogicalType::INTEGER),
        ]),
        vec![Value::integer(1), Value::integer(2)],
    );
    assert_eq!(
        cast(&unnamed_numbers, &map, false),
        "Mismatch Type Error: Type STRUCT(INTEGER, INTEGER) does not match with MAP(VARCHAR, BIGINT). Cannot cast unnamed STRUCTs to MAP"
    );
    let source_map = Value::map(
        LogicalType::VARCHAR,
        LogicalType::INTEGER,
        vec![(text("a"), Value::integer(1))],
    );
    assert_eq!(
        cast(
            &source_map,
            &LogicalType::map(LogicalType::VARCHAR, LogicalType::VARCHAR),
            false
        ),
        "{a=1}"
    );
    let nested = Value::struct_value(vec![("k".to_owned(), ints(&[1, 2]))]);
    assert_eq!(cast(&nested, &LogicalType::VARCHAR, false), "{'k': [1, 2]}");
}

#[test]
fn union_casts() {
    let union = LogicalType::union_type(vec![
        ("i".to_owned(), LogicalType::INTEGER),
        ("s".to_owned(), LogicalType::VARCHAR),
    ]);
    assert_eq!(cast(&Value::integer(1), &union, false), "1");
    assert_eq!(cast(&text("x"), &union, false), "x");
    assert_eq!(cast(&Value::null(LogicalType::INTEGER), &union, false), "NULL");
    assert_eq!(
        cast(&Value::decimal(15, 2, 1), &union, false),
        "Conversion Error: Type DECIMAL(2,1) can't be cast as UNION(i INTEGER, s VARCHAR). DECIMAL(2,1) can't be implicitly cast to any of the union member types: INTEGER, VARCHAR"
    );
    let ambiguous = LogicalType::union_type(vec![
        ("a".to_owned(), LogicalType::BIGINT),
        ("b".to_owned(), LogicalType::BIGINT),
    ]);
    assert_eq!(
        cast(&Value::integer(1), &ambiguous, false),
        "Conversion Error: Type INTEGER can't be cast as UNION(a BIGINT, b BIGINT). The cast is ambiguous, multiple possible members in target: 'a (BIGINT)', 'b (BIGINT)'. Disambiguate the target type by using the 'union_value(<tag> := <arg>)' function to promote the source value to a single member union before casting."
    );
    let value = Value::union_value(union.clone(), 0, Value::integer(5));
    let reordered = LogicalType::union_type(vec![
        ("s".to_owned(), LogicalType::VARCHAR),
        ("i".to_owned(), LogicalType::BIGINT),
    ]);
    assert_eq!(cast(&value, &reordered, false), "5");
    let narrower = LogicalType::union_type(vec![("i".to_owned(), LogicalType::BIGINT)]);
    assert_eq!(
        cast(&value, &narrower, false),
        "Conversion Error: Type UNION(i INTEGER, s VARCHAR) can't be cast as UNION(i BIGINT). The member 's' is not present in target union"
    );
}

#[test]
fn enum_casts() {
    let ab = LogicalType::enum_type(vec!["a".to_owned(), "b".to_owned()]);
    assert_eq!(cast(&text("b"), &ab, false), "b");
    assert_eq!(
        cast(&text("c"), &ab, false),
        "Conversion Error: Could not convert string 'c' to UINT8"
    );
    assert_eq!(cast(&text("c"), &ab, true), "NULL");
    let bc = LogicalType::enum_type(vec!["b".to_owned(), "c".to_owned()]);
    assert_eq!(cast(&typed("b", &ab), &bc, false), "b");
    assert_eq!(
        cast(&typed("a", &ab), &bc, false),
        "Conversion Error: Type UINT8 with value 0 can't be cast because the value is out of range for the destination type UINT8"
    );
    let digits = LogicalType::enum_type(vec!["1".to_owned(), "b".to_owned()]);
    assert_eq!(cast(&typed("1", &digits), &LogicalType::INTEGER, false), "1");
    assert_eq!(
        cast(&typed("b", &digits), &LogicalType::INTEGER, false),
        "Conversion Error: Could not convert string 'b' to INT32"
    );
    assert_eq!(
        cast(
            &Value::integer(1),
            &LogicalType::enum_type(vec!["a".to_owned()]),
            false
        ),
        "Conversion Error: Unimplemented type for cast (INTEGER -> ENUM('a'))"
    );
}

#[test]
fn blob_bit_uuid_casts() {
    let blob = typed("\\xAA\\x01ab", &LogicalType::BLOB);
    assert_eq!(cast(&blob, &LogicalType::VARCHAR, false), "\\xAA\\x01ab");
    let empty = Value::blob(b"");
    assert_eq!(
        cast(&empty, &LogicalType::BIT, false),
        "Conversion Error: Cannot cast empty BLOB to BIT"
    );
    assert_eq!(
        cast(&empty, &LogicalType::BIT, true),
        "Conversion Error: Cannot cast empty BLOB to BIT"
    );
    assert_eq!(
        cast(&typed("0101", &LogicalType::BIT), &LogicalType::INTEGER, false),
        "5"
    );
    let long_bits = typed("010101010101010101", &LogicalType::BIT);
    assert_eq!(
        cast(&long_bits, &LogicalType::TINYINT, true),
        "Conversion Error: Bitstring doesn't fit inside of INT8"
    );
    let uuid = typed("a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11", &LogicalType::UUID);
    assert_eq!(
        cast(&uuid, &LogicalType::BLOB, false),
        "\\xA0\\xEE\\xBC\\x99\\x9C\\x0BN\\xF8\\xBBmk\\xB9\\xBD8\\x0A\\x11"
    );
    assert_eq!(
        cast(&uuid, &LogicalType::INTEGER, false),
        "Conversion Error: Unimplemented type for cast (UUID -> INTEGER)"
    );
    assert_eq!(
        cast(&text("x"), &LogicalType::UUID, false),
        "Conversion Error: Could not convert string 'x' to INT128"
    );
}
