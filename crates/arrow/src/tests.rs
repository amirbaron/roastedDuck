//! Round trips of data chunks through Arrow: exported with the converter and the
//! appender, read back with the schema import and the conversion.

use rustdb_types::{
    DataChunk, Date, Interval, LogicalType, SelectionVector, Time, TimeNs, TimeTz, Timestamp, Uuid, Value,
    Vector,
};

use crate::conversion::data_chunk_from_arrow;
use crate::ffi::{ArrowSchema, ArrowSchemaWrapper};
use crate::type_extension::json_type;
use crate::{
    ArrowFormatVersion, ArrowOffsetSize, ArrowTableSchema, ClientProperties, populate_arrow_table_schema,
};
use crate::{to_arrow_array, to_arrow_schema};

/// A chunk with one column per `(type, values)`, all of the same length.
fn chunk(columns: &[(LogicalType, Vec<Value>)]) -> DataChunk {
    let len = columns.first().map_or(0, |(_, values)| values.len());
    let vectors = columns
        .iter()
        .map(|(ty, values)| {
            let mut vector = Vector::flat(ty.clone(), len);
            for (row, value) in values.iter().enumerate() {
                vector.set_value(row, value).unwrap();
            }
            vector
        })
        .collect();
    DataChunk::new(vectors, len)
}

/// Exports `input` and reads it back: the types the schema imports as, and the chunk.
fn round_trip(input: &DataChunk, options: &ClientProperties) -> (Vec<LogicalType>, DataChunk) {
    let types: Vec<LogicalType> = input.columns().iter().map(|c| c.logical_type().clone()).collect();
    let names: Vec<String> = (0..types.len()).map(|i| format!("c{i}")).collect();
    let mut schema = ArrowSchemaWrapper::default();
    to_arrow_schema(&mut schema.arrow_schema, &types, &names, options).unwrap();
    let mut array = to_arrow_array(input, options).unwrap();
    let mut table = ArrowTableSchema::default();
    // SAFETY: the schema was just exported.
    unsafe { populate_arrow_table_schema(&mut table, &schema.arrow_schema) }.unwrap();
    assert_eq!(table.names(), names);
    // SAFETY: the array was just exported with this schema.
    let output = unsafe { data_chunk_from_arrow(&mut array, &table) }.unwrap();
    assert!(array.release.is_none(), "the chunk owns the array");
    (table.types().to_vec(), output)
}

/// Checks that every value of `input` reads back as itself, with its type.
fn assert_round_trips(input: &DataChunk, options: &ClientProperties) {
    let (types, output) = round_trip(input, options);
    assert_eq!(output.len(), input.len());
    for (column, (original, ty)) in input.columns().iter().zip(&types).enumerate() {
        assert_eq!(ty, original.logical_type(), "type of column {column}");
        for row in 0..input.len() {
            assert_eq!(
                output.column(column).get_value(row),
                original.get_value(row),
                "column {column} ({ty}), row {row}"
            );
        }
    }
}

fn wkb_point(x: f64, y: f64) -> Vec<u8> {
    let mut wkb = vec![1, 1, 0, 0, 0];
    wkb.extend_from_slice(&x.to_le_bytes());
    wkb.extend_from_slice(&y.to_le_bytes());
    wkb
}

/// Columns of every type that reads back as itself under any options.
fn exact_columns() -> Vec<(LogicalType, Vec<Value>)> {
    let enum_type = LogicalType::enum_type(vec!["red".into(), "green".into(), "a rather long colour".into()]);
    let struct_type = LogicalType::struct_type(vec![
        ("i".into(), LogicalType::INTEGER),
        ("s".into(), LogicalType::VARCHAR),
    ]);
    let union_type = LogicalType::union_type(vec![
        ("n".into(), LogicalType::INTEGER),
        ("t".into(), LogicalType::VARCHAR),
    ]);
    let list = |values: Vec<Value>| Value::list(LogicalType::BIGINT, values);
    let nested_list = LogicalType::list(LogicalType::list(LogicalType::VARCHAR));
    let null = Value::null;
    vec![
        (
            LogicalType::BOOLEAN,
            vec![
                Value::boolean(true),
                null(LogicalType::BOOLEAN),
                Value::boolean(false),
            ],
        ),
        (
            LogicalType::TINYINT,
            vec![
                Value::tinyint(-8),
                null(LogicalType::TINYINT),
                Value::tinyint(127),
            ],
        ),
        (
            LogicalType::SMALLINT,
            vec![
                Value::smallint(-300),
                Value::smallint(0),
                null(LogicalType::SMALLINT),
            ],
        ),
        (
            LogicalType::INTEGER,
            vec![
                null(LogicalType::INTEGER),
                Value::integer(42),
                Value::integer(i32::MIN),
            ],
        ),
        (
            LogicalType::BIGINT,
            vec![
                Value::bigint(i64::MAX),
                Value::bigint(-1),
                null(LogicalType::BIGINT),
            ],
        ),
        (
            LogicalType::UTINYINT,
            vec![
                Value::utinyint(255),
                null(LogicalType::UTINYINT),
                Value::utinyint(0),
            ],
        ),
        (
            LogicalType::USMALLINT,
            vec![
                Value::usmallint(65535),
                Value::usmallint(1),
                null(LogicalType::USMALLINT),
            ],
        ),
        (
            LogicalType::UINTEGER,
            vec![
                Value::uinteger(u32::MAX),
                null(LogicalType::UINTEGER),
                Value::uinteger(7),
            ],
        ),
        (
            LogicalType::UBIGINT,
            vec![
                Value::ubigint(u64::MAX),
                Value::ubigint(3),
                null(LogicalType::UBIGINT),
            ],
        ),
        (
            LogicalType::FLOAT,
            vec![Value::float(1.5), null(LogicalType::FLOAT), Value::float(-0.25)],
        ),
        (
            LogicalType::DOUBLE,
            vec![
                Value::double(std::f64::consts::PI),
                Value::double(-1e300),
                null(LogicalType::DOUBLE),
            ],
        ),
        (
            LogicalType::decimal(4, 1),
            vec![
                Value::decimal(-999, 4, 1),
                null(LogicalType::decimal(4, 1)),
                Value::decimal(15, 4, 1),
            ],
        ),
        (
            LogicalType::decimal(9, 3),
            vec![
                Value::decimal(123_456_789, 9, 3),
                Value::decimal(-1, 9, 3),
                null(LogicalType::decimal(9, 3)),
            ],
        ),
        (
            LogicalType::decimal(18, 2),
            vec![
                null(LogicalType::decimal(18, 2)),
                Value::decimal(-12_345_678_901, 18, 2),
                Value::decimal(0, 18, 2),
            ],
        ),
        (
            LogicalType::decimal(38, 10),
            vec![
                Value::decimal(i128::MAX / 10, 38, 10),
                null(LogicalType::decimal(38, 10)),
                Value::decimal(-5, 38, 10),
            ],
        ),
        (
            LogicalType::VARCHAR,
            vec![
                Value::varchar("short"),
                null(LogicalType::VARCHAR),
                Value::varchar("a string well over twelve bytes long"),
            ],
        ),
        (
            LogicalType::BLOB,
            vec![
                Value::blob(b"\x00\x01binary data over twelve bytes"),
                Value::blob(b""),
                null(LogicalType::BLOB),
            ],
        ),
        (
            LogicalType::DATE,
            vec![
                Value::date(Date(19_000)),
                null(LogicalType::DATE),
                Value::date(Date(-5)),
            ],
        ),
        (
            LogicalType::TIME,
            vec![
                Value::time(Time(45_296_000_001)),
                Value::time(Time(0)),
                null(LogicalType::TIME),
            ],
        ),
        (
            LogicalType::TIME_NS,
            vec![
                Value::time_ns(TimeNs(45_296_000_001_234)),
                null(LogicalType::TIME_NS),
                Value::time_ns(TimeNs(1)),
            ],
        ),
        (
            LogicalType::TIMESTAMP,
            vec![
                Value::timestamp(Timestamp(1_700_000_000_123_456)),
                null(LogicalType::TIMESTAMP),
                Value::timestamp(Timestamp(-1)),
            ],
        ),
        (
            LogicalType::TIMESTAMP_S,
            vec![
                Value::timestamp_s(1_700_000_000),
                Value::timestamp_s(0),
                null(LogicalType::TIMESTAMP_S),
            ],
        ),
        (
            LogicalType::TIMESTAMP_MS,
            vec![
                null(LogicalType::TIMESTAMP_MS),
                Value::timestamp_ms(1_700_000_000_123),
                Value::timestamp_ms(-2),
            ],
        ),
        (
            LogicalType::TIMESTAMP_NS,
            vec![
                Value::timestamp_ns(1_700_000_000_123_456_789),
                null(LogicalType::TIMESTAMP_NS),
                Value::timestamp_ns(3),
            ],
        ),
        (
            LogicalType::TIMESTAMP_TZ,
            vec![
                Value::timestamp_tz(Timestamp(1_700_000_000_000_000)),
                null(LogicalType::TIMESTAMP_TZ),
                Value::timestamp_tz(Timestamp(0)),
            ],
        ),
        (
            LogicalType::INTERVAL,
            vec![
                Value::interval(Interval {
                    months: 14,
                    days: -3,
                    micros: 1_000_001,
                }),
                null(LogicalType::INTERVAL),
                Value::interval(Interval::default()),
            ],
        ),
        (
            LogicalType::list(LogicalType::BIGINT),
            vec![
                list(vec![
                    Value::bigint(1),
                    null(LogicalType::BIGINT),
                    Value::bigint(3),
                ]),
                null(LogicalType::list(LogicalType::BIGINT)),
                list(vec![]),
            ],
        ),
        (
            nested_list.clone(),
            vec![
                Value::list(
                    LogicalType::list(LogicalType::VARCHAR),
                    vec![
                        Value::list(
                            LogicalType::VARCHAR,
                            vec![Value::varchar("x"), Value::varchar("a long nested string value")],
                        ),
                        null(LogicalType::list(LogicalType::VARCHAR)),
                    ],
                ),
                Value::list(LogicalType::list(LogicalType::VARCHAR), vec![]),
                null(nested_list),
            ],
        ),
        (
            struct_type.clone(),
            vec![
                Value::struct_value(vec![
                    ("i".into(), Value::integer(1)),
                    ("s".into(), Value::varchar("one")),
                ]),
                null(struct_type),
                Value::struct_value(vec![
                    ("i".into(), null(LogicalType::INTEGER)),
                    ("s".into(), Value::varchar("third row of the struct")),
                ]),
            ],
        ),
        (
            LogicalType::map(LogicalType::VARCHAR, LogicalType::INTEGER),
            vec![
                Value::map(
                    LogicalType::VARCHAR,
                    LogicalType::INTEGER,
                    vec![
                        (Value::varchar("k1"), Value::integer(1)),
                        (Value::varchar("k2"), null(LogicalType::INTEGER)),
                    ],
                ),
                null(LogicalType::map(LogicalType::VARCHAR, LogicalType::INTEGER)),
                Value::map(LogicalType::VARCHAR, LogicalType::INTEGER, vec![]),
            ],
        ),
        (
            LogicalType::array(LogicalType::INTEGER, 2),
            vec![
                Value::array(LogicalType::INTEGER, vec![Value::integer(1), Value::integer(2)]),
                null(LogicalType::array(LogicalType::INTEGER, 2)),
                Value::array(
                    LogicalType::INTEGER,
                    vec![null(LogicalType::INTEGER), Value::integer(4)],
                ),
            ],
        ),
        (
            union_type.clone(),
            vec![
                Value::union_value(union_type.clone(), 0, Value::integer(5)),
                Value::union_value(union_type.clone(), 1, Value::varchar("member text")),
                null(union_type),
            ],
        ),
        (
            LogicalType::GEOMETRY,
            vec![
                Value::bytes(LogicalType::GEOMETRY, &wkb_point(1.0, 2.0)),
                null(LogicalType::GEOMETRY),
                Value::bytes(LogicalType::GEOMETRY, &wkb_point(-3.5, 4.25)),
            ],
        ),
        (
            LogicalType::BIGNUM,
            vec![
                Value::bytes(LogicalType::BIGNUM, b"\x80\x00\x01\x05"),
                null(LogicalType::BIGNUM),
                Value::bytes(LogicalType::BIGNUM, b"\x80\x00\x01\x07"),
            ],
        ),
        (
            enum_type.clone(),
            vec![
                Value::enum_value(enum_type.clone(), 2),
                null(enum_type.clone()),
                Value::enum_value(enum_type, 0),
            ],
        ),
    ]
}

/// Enums read back as their values (Arrow dictionaries of strings).
fn expected_type(ty: &LogicalType) -> LogicalType {
    if ty.id() == rustdb_types::LogicalTypeId::Enum {
        LogicalType::VARCHAR
    } else {
        ty.clone()
    }
}

#[test]
fn every_type_round_trips() {
    let option_sets = [
        ClientProperties::default(),
        ClientProperties {
            arrow_offset_size: ArrowOffsetSize::Large,
            ..ClientProperties::default()
        },
        ClientProperties {
            produce_arrow_string_view: true,
            arrow_use_list_view: true,
            arrow_output_version: ArrowFormatVersion::V1_4,
            ..ClientProperties::default()
        },
        ClientProperties {
            arrow_output_version: ArrowFormatVersion::V1_5,
            arrow_offset_size: ArrowOffsetSize::Large,
            arrow_use_list_view: true,
            ..ClientProperties::default()
        },
    ];
    let columns = exact_columns();
    let input = chunk(&columns);
    for options in &option_sets {
        let (types, output) = round_trip(&input, options);
        for (column, ((ty, values), read_type)) in columns.iter().zip(&types).enumerate() {
            assert_eq!(
                read_type,
                &expected_type(ty),
                "type of column {column} with {options:?}"
            );
            for (row, value) in values.iter().enumerate() {
                let read = output.column(column).get_value(row);
                assert_eq!(
                    read.to_varchar().unwrap(),
                    value.to_varchar().unwrap(),
                    "column {column} ({ty}), row {row} with {options:?}"
                );
                assert_eq!(read.is_null(), value.is_null());
            }
        }
    }
}

#[test]
fn values_and_types_are_exact() {
    let columns: Vec<_> = exact_columns()
        .into_iter()
        .filter(|(ty, _)| ty.id() != rustdb_types::LogicalTypeId::Enum)
        .collect();
    assert_round_trips(&chunk(&columns), &ClientProperties::default());
}

#[test]
fn lossless_conversion_keeps_duckdb_types() {
    let uuid = Uuid::parse(b"6ba7b810-9dad-11d1-80b4-00c04fd430c8", true).unwrap();
    let columns = vec![
        (
            LogicalType::BOOLEAN,
            vec![
                Value::boolean(true),
                Value::null(LogicalType::BOOLEAN),
                Value::boolean(false),
            ],
        ),
        (
            LogicalType::HUGEINT,
            vec![
                Value::hugeint(i128::MIN),
                Value::null(LogicalType::HUGEINT),
                Value::hugeint(7),
            ],
        ),
        (
            LogicalType::UHUGEINT,
            vec![
                Value::uhugeint(u128::MAX),
                Value::uhugeint(0),
                Value::null(LogicalType::UHUGEINT),
            ],
        ),
        (
            LogicalType::UUID,
            vec![
                Value::uuid(uuid),
                Value::null(LogicalType::UUID),
                Value::uuid(Uuid(0)),
            ],
        ),
        (
            LogicalType::TIME_TZ,
            vec![
                Value::time_tz(TimeTz::new(Time(3_723_000_000), 3600)),
                Value::null(LogicalType::TIME_TZ),
                Value::time_tz(TimeTz::new(Time(0), -7200)),
            ],
        ),
        (
            json_type(),
            vec![
                Value::bytes(json_type(), br#"{"a": [1, 2]}"#),
                Value::null(json_type()),
                Value::bytes(json_type(), b"null"),
            ],
        ),
        (
            LogicalType::BIT,
            vec![
                Value::bytes(LogicalType::BIT, b"\x03\xf5"),
                Value::null(LogicalType::BIT),
                Value::bytes(LogicalType::BIT, b"\x00\x01"),
            ],
        ),
    ];
    let options = ClientProperties {
        arrow_lossless_conversion: true,
        ..ClientProperties::default()
    };
    assert_round_trips(&chunk(&columns), &options);
}

#[test]
fn types_without_an_arrow_counterpart_become_their_closest_type() {
    let uuid = Uuid::parse(b"6ba7b810-9dad-11d1-80b4-00c04fd430c8", true).unwrap();
    let columns = vec![
        (LogicalType::HUGEINT, vec![Value::hugeint(-12_345)]),
        (LogicalType::UUID, vec![Value::uuid(uuid)]),
        (
            LogicalType::TIME_TZ,
            vec![Value::time_tz(TimeTz::new(Time(3_723_000_000), 3600))],
        ),
        (json_type(), vec![Value::bytes(json_type(), b"[1]")]),
        (
            LogicalType::BIT,
            vec![Value::bytes(LogicalType::BIT, b"\x03\xf5")],
        ),
    ];
    let (types, output) = round_trip(&chunk(&columns), &ClientProperties::default());
    assert_eq!(
        types,
        [
            LogicalType::decimal(38, 0),
            LogicalType::VARCHAR,
            LogicalType::TIME,
            LogicalType::VARCHAR,
            LogicalType::BLOB
        ]
    );
    assert_eq!(output.column(0).get_value(0), Value::decimal(-12_345, 38, 0));
    assert_eq!(
        output.column(1).get_value(0),
        Value::varchar("6ba7b810-9dad-11d1-80b4-00c04fd430c8")
    );
    assert_eq!(output.column(2).get_value(0), Value::time(Time(3_723_000_000)));
    assert_eq!(output.column(3).get_value(0), Value::varchar("[1]"));
    assert_eq!(output.column(4).get_value(0), Value::blob(b"\x03\xf5"));
}

#[test]
fn constant_dictionary_and_sequence_vectors_are_appended_by_row() {
    let base = chunk(&[(
        LogicalType::VARCHAR,
        vec![
            Value::varchar("zero"),
            Value::varchar("a long string number one"),
            Value::null(LogicalType::VARCHAR),
        ],
    )]);
    let dictionary = base.column(0).slice(&SelectionVector::new(vec![2, 1, 1, 0]));
    let constant = Vector::constant(&Value::integer(9)).unwrap();
    let sequence = Vector::sequence(LogicalType::BIGINT, 10, 5);
    let input = DataChunk::new(vec![dictionary.clone(), constant, sequence], 4);
    let (_, output) = round_trip(&input, &ClientProperties::default());
    for row in 0..4 {
        assert_eq!(output.column(0).get_value(row), dictionary.get_value(row));
        assert_eq!(output.column(1).get_value(row), Value::integer(9));
        assert_eq!(
            output.column(2).get_value(row),
            Value::bigint(10 + 5 * row as i64)
        );
    }
}

#[test]
fn schema_names_formats_and_flags() {
    let types = [
        LogicalType::INTEGER,
        LogicalType::map(LogicalType::INTEGER, LogicalType::VARCHAR),
        LogicalType::TIMESTAMP_TZ,
        LogicalType::decimal(4, 1),
    ];
    let names = ["a".to_owned(), "m".to_owned(), "t".to_owned(), "d".to_owned()];
    let options = ClientProperties {
        time_zone: "Europe/Amsterdam".to_owned(),
        ..ClientProperties::default()
    };
    let mut schema = ArrowSchemaWrapper::default();
    to_arrow_schema(&mut schema.arrow_schema, &types, &names, &options).unwrap();
    let root = &schema.arrow_schema;
    // SAFETY: the schema was just exported; its strings live as long as it does.
    unsafe {
        assert_eq!(root.format_str(), "+s");
        assert_eq!(root.name_string(), "duckdb_query_result");
        let children: Vec<&ArrowSchema> = root.child_schemas().collect();
        let formats: Vec<&str> = children.iter().map(|child| child.format_str()).collect();
        assert_eq!(formats, ["i", "+m", "tsu:Europe/Amsterdam", "d:4,1,128"]);
        assert!(
            children
                .iter()
                .all(|child| child.flags == crate::ffi::ARROW_FLAG_NULLABLE)
        );
        let entries = children[1].child_schemas().next().unwrap();
        assert_eq!(entries.name_string(), "entries");
        assert_eq!(entries.flags, 0);
        let key_value: Vec<(String, i64)> = entries
            .child_schemas()
            .map(|f| (f.name_string(), f.flags))
            .collect();
        assert_eq!(key_value, [("key".to_owned(), 0), ("value".to_owned(), 2)]);
    }
}

#[test]
fn unsupported_types_are_errors() {
    let mut schema = ArrowSchema::empty();
    let error = to_arrow_schema(
        &mut schema,
        &[LogicalType::variant()],
        &["v".to_owned()],
        &ClientProperties::default(),
    )
    .unwrap_err();
    assert_eq!(error.raw_message(), "Unsupported Arrow type VARIANT");
    assert!(schema.release.is_none());
}
