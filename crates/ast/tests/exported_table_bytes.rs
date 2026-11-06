//! Frozen DuckDB 1.5.6 `ExportedTableData` wires, including native names and paths.
use rustdb_ast::ExportedTableData;
use rustdb_common::ByteString;
use rustdb_serde::binary::{BinaryDeserializer, BinarySerializer};
use rustdb_serde::{Deserializer, SerializationOptions, Serializer};
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
#[test]
fn native_export_metadata_round_trips_exact_cpp_property_bytes() -> rustdb_common::Result<()> {
    let mut count = 0;
    for line in include_str!("data/exported_table_bytes_1.5.6.tsv").lines() {
        let (name, wire) = line.split_once('\t').expect("frozen TSV pair");
        let name = ByteString::from(decode(name));
        let wire = decode(wire);
        let mut reader = BinaryDeserializer::new(&wire);
        reader.begin()?;
        let value: ExportedTableData =
            (&mut reader as &mut dyn Deserializer).read_property(100, "payload")?;
        reader.end()?;
        assert_eq!(value.table_name, name);
        assert_eq!(value.schema_name, name);
        assert_eq!(value.database_name, name);
        assert_eq!(value.file_path, name);
        assert_eq!(value.not_null_columns, vec![name.clone(), ByteString::new()]);
        let mut writer = BinarySerializer::new(SerializationOptions::default());
        writer.begin();
        (&mut writer as &mut dyn Serializer).write_property(100, "payload", &value)?;
        writer.end();
        assert_eq!(writer.bytes(), wire);
        count += 1;
    }
    assert_eq!(count, 7);
    Ok(())
}
