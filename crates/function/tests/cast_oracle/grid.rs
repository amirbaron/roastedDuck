//! The case grid: edge-case source values per type and the VARCHAR inputs per target.
//!
//! Generation runs in two phases. Phase 1 casts VARCHAR texts to every target (this
//! covers the VARCHAR -> T casts and validates the typed source literals). Phase 2 casts
//! every typed source value whose literal DuckDB accepted (plus NULL) to every target.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};

use super::oracle::{Case, Record};
use super::types::{self, DECIMALS, Family, GridType};

/// Integer boundaries: every type's min/max, one past them, and the boundaries of other
/// types (narrowing, float precision, decimal widths).
const INTEGER_POOL: &[&str] = &[
    "0",
    "1",
    "-1",
    "7",
    "-7",
    "42",
    "-42",
    "99",
    "-99",
    "100",
    "-100",
    "127",
    "-127",
    "128",
    "-128",
    "-129",
    "255",
    "-255",
    "256",
    "999",
    "-999",
    "1000",
    "-1000",
    "9999",
    "-9999",
    "10000",
    "32767",
    "-32767",
    "32768",
    "-32768",
    "-32769",
    "65535",
    "65536",
    "99999",
    "-99999",
    "999999",
    "16777216",
    "16777217",
    "-16777217",
    "999999999",
    "-999999999",
    "1000000000",
    "2147483647",
    "-2147483647",
    "2147483648",
    "-2147483648",
    "-2147483649",
    "4294967295",
    "4294967296",
    "9007199254740993",
    "999999999999999999",
    "-999999999999999999",
    "1000000000000000000",
    "9223372036854775807",
    "-9223372036854775807",
    "9223372036854775808",
    "-9223372036854775808",
    "-9223372036854775809",
    "18446744073709551615",
    "18446744073709551616",
    "99999999999999999999999999999999999999",
    "-99999999999999999999999999999999999999",
    "100000000000000000000000000000000000000",
    "170141183460469231731687303715884105727",
    "-170141183460469231731687303715884105727",
    "170141183460469231731687303715884105728",
    "-170141183460469231731687303715884105728",
    "-170141183460469231731687303715884105729",
    "340282366920938463463374607431768211455",
    "340282366920938463463374607431768211456",
];

/// `(min, max)` of each integer type.
fn integer_range(sql: &str) -> (&'static str, &'static str) {
    match sql {
        "TINYINT" => ("-128", "127"),
        "SMALLINT" => ("-32768", "32767"),
        "INTEGER" => ("-2147483648", "2147483647"),
        "BIGINT" => ("-9223372036854775808", "9223372036854775807"),
        "HUGEINT" => (
            "-170141183460469231731687303715884105728",
            "170141183460469231731687303715884105727",
        ),
        "UTINYINT" => ("0", "255"),
        "USMALLINT" => ("0", "65535"),
        "UINTEGER" => ("0", "4294967295"),
        "UBIGINT" => ("0", "18446744073709551615"),
        "UHUGEINT" => ("0", "340282366920938463463374607431768211455"),
        _ => unreachable!("not an integer type: {sql}"),
    }
}

/// Compares two decimal integer strings numerically.
fn compare_integers(a: &str, b: &str) -> Ordering {
    fn split(s: &str) -> (bool, &str) {
        s.strip_prefix('-').map_or((false, s), |digits| (true, digits))
    }
    let ((a_negative, a), (b_negative, b)) = (split(a), split(b));
    let magnitude = a.len().cmp(&b.len()).then_with(|| a.cmp(b));
    match (a_negative, b_negative) {
        (false, false) => magnitude,
        (true, true) => magnitude.reverse(),
        (false, true) => Ordering::Greater,
        (true, false) => Ordering::Less,
    }
}

/// The pool values within the type's range, plus the nearest ones just outside it.
fn integer_sources(sql: &str) -> Vec<String> {
    let (min, max) = integer_range(sql);
    let below = INTEGER_POOL
        .iter()
        .filter(|v| compare_integers(v, min).is_lt())
        .max_by(|a, b| compare_integers(a, b));
    let above = INTEGER_POOL
        .iter()
        .filter(|v| compare_integers(v, max).is_gt())
        .min_by(|a, b| compare_integers(a, b));
    INTEGER_POOL
        .iter()
        .filter(|v| {
            (compare_integers(v, min).is_ge() && compare_integers(v, max).is_le())
                || Some(*v) == below
                || Some(*v) == above
        })
        .map(|v| (*v).to_owned())
        .collect()
}

const FLOAT_SOURCES: &[&str] = &[
    "0",
    "-0",
    "1",
    "-1",
    "0.1",
    "0.5",
    "-0.5",
    "1.5",
    "2.5",
    "-2.5",
    "0.05",
    "0.15",
    "99.95",
    "123.456",
    "-123.456",
    "0.001",
    "1e-5",
    "1e-10",
    "1e10",
    "12345.6789",
    "127.4",
    "127.5",
    "127.6",
    "-128.5",
    "-128.6",
    "255.5",
    "65535.5",
    "16777217",
    "2147483647.5",
    "2147483648",
    "-2147483648.5",
    "4294967295.5",
    "9007199254740993",
    "9.223372036854776e18",
    "1.8446744073709552e19",
    "1e38",
    "-1e38",
    "1.7014118346046923e38",
    "3.4028235e38",
    "-3.4028235e38",
    "3.402823669209385e38",
    "3.5e38",
    "1.17549435e-38",
    "1.401298464324817e-45",
    "1e-50",
    "1.7976931348623157e308",
    "-1.7976931348623157e308",
    "2.2250738585072014e-308",
    "5e-324",
    "inf",
    "-inf",
    "nan",
];

/// `10^-scale` as text (`"1"` for scale 0).
fn unit(scale: u8) -> String {
    match scale {
        0 => "1".to_owned(),
        s => format!("0.{}1", "0".repeat(usize::from(s) - 1)),
    }
}

/// Decimal sources: zero, the smallest unit, half a unit, the extremes and one past them,
/// and values on the boundaries of other types.
fn decimal_sources(width: u8, scale: u8) -> Vec<String> {
    let integer_digits = usize::from(width - scale);
    let fraction = "9".repeat(usize::from(scale));
    let max = match (integer_digits, scale) {
        (0, _) => format!("0.{fraction}"),
        (n, 0) => "9".repeat(n),
        (n, _) => format!("{}.{fraction}", "9".repeat(n)),
    };
    let past = format!("1{}", "0".repeat(integer_digits));
    let mut values = vec![
        "0".to_owned(),
        unit(scale),
        format!("-{}", unit(scale)),
        format!("0.{}5", "0".repeat(usize::from(scale))),
        max.clone(),
        format!("-{max}"),
        past.clone(),
        format!("-{past}"),
    ];
    values.extend(
        [
            "0.5",
            "-0.5",
            "1",
            "-1",
            "1.5",
            "2.5",
            "-2.5",
            "0.05",
            "0.15",
            "12.34",
            "99.95",
            "127",
            "127.5",
            "128",
            "-128.5",
            "255",
            "255.5",
            "256",
            "32767",
            "32767.5",
            "65535",
            "2147483647",
            "2147483648",
            "4294967295",
            "16777217",
            "9223372036854775807",
            "9223372036854775808",
            "18446744073709551615",
            "18446744073709551616",
            "123456789012345678901234567.89",
        ]
        .map(str::to_owned),
    );
    values
}

const TEMPORAL_DATE: &[&str] = &[
    "1970-01-01",
    "2000-02-29",
    "2024-02-29",
    "2001-02-29",
    "1900-02-29",
    "1969-12-31",
    "0001-01-01",
    "0001-01-01 (BC)",
    "0001-12-31 (BC)",
    "4714-11-24 (BC)",
    "5877641-06-25 (BC)",
    "5877642-06-25 (BC)",
    "5881580-07-10",
    "5881580-07-11",
    "9999-12-31",
    "infinity",
    "-infinity",
    "epoch",
    "2024-1-5",
    "2024/01/05",
    "20240105",
    " 2024-01-05 ",
    "2024-01-05T00:00:00",
    "2024-01-05 00:00:00",
    "2024-01-05 12:00:00",
    "2024-13-01",
    "2024-00-10",
    "2024-01-32",
    "2024-04-31",
    "99999-01-01",
    "2024-01",
];

const TEMPORAL_TIME: &[&str] = &[
    "00:00:00",
    "23:59:59",
    "24:00:00",
    "24:00:01",
    "12:34:56.1",
    "12:34:56.12",
    "12:34:56.123",
    "12:34:56.1234",
    "12:34:56.12345",
    "12:34:56.123456",
    "12:34:56.1234567",
    "12:34:56.12345678",
    "12:34:56.123456789",
    "23:59:59.999999",
    "23:59:59.9999995",
    "23:59:59.999999999",
    "12:34",
    "1:2:3",
    "25:00:00",
    "23:60:00",
    "23:59:60",
    "12:34:56+05",
    "12:34:56 ",
    "allballs",
];

const TEMPORAL_TIMETZ: &[&str] = &[
    "12:34:56+05",
    "12:34:56-07:30",
    "12:34:56+00",
    "00:00:00+15:59",
    "00:00:00+15:59:59",
    "00:00:00+16",
    "24:00:00-15:59",
    "12:34:56.123456+01",
    "12:34:56.123456789-01:30",
    "12:34:56",
    "12:34:56 UTC",
    "12:34:56Z",
    "12:34:56+5",
    "12:34:56+0530",
];

const TEMPORAL_TIMESTAMP: &[&str] = &[
    "1970-01-01 00:00:00",
    "2024-02-29 12:34:56.789",
    "2024-02-29T12:34:56",
    "2024-02-29 12:34:56Z",
    "2024-02-29 12:34:56+02",
    "2024-02-29 12:34:56-03:30",
    "2024-02-29 12:34:56 America/New_York",
    "2024-02-29 12:34:56 UTC",
    "2024-01-01 24:00:00",
    "2024-01-01 12:34:56.1",
    "2024-01-01 12:34:56.123456",
    "2024-01-01 12:34:56.1234567",
    "2024-01-01 12:34:56.123456789",
    "2024-01-01",
    "1969-12-31 23:59:59.999999",
    "0001-01-01 00:00:00",
    "0001-01-01 00:00:00 (BC)",
    "290309-12-22 (BC) 00:00:00",
    "290309-12-21 (BC) 23:59:59",
    "294247-01-10 04:00:54.775806",
    "294247-01-10 04:00:54.775807",
    "294247-01-10 04:00:54.775808",
    "1677-09-21 00:12:43.145225",
    "1677-09-21 00:12:43.145224",
    "2262-04-11 23:47:16.854775",
    "2262-04-11 23:47:16.854776",
    "infinity",
    "-infinity",
    "epoch",
    "2024-02-30 00:00:00",
    "2024-01-01 25:00:00",
    "2024-01-01T",
    "2024-01-01 12:34:56 +02",
];

const TEMPORAL_INTERVAL: &[&str] = &[
    "1 day",
    "1 year 2 months 3 days",
    "-1 day",
    "1 day ago",
    "@ 1 day ago",
    "01:02:03",
    "-01:02:03",
    "1 day 01:02:03.456",
    "P1Y2M3DT4H5M6S",
    "P1W",
    "PT0.5S",
    "2 weeks",
    "1 millennium",
    "1 century",
    "1 decade",
    "1 quarter",
    "1 microsecond",
    "1 millisecond",
    "1.5 days",
    "1.5 months",
    "1.5 years",
    "0 seconds",
    "2147483647 months",
    "2147483648 months",
    "-2147483648 days",
    "178956970 years",
    "178956971 years",
    "9223372036854775807 microseconds",
    "2562047788 hours",
    "106751991 days 04:00:54.775807",
    "1 days 2 days",
    "1",
    "1 fortnight",
    "+1 day",
];

const UUID_SOURCES: &[&str] = &[
    "00000000-0000-0000-0000-000000000000",
    "ffffffff-ffff-ffff-ffff-ffffffffffff",
    "a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11",
    "A0EEBC99-9C0B-4EF8-BB6D-6BB9BD380A11",
    "{a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11}",
    "a0eebc999c0b4ef8bb6d6bb9bd380a11",
    "a0ee-bc99-9c0b-4ef8-bb6d-6bb9-bd38-0a11",
    "80000000-0000-0000-0000-000000000000",
    "7fffffff-ffff-ffff-ffff-ffffffffffff",
    "00000000-0000-0000-0000-000000000001",
    "a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a1",
    "a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a111",
    "g0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11",
    "{a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11",
    " a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11",
];

const BLOB_SOURCES: &[&str] = &[
    "abc",
    "",
    "\\xAA\\xBB",
    "\\x00",
    "\\x00\\x01\\xFF",
    "a\\x00b",
    "\\\\x41",
    "\\x41\\x42",
    "\\xaa",
    "\\X41",
    "\\xGG",
    "\\x4",
    "\\",
    "1",
    "123",
    "true",
    "0101",
    "\u{fc}",
    "a b\tc",
];

const BIT_SOURCES: &[&str] = &[
    "0",
    "1",
    "0101",
    "10000000",
    "11111111",
    "00000001",
    "111111111",
    "1000000000000000",
    "11111111111111111111111111111111",
    "10000000000000000000000000000000",
    "1111111111111111111111111111111111111111111111111111111111111111",
    "0000000000000000000000000000000000000000000000000000000000000001",
    "00000000000000000000000000000000000000000000000000000000000000001",
    "",
    "2",
    "01a",
    " 01",
];

const NESTED_SOURCES: &[(&str, &[&str])] = &[
    (
        "INTEGER[]",
        &[
            "[]",
            "[1, 2, 3]",
            "[1, NULL, 3]",
            "[NULL]",
            "[-2147483648, 2147483647]",
            "[ 1 , 2 ]",
            "[2147483648]",
            "[1,2",
            "1,2",
            "[[1]]",
            "['1']",
            "[1.5]",
            "[a]",
        ],
    ),
    (
        "VARCHAR[]",
        &[
            "[]",
            "[a, b]",
            "['a,b', 'c']",
            "[1, NULL, 'a,b']",
            "[NULL]",
            "['NULL']",
            "[\"x\"]",
            "[ a , b ]",
            "['it''s']",
            "[a\\,b]",
            "[[1, 2]]",
            "[{'a': 1}]",
            "[a",
        ],
    ),
    (
        "INTEGER[2]",
        &["[1, 2]", "[NULL, 2]", "[1]", "[1, 2, 3]", "[]", "[1, x]"],
    ),
    (
        "STRUCT(a INTEGER, b VARCHAR)",
        &[
            "{'a': 1, 'b': x}",
            "{'a': NULL, 'b': NULL}",
            "{'b': 'y', 'a': 2}",
            "{a: 1, b: 'x,y'}",
            "{'a': 1}",
            "{'c': 1}",
            "(1, x)",
            "{'a': 1, 'b': x",
            "{}",
            "{'a': x, 'b': 1}",
        ],
    ),
    (
        "MAP(VARCHAR, INTEGER)",
        &[
            "{a=1, b=2}",
            "{}",
            "{a=NULL}",
            "{'k=1'=2}",
            "{a=1, a=2}",
            "{NULL=1}",
            "{a=x}",
            "{a:1}",
            "{a=1",
        ],
    ),
    ("UNION(i INTEGER, s VARCHAR)", &["1", "abc", "", "NULL"]),
];

const ENUM_SOURCES: &[&str] = &["a", "b", "c", "d", "A", " a", ""];

/// VARCHAR inputs sent to every target.
const GENERAL_TEXTS: &[&str] = &[
    "",
    " ",
    "NULL",
    "null",
    "abc",
    "1",
    "0",
    "-1",
    "1.5",
    "true",
    "\u{fc}",
    "\u{1F600}",
    "a\tb",
    "a\nb",
    "it's",
    "\"dq\"",
    "\\",
    "[1]",
    "{a=1}",
    "2024-01-01",
    "12:00:00",
    "1 day",
];

/// VARCHAR inputs for numeric and BOOLEAN targets: every number syntax DuckDB accepts or
/// rejects.
fn numeric_texts() -> Vec<String> {
    let fixed = [
        "-0",
        "+0",
        "+1",
        " 1",
        "1 ",
        " 1 ",
        "\t1\n",
        "\n1",
        "1_000",
        "1__000",
        "_1",
        "1_",
        "1_000.5",
        "1.000_5",
        "1e1_0",
        "0x1F",
        "0X1f",
        "0x",
        "0xFF",
        "0xFFFFFFFF",
        "0x7FFFFFFF",
        "0x80000000",
        "0xFFFFFFFFFFFFFFFF",
        "0x1_F",
        "-0x1",
        "0b101",
        "0B11",
        "0b",
        "0b2",
        "0o17",
        "1e3",
        "1E3",
        "1e+3",
        "1e-3",
        "1.5e2",
        "-1.5e-2",
        "1e",
        "e3",
        "1e3.5",
        ".5",
        "1.",
        ".",
        "-.5",
        "+.5",
        "-1.",
        "0.5",
        "2.5",
        "-2.5",
        "1.49999",
        "1.50001",
        "1,000",
        "1 000",
        "\u{967}\u{968}",
        "\u{ff11}",
        "\u{661}",
        "nan",
        "NaN",
        "-nan",
        "inf",
        "-inf",
        "+inf",
        "Infinity",
        "-Infinity",
        "+infinity",
        "INF",
        "infinity ",
        " -inf",
        "infinit",
        "1a",
        "a1",
        "12abc",
        "--1",
        "+-1",
        "-+1",
        "00001",
        "000",
        "-000",
        "0001.500",
        "1.0000000000000000000000000000000000000001",
        "1d",
        "1f",
        "1L",
        "$1",
        "1%",
        "t",
        "f",
        "1.5.6",
        "1e10",
        "1e308",
        "1e309",
        "-1e309",
        "2e-308",
        "1e-400",
        "4.9e-324",
        "3.4028235e38",
        "3.4028236e38",
        "3.5e38",
        "1e39",
        "-1e39",
        "1e38",
        "0x1.8p1",
        "0.000001",
        "0.0000001",
        "999.95",
        "999.94",
        "99.995",
        "0.05",
        "-0.05",
        "0.15",
        "0.25",
        "0.35",
        "-0.35",
        "12.345e1",
        "1.2345e2",
        "127.5",
        "-128.5",
        "255.5",
        "-0.5",
        "-0.4",
        "-1e-1",
    ];
    let mut texts: Vec<String> = fixed.iter().map(|t| (*t).to_owned()).collect();
    texts.extend(INTEGER_POOL.iter().map(|t| (*t).to_owned()));
    texts.extend([
        format!("1{}", "0".repeat(40)),
        "7".repeat(400),
        format!("0.{}1", "0".repeat(400)),
        format!("1.{}", "5".repeat(60)),
        "9".repeat(39),
        format!("-{}", "9".repeat(39)),
        format!("{}.{}", "9".repeat(20), "9".repeat(20)),
        format!("0x{}", "F".repeat(33)),
        format!("0b{}", "1".repeat(129)),
        format!("{}1", "0".repeat(100)),
    ]);
    texts
}

const BOOLEAN_TEXTS: &[&str] = &[
    "true", "TRUE", "True", "false", "FALSE", "t", "T", "f", "F", "yes", "no", "y", "n", "on", "off",
    " true", "true ", " t ", "1", "0", "2", "-1", "1.0", "0.0", "tru", "truee", "Yes", "YES", "NO",
];

/// The typed source texts of `ty` (before validation by the oracle).
fn source_texts(ty: &GridType) -> Vec<String> {
    let owned = |texts: &[&str]| texts.iter().map(|t| (*t).to_owned()).collect::<Vec<_>>();
    match (ty.family, ty.sql) {
        (Family::Boolean, _) => owned(&["true", "false"]),
        (Family::Integer, sql) => integer_sources(sql),
        (Family::Float, _) => owned(FLOAT_SOURCES),
        (Family::Decimal, _) => ty
            .ty
            .decimal_properties()
            .map(|(width, scale)| decimal_sources(width, scale))
            .unwrap_or_default(),
        (Family::Blob, _) => owned(BLOB_SOURCES),
        (Family::Bit, _) => owned(BIT_SOURCES),
        (Family::Uuid, _) => owned(UUID_SOURCES),
        (Family::Enum, _) => owned(ENUM_SOURCES),
        (Family::Temporal, "DATE") => owned(TEMPORAL_DATE),
        (Family::Temporal, "TIME" | "TIME_NS") => owned(TEMPORAL_TIME),
        (Family::Temporal, "TIMETZ") => owned(TEMPORAL_TIMETZ),
        (Family::Temporal, "INTERVAL") => owned(TEMPORAL_INTERVAL),
        (Family::Temporal, _) => owned(TEMPORAL_TIMESTAMP),
        (Family::Nested, sql) => NESTED_SOURCES
            .iter()
            .find(|(name, _)| *name == sql)
            .map(|(_, texts)| owned(texts))
            .unwrap_or_default(),
        (Family::Null | Family::Varchar, _) => Vec::new(),
    }
}

/// Every temporal text, sent to all temporal targets.
fn temporal_texts() -> Vec<String> {
    [
        TEMPORAL_DATE,
        TEMPORAL_TIME,
        TEMPORAL_TIMETZ,
        TEMPORAL_TIMESTAMP,
        TEMPORAL_INTERVAL,
    ]
    .concat()
    .into_iter()
    .map(str::to_owned)
    .collect()
}

/// The VARCHAR texts cast to `target` in phase 1.
fn varchar_texts(target: &GridType) -> Vec<String> {
    let mut texts: Vec<String> = GENERAL_TEXTS.iter().map(|t| (*t).to_owned()).collect();
    match target.family {
        Family::Boolean => texts.extend(BOOLEAN_TEXTS.iter().map(|t| (*t).to_owned())),
        Family::Integer | Family::Float | Family::Decimal => {
            texts.extend(numeric_texts());
            for (width, scale) in DECIMALS {
                texts.extend(decimal_sources(width, scale));
            }
            texts.extend(FLOAT_SOURCES.iter().map(|t| (*t).to_owned()));
        }
        Family::Temporal => texts.extend(temporal_texts()),
        Family::Nested | Family::Enum => {
            for (_, nested) in NESTED_SOURCES {
                texts.extend(nested.iter().map(|t| (*t).to_owned()));
            }
            texts.extend(ENUM_SOURCES.iter().map(|t| (*t).to_owned()));
        }
        Family::Blob | Family::Bit | Family::Uuid => {
            texts.extend(BIT_SOURCES.iter().map(|t| (*t).to_owned()));
            texts.extend(BLOB_SOURCES.iter().map(|t| (*t).to_owned()));
            texts.extend(UUID_SOURCES.iter().map(|t| (*t).to_owned()));
        }
        Family::Varchar | Family::Null => {}
    }
    texts.extend(source_texts(target));
    dedup(texts)
}

fn dedup(texts: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    texts
        .into_iter()
        .filter(|text| seen.insert(text.clone()))
        .collect()
}

/// Phase 1: VARCHAR texts (and a NULL VARCHAR) cast to every target.
pub fn varchar_cases() -> Vec<Case> {
    let varchar = types::get("VARCHAR");
    let mut cases = Vec::new();
    for target in types::targets() {
        cases.push(Case {
            source: varchar,
            target,
            text: None,
        });
        cases.extend(varchar_texts(target).into_iter().map(|text| Case {
            source: varchar,
            target,
            text: Some(text),
        }));
    }
    cases
}

/// Phase 2: every typed source value DuckDB accepts (per the phase 1 records), a NULL of
/// every type and the `NULL` literal, cast to every target.
pub fn typed_cases(phase1: &[Record]) -> Vec<Case> {
    let accepted: HashSet<(&str, &str)> = phase1
        .iter()
        .filter(|record| matches!(record.cast, super::oracle::Outcome::Value(_)))
        .filter_map(|record| Some((record.case.target.sql, record.case.text.as_deref()?)))
        .collect();
    let mut cases = Vec::new();
    for source in types::all() {
        if source.family == Family::Varchar {
            continue;
        }
        let mut texts: Vec<Option<String>> = vec![None];
        texts.extend(
            dedup(source_texts(source))
                .into_iter()
                .filter(|text| accepted.contains(&(source.sql, text.as_str())))
                .map(Some),
        );
        for text in texts {
            for target in types::targets() {
                cases.push(Case {
                    source,
                    target,
                    text: text.clone(),
                });
            }
        }
    }
    cases
}

/// Cases kept per pair with a VARCHAR source and per other pair. Typed and pending pairs
/// are only sampled where DuckDB's cast exists (some case yields a value).
const SUBSET_VARCHAR: usize = 16;
const SUBSET_TYPED: usize = 1;

/// Groups outcomes that exercise the same behaviour, for picking diverse samples.
fn signature(record: &Record) -> (u8, u8, String) {
    use super::oracle::Outcome;
    let kind = |outcome: &Outcome| match outcome {
        Outcome::Value(_) => 0,
        Outcome::Null => 1,
        Outcome::Error(_) => 2,
    };
    let message = match &record.cast {
        Outcome::Error(message) => message.chars().take(30).collect(),
        _ => String::new(),
    };
    (kind(&record.cast), kind(&record.try_cast), message)
}

/// Picks the checked-in sample: diverse cases per pair (one per distinct outcome
/// signature first, then evenly spaced), every case selected by `always`, and the
/// VARCHAR -> source record of every picked typed case so the source value can be verified.
pub fn select_subset(
    records: &[Record],
    is_pending: impl Fn(&GridType, &GridType) -> bool,
    always: impl Fn(&Case) -> bool,
) -> Vec<Record> {
    use super::oracle::Outcome;
    let mut by_pair: BTreeMap<(&str, &str), Vec<usize>> = BTreeMap::new();
    for (index, record) in records.iter().enumerate() {
        let key = (record.case.source.sql, record.case.target.sql);
        by_pair.entry(key).or_default().push(index);
    }
    let mut picked = HashSet::new();
    for (pair_index, indices) in by_pair.values().enumerate() {
        let first = &records[indices[0]].case;
        let pending = is_pending(first.source, first.target);
        let varchar = first.source.family == Family::Varchar;
        let limit = if varchar && !pending {
            SUBSET_VARCHAR
        } else {
            SUBSET_TYPED
        };
        let has_value = |i: &usize| matches!(records[*i].cast, Outcome::Value(_));
        let mut candidates: Vec<usize> = if pending {
            // Only casts DuckDB implements are worth tracking before they land.
            indices.iter().copied().filter(has_value).collect()
        } else if varchar || indices.iter().any(has_value) {
            indices.clone()
        } else {
            Vec::new()
        };
        if candidates.len() > 1 {
            // Prefer real values over the NULL source, and vary the value across pairs.
            candidates.retain(|&i| records[i].case.text.is_some());
            let rotation = pair_index % candidates.len().max(1);
            candidates.rotate_left(rotation);
        }
        let mut chosen = Vec::new();
        let mut signatures = HashSet::new();
        for &i in &candidates {
            if chosen.len() < limit && signatures.insert(signature(&records[i])) {
                chosen.push(i);
            }
        }
        let step = (candidates.len() / limit.max(1)).max(1);
        for &i in candidates.iter().step_by(step) {
            if chosen.len() < limit && !chosen.contains(&i) {
                chosen.push(i);
            }
        }
        picked.extend(chosen);
    }
    picked.extend((0..records.len()).filter(|&i| always(&records[i].case)));
    let varchar_records: HashMap<(&str, &str), usize> = records
        .iter()
        .enumerate()
        .filter(|(_, r)| r.case.source.family == Family::Varchar)
        .filter_map(|(i, r)| Some(((r.case.target.sql, r.case.text.as_deref()?), i)))
        .collect();
    let sources: Vec<usize> = picked
        .iter()
        .filter_map(|&i| {
            let case = &records[i].case;
            varchar_records
                .get(&(case.source.sql, case.text.as_deref()?))
                .copied()
        })
        .collect();
    picked.extend(sources);
    let mut picked: Vec<usize> = picked.into_iter().collect();
    picked.sort_unstable();
    picked.into_iter().map(|i| records[i].clone()).collect()
}
