//! `test_all_types()` (DuckDB's `function/table/system/test_all_types.cpp`): one column
//! per supported type and three rows holding its minimum, its maximum and NULL.

use std::any::Any;
use std::sync::Arc;

use rustdb_common::{Error, Result};
use rustdb_function::{
    FunctionData, TableFunction, TableFunctionBindInput, TableFunctionBindOutput, TableFunctionSet,
    TableScanState,
};
use rustdb_types::{DataChunk, Date, Interval, LogicalType, Time, TimeNs, Timestamp, Value, Vector};

use crate::limits::{double_max_bignum, maximum_value, minimum_value};

/// One column of `test_all_types` (DuckDB's `TestType`).
#[derive(Debug, Clone)]
struct TestType {
    ty: LogicalType,
    name: &'static str,
    min: Value,
    max: Value,
}

impl TestType {
    /// A column whose extremes are the type's `MinimumValue` and `MaximumValue`.
    fn limits(ty: LogicalType, name: &'static str) -> Result<Self> {
        Ok(Self {
            min: minimum_value(&ty)?,
            max: maximum_value(&ty)?,
            ty,
            name,
        })
    }

    fn new(ty: LogicalType, name: &'static str, min: Value, max: Value) -> Self {
        Self { ty, name, min, max }
    }
}

const DUCKS: &str = "🦆🦆🦆🦆🦆🦆";

/// `POINT EMPTY` as WKB.
const MIN_GEOMETRY_WKB: &str = "0101000000000000000000F87F000000000000F87F";

/// The nested `GEOMETRYCOLLECTION` of DuckDB's maximum GEOMETRY, as WKB.
const MAX_GEOMETRY_WKB: &str = concat!(
    "01070000000D0000000101000000000000000000F03F00000000000000400101000000000000000000F87F00000000",
    "0000F87F01020000000200000000000000000000000000000000000000000000000000F03F000000000000F03F0102",
    "0000000000000001030000000100000005000000000000000000000000000000000000000000000000000000000000",
    "000000F03F000000000000F03F000000000000F03F000000000000F03F00000000000000000000000000000000000000",
    "000000000000000103000000000000000104000000020000000101000000000000000000144000000000000018400101",
    "000000000000000000F87F000000000000F87F0105000000040000000102000000020000000000000000000000000000",
    "0000000000000000000000F03F000000000000F03F01020000000000000001020000000200000000000000000000400000",
    "000000000040000000000000084000000000000008400102000000000000000105000000000000000106000000040000",
    "0001030000000100000005000000000000000000000000000000000000000000000000000000000000000000F03F0000",
    "00000000F03F000000000000F03F000000000000F03F0000000000000000000000000000000000000000000000000103",
    "0000000000000001030000000100000005000000000000000000000000000000000000000000000000000000000000000",
    "0000040000000000000004000000000000000400000000000000040000000000000000000000000000000000000000000",
    "000000010300000000000000010600000000000000010700000001000000010100000000000000000014400000000000",
    "001840010700000000000000",
);

fn hex_bytes(hex: &str) -> Vec<u8> {
    let digit = |c: u8| (c as char).to_digit(16).unwrap_or(0) as u8;
    hex.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&[high, low]| (digit(high) << 4) | digit(low))
        .collect()
}

fn enum_type(values: impl IntoIterator<Item = String>) -> LogicalType {
    LogicalType::enum_type(values.into_iter().collect())
}

fn date(year: i32, month: i32, day: i32) -> Result<Date> {
    Date::from_ymd(year, month, day).ok_or_else(|| Error::internal("invalid test date"))
}

/// `2022-05-12 <hour>:23:45` as a timestamp.
fn timestamp_2022(hour: i64) -> Result<Timestamp> {
    let time = Time(((hour * 60 + 23) * 60 + 45) * 1_000_000);
    Timestamp::from_datetime(date(2022, 5, 12)?, time)
        .ok_or_else(|| Error::internal("invalid test timestamp"))
}

fn int_list(values: &[Option<i32>]) -> Value {
    let values = values
        .iter()
        .map(|value| value.map_or_else(|| Value::null(LogicalType::INTEGER), Value::integer))
        .collect();
    Value::list(LogicalType::INTEGER, values)
}

fn varchar_values(values: &[Option<&str>]) -> Vec<Value> {
    values
        .iter()
        .map(|value| value.map_or_else(|| Value::null(LogicalType::VARCHAR), Value::varchar))
        .collect()
}

/// DuckDB's `TestAllTypesFun::GetTestTypes`.
#[allow(clippy::too_many_lines, reason = "one entry per type, in DuckDB's order")]
fn test_types(use_large_enum: bool, use_large_bignum: bool) -> Result<Vec<TestType>> {
    let mut result = Vec::new();
    for (ty, name) in [
        (LogicalType::BOOLEAN, "bool"),
        (LogicalType::TINYINT, "tinyint"),
        (LogicalType::SMALLINT, "smallint"),
        (LogicalType::INTEGER, "int"),
        (LogicalType::BIGINT, "bigint"),
        (LogicalType::HUGEINT, "hugeint"),
        (LogicalType::UHUGEINT, "uhugeint"),
        (LogicalType::UTINYINT, "utinyint"),
        (LogicalType::USMALLINT, "usmallint"),
        (LogicalType::UINTEGER, "uint"),
        (LogicalType::UBIGINT, "ubigint"),
    ] {
        result.push(TestType::limits(ty, name)?);
    }
    if use_large_bignum {
        // The largest magnitude a BIGNUM can hold: MAX_DATA_SIZE bytes of 0xFF.
        const MAX_DATA_SIZE: usize = 8_388_607;
        let header = 0x0080_0000u32 | MAX_DATA_SIZE as u32;
        let mut max = vec![(header >> 16) as u8, (header >> 8) as u8, header as u8];
        max.resize(3 + MAX_DATA_SIZE, 0xFF);
        let min: Vec<u8> = max.iter().map(|byte| !byte).collect();
        result.push(TestType::new(
            LogicalType::BIGNUM,
            "bignum",
            Value::bytes(LogicalType::BIGNUM, &min),
            Value::bytes(LogicalType::BIGNUM, &max),
        ));
    } else {
        result.push(TestType::new(
            LogicalType::BIGNUM,
            "bignum",
            Value::bytes(LogicalType::BIGNUM, &double_max_bignum(true)),
            Value::bytes(LogicalType::BIGNUM, &double_max_bignum(false)),
        ));
    }
    for (ty, name) in [
        (LogicalType::DATE, "date"),
        (LogicalType::TIME, "time"),
        (LogicalType::TIMESTAMP, "timestamp"),
        (LogicalType::TIMESTAMP_S, "timestamp_s"),
        (LogicalType::TIMESTAMP_MS, "timestamp_ms"),
        (LogicalType::TIMESTAMP_NS, "timestamp_ns"),
        (LogicalType::TIME_TZ, "time_tz"),
        (LogicalType::TIMESTAMP_TZ, "timestamp_tz"),
    ] {
        result.push(TestType::limits(ty, name)?);
    }
    result.push(TestType::new(
        LogicalType::FLOAT,
        "float",
        Value::float(f32::MIN),
        Value::float(f32::MAX),
    ));
    result.push(TestType::new(
        LogicalType::DOUBLE,
        "double",
        Value::double(f64::MIN),
        Value::double(f64::MAX),
    ));
    for (width, scale, name) in [
        (4, 1, "dec_4_1"),
        (9, 4, "dec_9_4"),
        (18, 6, "dec_18_6"),
        (38, 10, "dec38_10"),
    ] {
        result.push(TestType::limits(LogicalType::decimal(width, scale), name)?);
    }
    result.push(TestType::limits(LogicalType::UUID, "uuid")?);
    result.push(TestType::new(
        LogicalType::INTERVAL,
        "interval",
        Value::interval(Interval {
            months: 0,
            days: 0,
            micros: 0,
        }),
        Value::interval(Interval {
            months: 999,
            days: 999,
            micros: 999_999_999,
        }),
    ));
    result.push(TestType::new(
        LogicalType::VARCHAR,
        "varchar",
        Value::varchar(DUCKS),
        Value::bytes(LogicalType::VARCHAR, b"goo\0se"),
    ));
    result.push(TestType::new(
        LogicalType::BLOB,
        "blob",
        Value::blob(b"thisisalongblob\x00withnullbytes"),
        Value::blob(b"\x00\x00\x00a"),
    ));
    let bit = |text: &str| -> Result<Value> {
        let mut bits = Vec::new();
        rustdb_types::bit::parse_text(text.as_bytes(), &mut bits).map_err(Error::conversion)?;
        Ok(Value::bytes(LogicalType::BIT, &bits))
    };
    result.push(TestType::new(
        LogicalType::BIT,
        "bit",
        bit("0010001001011100010101011010111")?,
        bit("10101")?,
    ));
    let small_enum = enum_type(["DUCK_DUCK_ENUM".to_owned(), "GOOSE".to_owned()]);
    result.push(TestType::limits(small_enum, "small_enum")?);
    let medium_enum = enum_type((0..300).map(|i| format!("enum_{i}")));
    result.push(TestType::limits(medium_enum, "medium_enum")?);
    let large_enum = if use_large_enum {
        enum_type((0..70_000).map(|i| format!("enum_{i}")))
    } else {
        enum_type(["enum_0".to_owned(), "enum_69999".to_owned()])
    };
    result.push(TestType::limits(large_enum, "large_enum")?);

    // LISTs.
    let int_list_type = LogicalType::list(LogicalType::INTEGER);
    let empty_int_list = int_list(&[]);
    let full_int_list = int_list(&[Some(42), Some(999), None, None, Some(-42)]);
    result.push(TestType::new(
        int_list_type.clone(),
        "int_array",
        empty_int_list.clone(),
        full_int_list.clone(),
    ));
    let doubles = [
        Some(42.0),
        Some(f64::NAN),
        Some(f64::INFINITY),
        Some(f64::NEG_INFINITY),
        None,
        Some(-42.0),
    ];
    result.push(TestType::new(
        LogicalType::list(LogicalType::DOUBLE),
        "double_array",
        Value::list(LogicalType::DOUBLE, Vec::new()),
        Value::list(
            LogicalType::DOUBLE,
            doubles
                .iter()
                .map(|value| value.map_or_else(|| Value::null(LogicalType::DOUBLE), Value::double))
                .collect(),
        ),
    ));
    result.push(TestType::new(
        LogicalType::list(LogicalType::DATE),
        "date_array",
        Value::list(LogicalType::DATE, Vec::new()),
        Value::list(
            LogicalType::DATE,
            vec![
                Value::date(Date(0)),
                Value::date(Date::INFINITY),
                Value::date(Date::NEG_INFINITY),
                Value::null(LogicalType::DATE),
                Value::date(date(2022, 5, 12)?),
            ],
        ),
    ));
    result.push(TestType::new(
        LogicalType::list(LogicalType::TIMESTAMP),
        "timestamp_array",
        Value::list(LogicalType::TIMESTAMP, Vec::new()),
        Value::list(
            LogicalType::TIMESTAMP,
            vec![
                Value::timestamp(Timestamp(0)),
                Value::timestamp(Timestamp::INFINITY),
                Value::timestamp(Timestamp::NEG_INFINITY),
                Value::null(LogicalType::TIMESTAMP),
                Value::timestamp(timestamp_2022(16)?),
            ],
        ),
    ));
    result.push(TestType::new(
        LogicalType::list(LogicalType::TIMESTAMP_TZ),
        "timestamptz_array",
        Value::list(LogicalType::TIMESTAMP_TZ, Vec::new()),
        Value::list(
            LogicalType::TIMESTAMP_TZ,
            vec![
                Value::timestamp_tz(Timestamp(0)),
                Value::timestamp_tz(Timestamp::INFINITY),
                Value::timestamp_tz(Timestamp::NEG_INFINITY),
                Value::null(LogicalType::TIMESTAMP_TZ),
                // 2022-05-12 16:23:45-07
                Value::timestamp_tz(timestamp_2022(23)?),
            ],
        ),
    ));
    let varchar_list_type = LogicalType::list(LogicalType::VARCHAR);
    let full_varchar_list = Value::list(
        LogicalType::VARCHAR,
        varchar_values(&[Some(DUCKS), Some("goose"), None, Some("")]),
    );
    result.push(TestType::new(
        varchar_list_type.clone(),
        "varchar_array",
        Value::list(LogicalType::VARCHAR, Vec::new()),
        full_varchar_list.clone(),
    ));
    result.push(TestType::new(
        LogicalType::list(int_list_type.clone()),
        "nested_int_array",
        Value::list(int_list_type.clone(), Vec::new()),
        Value::list(
            int_list_type.clone(),
            vec![
                empty_int_list.clone(),
                full_int_list.clone(),
                Value::null(int_list_type.clone()),
                empty_int_list.clone(),
                full_int_list.clone(),
            ],
        ),
    ));

    // STRUCTs.
    let struct_type = LogicalType::struct_type(vec![
        ("a".to_owned(), LogicalType::INTEGER),
        ("b".to_owned(), LogicalType::VARCHAR),
    ]);
    let min_struct = Value::struct_value(vec![
        ("a".to_owned(), Value::null(LogicalType::INTEGER)),
        ("b".to_owned(), Value::null(LogicalType::VARCHAR)),
    ]);
    let max_struct = Value::struct_value(vec![
        ("a".to_owned(), Value::integer(42)),
        ("b".to_owned(), Value::varchar(DUCKS)),
    ]);
    result.push(TestType::new(
        struct_type.clone(),
        "struct",
        min_struct.clone(),
        max_struct.clone(),
    ));
    result.push(TestType::new(
        LogicalType::struct_type(vec![
            ("a".to_owned(), int_list_type.clone()),
            ("b".to_owned(), varchar_list_type.clone()),
        ]),
        "struct_of_arrays",
        Value::struct_value(vec![
            ("a".to_owned(), Value::null(int_list_type.clone())),
            ("b".to_owned(), Value::null(varchar_list_type)),
        ]),
        Value::struct_value(vec![
            ("a".to_owned(), full_int_list.clone()),
            ("b".to_owned(), full_varchar_list),
        ]),
    ));
    result.push(TestType::new(
        LogicalType::list(struct_type.clone()),
        "array_of_structs",
        Value::list(struct_type.clone(), Vec::new()),
        Value::list(
            struct_type.clone(),
            vec![
                min_struct.clone(),
                max_struct.clone(),
                Value::null(struct_type.clone()),
            ],
        ),
    ));
    result.push(TestType::new(
        LogicalType::map(LogicalType::VARCHAR, LogicalType::VARCHAR),
        "map",
        Value::map(LogicalType::VARCHAR, LogicalType::VARCHAR, Vec::new()),
        Value::map(
            LogicalType::VARCHAR,
            LogicalType::VARCHAR,
            vec![
                (Value::varchar("key1"), Value::varchar(DUCKS)),
                (Value::varchar("key2"), Value::varchar("goose")),
            ],
        ),
    ));
    let union_type = LogicalType::union_type(vec![
        ("name".to_owned(), LogicalType::VARCHAR),
        ("age".to_owned(), LogicalType::SMALLINT),
    ]);
    result.push(TestType::new(
        union_type.clone(),
        "union",
        Value::union_value(union_type.clone(), 0, Value::varchar("Frank")),
        Value::union_value(union_type, 1, Value::smallint(5)),
    ));

    // ARRAYs.
    let fixed_int_type = LogicalType::array(LogicalType::INTEGER, 3);
    let fixed_int_min = Value::array(
        LogicalType::INTEGER,
        vec![
            Value::null(LogicalType::INTEGER),
            Value::integer(2),
            Value::integer(3),
        ],
    );
    let fixed_int_max = Value::array(
        LogicalType::INTEGER,
        vec![Value::integer(4), Value::integer(5), Value::integer(6)],
    );
    result.push(TestType::new(
        fixed_int_type.clone(),
        "fixed_int_array",
        fixed_int_min.clone(),
        fixed_int_max.clone(),
    ));
    let fixed_varchar_type = LogicalType::array(LogicalType::VARCHAR, 3);
    let fixed_varchar_min = Value::array(
        LogicalType::VARCHAR,
        varchar_values(&[Some("a"), None, Some("c")]),
    );
    let fixed_varchar_max = Value::array(
        LogicalType::VARCHAR,
        varchar_values(&[Some("d"), Some("e"), Some("f")]),
    );
    result.push(TestType::new(
        fixed_varchar_type.clone(),
        "fixed_varchar_array",
        fixed_varchar_min.clone(),
        fixed_varchar_max.clone(),
    ));
    result.push(TestType::new(
        LogicalType::array(fixed_int_type.clone(), 3),
        "fixed_nested_int_array",
        Value::array(
            fixed_int_type.clone(),
            vec![
                fixed_int_min.clone(),
                Value::null(fixed_int_type.clone()),
                fixed_int_min.clone(),
            ],
        ),
        Value::array(
            fixed_int_type.clone(),
            vec![
                fixed_int_max.clone(),
                fixed_int_min.clone(),
                fixed_int_max.clone(),
            ],
        ),
    ));
    result.push(TestType::new(
        LogicalType::array(fixed_varchar_type.clone(), 3),
        "fixed_nested_varchar_array",
        Value::array(
            fixed_varchar_type.clone(),
            vec![
                fixed_varchar_min.clone(),
                Value::null(fixed_varchar_type.clone()),
                fixed_varchar_min.clone(),
            ],
        ),
        Value::array(
            fixed_varchar_type.clone(),
            vec![
                fixed_varchar_max.clone(),
                fixed_varchar_min.clone(),
                fixed_varchar_max.clone(),
            ],
        ),
    ));
    result.push(TestType::new(
        LogicalType::array(struct_type.clone(), 3),
        "fixed_struct_array",
        Value::array(
            struct_type.clone(),
            vec![min_struct.clone(), max_struct.clone(), min_struct.clone()],
        ),
        Value::array(struct_type, vec![max_struct.clone(), min_struct, max_struct]),
    ));
    result.push(TestType::new(
        LogicalType::struct_type(vec![
            ("a".to_owned(), fixed_int_type.clone()),
            ("b".to_owned(), fixed_varchar_type),
        ]),
        "struct_of_fixed_array",
        Value::struct_value(vec![
            ("a".to_owned(), fixed_int_min.clone()),
            ("b".to_owned(), fixed_varchar_min),
        ]),
        Value::struct_value(vec![
            ("a".to_owned(), fixed_int_max.clone()),
            ("b".to_owned(), fixed_varchar_max),
        ]),
    ));
    result.push(TestType::new(
        LogicalType::array(int_list_type.clone(), 3),
        "fixed_array_of_int_list",
        Value::array(
            int_list_type.clone(),
            vec![
                empty_int_list.clone(),
                full_int_list.clone(),
                empty_int_list.clone(),
            ],
        ),
        Value::array(
            int_list_type,
            vec![full_int_list.clone(), empty_int_list, full_int_list],
        ),
    ));
    result.push(TestType::new(
        LogicalType::list(fixed_int_type.clone()),
        "list_of_fixed_int_array",
        Value::list(
            fixed_int_type.clone(),
            vec![
                fixed_int_min.clone(),
                fixed_int_max.clone(),
                fixed_int_min.clone(),
            ],
        ),
        Value::list(
            fixed_int_type,
            vec![fixed_int_max.clone(), fixed_int_min, fixed_int_max],
        ),
    ));
    result.push(TestType::new(
        LogicalType::TIME_NS,
        "time_ns",
        Value::time_ns(TimeNs(0)),
        Value::time_ns(TimeNs(86_400_000_000_000)),
    ));
    result.push(TestType::new(
        LogicalType::GEOMETRY,
        "geometry",
        Value::bytes(LogicalType::GEOMETRY, &hex_bytes(MIN_GEOMETRY_WKB)),
        Value::bytes(LogicalType::GEOMETRY, &hex_bytes(MAX_GEOMETRY_WKB)),
    ));
    Ok(result)
}

#[derive(Debug)]
struct TestAllTypesBindData {
    types: Vec<TestType>,
}

impl FunctionData for TestAllTypesBindData {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// A boolean named parameter of the call (DuckDB rejects NULL).
fn flag(input: &TableFunctionBindInput<'_>, name: &str) -> Result<bool> {
    match input.named_parameters.iter().find(|(key, _)| key == name) {
        None => Ok(false),
        Some((_, value)) if value.is_null() => Err(Error::invalid_input(format!(
            "Cannot use NULL as argument for {name}"
        ))),
        Some((_, value)) => Ok(value.as_i128().is_some_and(|flag| flag != 0)),
    }
}

/// DuckDB's `TestAllTypesBind`.
fn bind(input: &TableFunctionBindInput<'_>) -> Result<TableFunctionBindOutput> {
    let types = test_types(flag(input, "use_large_enum")?, flag(input, "use_large_bignum")?)?;
    Ok(TableFunctionBindOutput {
        return_types: types.iter().map(|test| test.ty.clone()).collect(),
        names: types.iter().map(|test| test.name.to_owned()).collect(),
        bind_data: Some(Arc::new(TestAllTypesBindData { types })),
    })
}

/// DuckDB's `TestAllTypesFunction`: the minimum, maximum and NULL rows in one chunk.
fn scan(
    bind_data: Option<&dyn FunctionData>,
    _: &[u64],
    state: &mut TableScanState,
    chunk: &mut DataChunk,
) -> Result<bool> {
    let bind_data = bind_data
        .and_then(|data| data.as_any().downcast_ref::<TestAllTypesBindData>())
        .ok_or_else(|| Error::internal("test_all_types scan without bind data"))?;
    let rows = if state.position == 0 { 3 } else { 0 };
    let mut columns = Vec::with_capacity(bind_data.types.len());
    for test in &bind_data.types {
        let mut column = Vector::flat(test.ty.clone(), 3);
        if rows > 0 {
            column.set_value(0, &test.min)?;
            column.set_value(1, &test.max)?;
            column.set_value(2, &Value::null(test.ty.clone()))?;
        }
        columns.push(column);
    }
    state.position += rows as u64;
    chunk.set_columns(columns, rows);
    Ok(false)
}

/// `test_all_types([use_large_enum := BOOLEAN, use_large_bignum := BOOLEAN])`.
pub fn test_all_types_functions() -> TableFunctionSet {
    let mut function = TableFunction::new("test_all_types", Vec::new())
        .with_bind(bind)
        .with_scan(scan);
    function.named_parameters = vec![
        ("use_large_enum".to_owned(), LogicalType::BOOLEAN),
        ("use_large_bignum".to_owned(), LogicalType::BOOLEAN),
    ];
    let mut set = TableFunctionSet::new("test_all_types");
    set.add(function);
    set
}
