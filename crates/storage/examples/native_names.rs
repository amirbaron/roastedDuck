//! Compares native filter wire bytes/SQL and nested statistics with C++ 1.5.6.
use roastedduck_common::ByteString;
use roastedduck_serde::binary::BinarySerializer;
use roastedduck_serde::{SerializationOptions, Serializer};
use roastedduck_storage::{TableFilter, statistics::BaseStatistics};
use roastedduck_types::{LogicalType, Value, compare::CompareOp};
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
    let mut count = 0;
    for line in io::stdin().lock().lines() {
        let line = line?;
        let fields: Vec<_> = line.split('\t').collect();
        let name = ByteString::from(decode(fields[1]));
        let constant = || TableFilter::ConstantComparison {
            op: CompareOp::GreaterThanOrEqual,
            constant: Value::integer(5),
        };
        let rendered = if fields[0] == "4" || fields[0] == "5" {
            let mut ty = LogicalType::struct_type(vec![(name.clone(), LogicalType::INTEGER)]);
            if fields[0] == "5" {
                ty = LogicalType::struct_type(vec![(name, LogicalType::list(ty))]);
            }
            BaseStatistics::create_empty(ty).to_bytes()
        } else {
            let (filter, column) = match fields[0] {
                "0" => (constant(), name),
                "1" => (
                    TableFilter::StructExtract {
                        child_index: 2,
                        child_name: name,
                        child_filter: Box::new(constant()),
                    },
                    ByteString::from("r"),
                ),
                "2" => (TableFilter::In(vec![Value::integer(5), Value::integer(42)]), name),
                "3" => (
                    TableFilter::And(vec![
                        constant(),
                        TableFilter::ConstantComparison {
                            op: CompareOp::LessThan,
                            constant: Value::integer(42),
                        },
                    ]),
                    name,
                ),
                _ => return Err("unknown oracle mode".into()),
            };
            let mut writer = BinarySerializer::new(SerializationOptions::default());
            writer.begin();
            (&mut writer as &mut dyn Serializer).write_property(100, "payload", &filter)?;
            writer.end();
            assert_eq!(writer.bytes(), decode(fields[2]), "filter wire case {count}");
            filter.to_sql(column.as_bytes())
        };
        assert_eq!(rendered.as_bytes(), decode(fields[3]), "native text case {count}");
        count += 1;
    }
    println!("{count} C++ native storage name cases matched");
    Ok(())
}
