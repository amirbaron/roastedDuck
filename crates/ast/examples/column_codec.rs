//! Compares actual persisted column definitions and SQL bytes with DuckDB's native oracle.
use roastedduck_ast::{ColumnDefinition, ColumnList, CompressionType, TableColumnType, columns_to_sql};
use roastedduck_common::{ByteString, OrderedMap};
use roastedduck_serde::binary::{BinaryDeserializer, BinarySerializer};
use roastedduck_serde::{Deserializer, SerializationOptions, Serializer};
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
    let mut count = 0;
    for line in io::stdin().lock().lines() {
        let line = line?;
        let fields: Vec<_> = line.split('\t').collect();
        assert_eq!(fields.len(), 3);
        let name = ByteString::from(decode(fields[0]));
        let expected = decode(fields[1]);
        let mut reader = BinaryDeserializer::new(&expected);
        reader.begin()?;
        let column: ColumnDefinition = (&mut reader as &mut dyn Deserializer).read_property(100, "column")?;
        reader.end()?;
        assert_eq!(column.name, name);
        let expected_column = ColumnDefinition {
            name,
            logical_type: LogicalType::INTEGER,
            expression: None,
            category: TableColumnType::Standard,
            compression_type: CompressionType::CompressionAuto,
            comment: Value::default(),
            tags: OrderedMap::new(),
        };
        assert_eq!(column, expected_column);
        let mut writer = BinarySerializer::new(SerializationOptions::default());
        writer.begin();
        (&mut writer as &mut dyn Serializer).write_property(100, "column", &column)?;
        writer.end();
        assert_eq!(writer.bytes(), expected);
        let columns = ColumnList {
            columns: vec![column],
        };
        assert_eq!(columns_to_sql(&columns, &[]).as_bytes(), decode(fields[2]));
        count += 1;
    }
    println!("{count} C++ column definition binary round trips and SQL byte renderings matched");
    Ok(())
}
