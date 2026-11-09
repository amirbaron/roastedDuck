//! Frozen C++ parser AST bytes and original SQL rendering, including Unicode names.
use roastedduck_ast::SqlStatementKind;
use roastedduck_serde::binary::BinarySerializer;
use roastedduck_serde::{SerializationOptions, Serializer};
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
fn parsed_name_wires_and_sql_match_cpp() -> Result<(), Box<dyn std::error::Error>> {
    let mut count = 0;
    for line in include_str!("fixtures/native_names_1.5.6.tsv").lines() {
        let fields: Vec<_> = line.split('\t').collect();
        let bytes = decode(fields[1]);
        let statements = roastedduck_parser::parse(std::str::from_utf8(&bytes)?)?;
        let [statement] = statements.as_slice() else {
            return Err("expected one statement".into());
        };
        let SqlStatementKind::Select(select) = &statement.kind else {
            return Err("expected SELECT".into());
        };
        let mut writer = BinarySerializer::new(SerializationOptions::default());
        writer.begin();
        (&mut writer as &mut dyn Serializer).write_property(100, "payload", select)?;
        writer.end();
        assert_eq!(writer.bytes(), decode(fields[2]));
        assert_eq!(select.to_sql_bytes().as_bytes(), decode(fields[3]));
        count += 1;
    }
    assert_eq!(count, 8);
    Ok(())
}
