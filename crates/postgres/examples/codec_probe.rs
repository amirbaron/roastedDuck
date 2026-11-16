//! Differential codec driver. Input SQL is supplied only by the owned test harness.
use roastedduck_postgres::{
    Driver,
    codec::{Codec, Options, encode_parameter},
    config::Parameters,
};
use roastedduck_types::{LogicalType, Value};

fn csv(values: &[String]) -> String {
    values
        .iter()
        .map(|v| format!("\"{}\"", v.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(",")
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments: Vec<String> = std::env::args().collect();
    let query = std::fs::read_to_string(arguments.get(1).ok_or("query file required")?)?;
    let driver = Driver::new()?;
    let session = driver.connect(&Parameters::parse("dbname=postgresscanner")?)?;
    session.batch("SET TIME ZONE 'UTC'")?;
    let options = Options {
        resolve_custom_types: !arguments.iter().any(|s| s == "--legacy"),
        numeric_as_varchar: arguments.iter().any(|s| s == "--numeric-as-varchar"),
        array_as_varchar: arguments.iter().any(|s| s == "--array-as-varchar"),
        numeric_nan_as_null: !arguments.iter().any(|s| s == "--reject-nan"),
    };
    let columns = session.describe(&query)?;
    let codecs = columns
        .iter()
        .map(|c| {
            if arguments.iter().any(|s| s == "--query") {
                Codec::for_query(&c.data_type, c.type_modifier, options)
            } else {
                Codec::new(&c.data_type, c.type_modifier, 0, options)
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    let identifiers = columns
        .iter()
        .enumerate()
        .map(|(i, _)| format!("c{i}"))
        .collect::<Vec<_>>();
    let expressions = codecs
        .iter()
        .zip(&identifiers)
        .map(|(c, name)| c.projection(name))
        .collect::<Result<Vec<_>, _>>()?;
    let query = format!(
        "SELECT {} FROM ({query}) AS q({})",
        expressions.join(","),
        identifiers.join(",")
    );
    if arguments.iter().any(|s| s == "--types") {
        println!(
            "{}",
            csv(&codecs
                .iter()
                .map(|c| c.logical_type.to_string())
                .collect::<Vec<_>>())
        );
    }
    let mut stream = session.stream(&query, &[])?;
    loop {
        let batch = stream.next_batch(128)?;
        if batch.is_empty() {
            break;
        }
        for row in batch {
            let values = codecs
                .iter()
                .zip(row)
                .map(|(c, raw)| c.decode(raw.as_deref()))
                .collect::<Result<Vec<_>, _>>()?;
            println!(
                "{}",
                csv(&values
                    .iter()
                    .map(|v| if v.is_null() {
                        Ok("__NULL__".into())
                    } else {
                        v.to_varchar()
                    })
                    .collect::<Result<Vec<_>, _>>()?)
            );
        }
    }
    // Exercise actual parameter conversion through the server, including wide
    // decimal, negative epochs, UUID byte order and nullable multidimensional arrays.
    for (sql_type, value, dimensions) in [
        (
            "numeric(38,38)",
            Value::decimal(-12_345_678_901_234_567_890_123_456_789_012_345_678, 38, 38),
            0,
        ),
        (
            "numeric(38,0)",
            Value::decimal(99_999_999_999_999_999_999_999_999_999_999_999_999, 38, 0),
            0,
        ),
        ("timestamp", Value::timestamp(roastedduck_types::Timestamp(-1)), 0),
        ("date", Value::date(roastedduck_types::Date::NEG_INFINITY), 0),
        (
            "timetz",
            Value::time_tz(roastedduck_types::TimeTz::new(
                roastedduck_types::Time(123_456),
                -7_200,
            )),
            0,
        ),
        (
            "uuid",
            Value::uuid(roastedduck_types::Uuid::from_u128(
                0x0123_4567_89ab_cdef_8000_fedc_ba98_7654,
            )),
            0,
        ),
        (
            "integer[]",
            Value::list(
                LogicalType::list(LogicalType::INTEGER),
                vec![
                    Value::list(
                        LogicalType::INTEGER,
                        vec![Value::integer(7), Value::null(LogicalType::INTEGER)],
                    ),
                    Value::list(LogicalType::INTEGER, vec![Value::integer(-8), Value::integer(9)]),
                ],
            ),
            2,
        ),
    ] {
        let mut rows = session.stream(&format!("SELECT $1::{sql_type}"), &[encode_parameter(&value)?])?;
        let column = &rows.columns[0];
        let codec = Codec::new(
            &column.data_type,
            column.type_modifier,
            dimensions,
            Options::default(),
        )?;
        let batch = rows.next_batch(2)?;
        let decoded = codec.decode(batch[0][0].as_deref())?;
        if decoded != value {
            return Err(
                format!("{sql_type} parameter round trip mismatch: {decoded:?} versus {value:?}").into(),
            );
        }
    }
    Ok(())
}
