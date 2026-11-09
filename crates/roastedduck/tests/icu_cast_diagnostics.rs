//! Exact ICU/core cast diagnostics captured from DuckDB 1.5.6.
#![allow(clippy::unwrap_used)]

use roastedduck::{Connection, Database};

fn connection() -> Connection {
    let mut connection = Database::open_in_memory().unwrap().connect().unwrap();
    connection.query("LOAD icu; SET TimeZone='UTC'").unwrap();
    connection
}

#[test]
fn icu_thrown_conversion_errors_do_not_acquire_query_context() {
    let mut connection = connection();
    let cases = [
        (
            "'294247-01-10 04:00:54.775806' AS VARCHAR",
            "TIMESTAMPTZ",
            "Conversion Error: ICU date overflows timestamp range",
        ),
        (
            "'294247-01-10 04:00:54.775806' AS TIMESTAMP",
            "TIMESTAMPTZ",
            "Conversion Error: ICU date overflows timestamp range",
        ),
        (
            "'0001-01-01 00:00:00' AS TIMESTAMPTZ",
            "TIMESTAMP_NS",
            "Conversion Error: Could not convert Timestamp(US) to Timestamp(NS)",
        ),
        (
            "'290309-12-22 (BC) 00:00:00' AS TIMESTAMPTZ",
            "TIMESTAMP_NS",
            "Conversion Error: Could not convert Timestamp(US) to Timestamp(NS)",
        ),
        (
            "'1677-09-21 00:12:43.145225' AS TIMESTAMPTZ",
            "TIMESTAMP_NS",
            "Conversion Error: Date out of range in timestamp_ns conversion",
        ),
        (
            "'1677-09-21 00:12:43.145224' AS TIMESTAMPTZ",
            "TIMESTAMP_NS",
            "Conversion Error: Could not convert Timestamp(US) to Timestamp(NS)",
        ),
        (
            "'2262-04-11 23:47:16.854776' AS TIMESTAMPTZ",
            "TIMESTAMP_NS",
            "Conversion Error: Could not convert Timestamp(US) to Timestamp(NS)",
        ),
    ];
    for (source, target, expected) in cases {
        for operation in ["CAST", "TRY_CAST"] {
            let sql = format!("SELECT hex(CAST({operation}(CAST({source}) AS {target}) AS VARCHAR))");
            assert_eq!(connection.query(&sql).unwrap_err().to_string(), expected, "{sql}");
        }
    }
}

#[test]
fn icu_date_overflow_uses_physical_storage_type_names() {
    let mut connection = connection();
    for date in ["5877641-06-25 (BC)", "5877642-06-25 (BC)", "5881580-07-10"] {
        for operation in ["CAST", "TRY_CAST"] {
            let sql =
                format!("SELECT hex(CAST({operation}(CAST('{date}' AS DATE) AS TIMESTAMPTZ) AS VARCHAR))");
            assert_eq!(
                connection.query(&sql).unwrap_err().to_string(),
                format!(
                    "Invalid Input Error: Type INT32 with value {date} can't be cast to the destination type INT64"
                ),
                "{sql}"
            );
        }
    }
}

#[test]
fn reported_constant_cast_failure_retains_context_and_try_cast_null() {
    let mut connection = connection();
    assert_eq!(
        connection
            .query("SELECT CAST('not an integer' AS INTEGER)")
            .unwrap_err()
            .to_string(),
        "Conversion Error: Could not convert string 'not an integer' to INT32\n\n\
         LINE 1: SELECT CAST('not an integer' AS INTEGER)\n               ^"
    );
    let result = connection
        .query("SELECT TRY_CAST('not an integer' AS INTEGER)")
        .unwrap();
    assert!(result.value(0, 0).is_null());
}

#[test]
fn reported_vector_cast_failure_retains_alias_and_context() {
    let mut connection = connection();
    assert_eq!(
        connection
            .query("SELECT CAST(i AS INTEGER) FROM (VALUES ('1'),('bad'))t(i)")
            .unwrap_err()
            .to_string(),
        "Conversion Error: Could not convert string 'bad' to INT32 when casting from source column i\n\n\
         LINE 1: SELECT CAST(i AS INTEGER) FROM (VALUES ('1'),('bad'))t(i)\n               ^"
    );
    let result = connection
        .query("SELECT TRY_CAST(i AS INTEGER) FROM (VALUES ('1'),('bad'))t(i)")
        .unwrap();
    assert_eq!(result.value(0, 0).to_varchar().unwrap(), "1");
    assert!(result.value(0, 1).is_null());
}

#[test]
fn interval_reported_failures_retain_query_context() {
    let mut connection = connection();
    let cases: &[(&str, &[u8])] = &[
        ("SELECT hex(CAST(CAST(CAST('' AS VARCHAR) AS INTERVAL) AS VARCHAR))", b"Conversion Error: Could not convert string '' to INTERVAL\n\nLINE 1: SELECT hex(CAST(CAST(CAST('' AS VARCHAR) AS INTERVAL) AS VARCHAR))\n                        ^"),
    ];
    for (sql, expected) in cases {
        assert_eq!(connection.query(sql).unwrap_err().to_bytes(), *expected, "{sql}");
    }
}

#[test]
fn bit_numeric_overflow_retains_context_even_under_try_cast() {
    let mut connection = connection();
    let cases: &[(&str, &[u8])] = &[
        ("SELECT hex(CAST(CAST(CAST('111111111' AS BIT) AS BOOLEAN) AS VARCHAR))", b"Conversion Error: Bitstring doesn't fit inside of UINT8\n\nLINE 1: SELECT hex(CAST(CAST(CAST('111111111' AS BIT) AS BOOLEAN) AS VARCHAR))\n                        ^"),
        ("SELECT hex(CAST(TRY_CAST(CAST('111111111' AS BIT) AS BOOLEAN) AS VARCHAR))", b"Conversion Error: Bitstring doesn't fit inside of UINT8\n\nLINE 1: SELECT hex(CAST(TRY_CAST(CAST('111111111' AS BIT) AS BOOLEAN) AS VARCHAR))\n                        ^"),
    ];
    for (sql, expected) in cases {
        assert_eq!(connection.query(sql).unwrap_err().to_bytes(), *expected, "{sql}");
    }
}

#[test]
fn string_bit_failures_do_not_acquire_query_context() {
    let mut connection = connection();
    let cases: &[(&str, &[u8])] = &[
        (
            "SELECT hex(CAST(CAST(CAST('' AS VARCHAR) AS BIT) AS VARCHAR))",
            b"Conversion Error: Cannot cast empty string to BIT",
        ),
        (
            "SELECT hex(CAST(CAST(CAST('a' AS ENUM('a', 'b', 'c')) AS BIT) AS VARCHAR))",
            b"Conversion Error: Invalid character encountered in string -> bit conversion: 'a'",
        ),
    ];
    for (sql, expected) in cases {
        assert_eq!(connection.query(sql).unwrap_err().to_bytes(), *expected, "{sql}");
    }
}

#[test]
fn blob_uuid_errors_preserve_non_utf8_bytes() {
    let mut connection = connection();
    let cases: &[(&str, &[u8])] = &[
        ("SELECT hex(CAST(CAST(CAST('\\xAA\\xBB' AS BLOB) AS UUID) AS VARCHAR))", b"Conversion Error: Could not convert string '\xaa\xbb' to INT128\n\nLINE 1: SELECT hex(CAST(CAST(CAST('\\xAA\\xBB' AS BLOB) AS UUID) AS VARCHAR))\n                        ^"),
        ("SELECT hex(CAST(CAST(CAST('\\x00\\x01\\xFF' AS BLOB) AS UUID) AS VARCHAR))", b"Conversion Error: Could not convert string '\\0\x01\xff' to INT128\n\nLINE 1: SELECT hex(CAST(CAST(CAST('\\x00\\x01\\xFF' AS BLOB) AS UUID) AS VARCHAR))\n                        ^"),
        ("SELECT hex(CAST(CAST(CAST('\\xaa' AS BLOB) AS UUID) AS VARCHAR))", b"Conversion Error: Could not convert string '\xaa' to INT128\n\nLINE 1: SELECT hex(CAST(CAST(CAST('\\xaa' AS BLOB) AS UUID) AS VARCHAR))\n                        ^"),
    ];
    for (sql, expected) in cases {
        assert_eq!(connection.query(sql).unwrap_err().to_bytes(), *expected, "{sql}");
    }
}

#[test]
fn invalid_unicode_bit_diagnostics_preserve_first_source_byte() {
    let mut connection = connection();
    let cases: &[(&str, &[u8])] = &[
        (
            "SELECT hex(CAST(CAST(CAST('ü' AS VARCHAR) AS BIT) AS VARCHAR))",
            b"Conversion Error: Invalid character encountered in string -> bit conversion: '\xc3'",
        ),
        (
            "SELECT hex(CAST(CAST(CAST('😀' AS VARCHAR) AS BIT) AS VARCHAR))",
            b"Conversion Error: Invalid character encountered in string -> bit conversion: '\xf0'",
        ),
    ];
    for (sql, expected) in cases {
        assert_eq!(connection.query(sql).unwrap_err().to_bytes(), *expected, "{sql}");
    }
}
