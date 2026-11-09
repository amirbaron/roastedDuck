//! Native C++ cast outcomes for raw STRUCT/UNION member names.
use roastedduck_common::ByteString;
use roastedduck_function::cast::{CastFunctionSet, ValueCast};
use roastedduck_types::{LogicalType, Value};
use std::io::{self, BufRead};
fn decode(hex: &str) -> Vec<u8> {
    hex.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let digit = |b| match b {
                b'0'..=b'9' => b - b'0',
                b'a'..=b'f' => b - b'a' + 10,
                _ => unreachable!(),
            };
            digit(pair[0]) * 16 + digit(pair[1])
        })
        .collect()
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let set = CastFunctionSet::new();
    let mut count = 0;
    for line in io::stdin().lock().lines() {
        let line = line?;
        let fields: Vec<_> = line.split('\t').collect();
        let name = ByteString::from(decode(fields[1]));
        let error = if fields[0] == "0" || fields[0] == "1" {
            let (source, target) = if fields[0] == "0" {
                (
                    LogicalType::struct_type(vec![(name.clone(), LogicalType::INTEGER)]),
                    LogicalType::struct_type(vec![
                        (name.clone(), LogicalType::INTEGER),
                        (name, LogicalType::INTEGER),
                    ]),
                )
            } else {
                let mut other = name.clone();
                other.push_bytes("_other");
                (
                    LogicalType::union_type(vec![(name, LogicalType::INTEGER)]),
                    LogicalType::union_type(vec![(other, LogicalType::INTEGER)]),
                )
            };
            let Err(error) = set.get_cast_function(&source, &target) else {
                return Err("C++ rejected a cast accepted by Rust".into());
            };
            error
        } else {
            let (value, target) = match fields[0] {
                "2" => (
                    Value::struct_value(vec![(name, Value::integer(42))]),
                    LogicalType::INTEGER,
                ),
                "3" => (
                    Value::varchar("not_a_struct"),
                    LogicalType::struct_type(vec![(name, LogicalType::INTEGER)]),
                ),
                "4" => (
                    Value::struct_value(vec![("existing".into(), Value::integer(42))])
                        .default_cast_as(&LogicalType::variant())?,
                    LogicalType::struct_type(vec![(name, LogicalType::INTEGER)]),
                ),
                _ => return Err("unknown cast case".into()),
            };
            let outcome = value.default_cast_as(&target);
            if let Some(expected) = fields[2].strip_prefix("OK:") {
                let actual = outcome?;
                assert_eq!(
                    actual.to_sql_string().as_bytes(),
                    decode(expected),
                    "cast value case {count}"
                );
                count += 1;
                continue;
            }
            let Err(error) = outcome else {
                return Err("C++ rejected a value cast accepted by Rust".into());
            };
            error
        };
        assert_ne!(fields[2], "OK");
        assert_eq!(error.to_bytes(), decode(fields[2]), "cast name case {count}");
        count += 1;
    }
    println!("{count} C++ raw-name cast outcomes matched");
    Ok(())
}
