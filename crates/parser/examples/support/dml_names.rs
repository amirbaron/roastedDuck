//! Frozen C++ INSERT rendering, JOIN USING and COPY-name codecs.
use roastedduck_ast::{ParseInfo, SelectStatement, SqlStatementKind};
use roastedduck_common::{ByteString, Error, Result};
use roastedduck_serde::binary::{BinaryDeserializer, BinarySerializer};
use roastedduck_serde::{Deserialize, Deserializer, SerializationOptions, Serialize, Serializer};
fn decode(hex: &str) -> Result<Vec<u8>> {
    if !hex.len().is_multiple_of(2) {
        return Err(Error::invalid_input("odd oracle hex length"));
    }
    hex.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(
            |pair| match (char::from(pair[0]).to_digit(16), char::from(pair[1]).to_digit(16)) {
                (Some(a), Some(b)) => Ok((a * 16 + b) as u8),
                _ => Err(Error::invalid_input("invalid oracle hex")),
            },
        )
        .collect()
}
fn check<T: Deserialize + Serialize>(
    wire: &[u8],
    expected: &[u8],
    render: impl FnOnce(&T) -> Result<ByteString>,
) -> Result<()> {
    let mut reader = BinaryDeserializer::new(wire);
    reader.begin()?;
    let value: T = (&mut reader as &mut dyn Deserializer).read_property(100, "payload")?;
    reader.end()?;
    let mut writer = BinarySerializer::new(SerializationOptions::default());
    writer.begin();
    (&mut writer as &mut dyn Serializer).write_property(100, "payload", &value)?;
    writer.end();
    assert_eq!(writer.bytes(), wire, "native statement-name wire");
    assert_eq!(render(&value)?.as_bytes(), expected, "native statement names/SQL");
    Ok(())
}
pub fn compare(line: &str) -> Result<()> {
    let fields: Vec<_> = line.split('\t').collect();
    let name = ByteString::from(decode(fields[1])?);
    let expected = decode(fields[3])?;
    match fields[0] {
        "insert" => {
            let mut statements =
                roastedduck_parser::parse("INSERT INTO t (x) VALUES (1) ON CONFLICT(x) DO UPDATE SET x = 2")?;
            let Some(statement) = statements.pop() else {
                return Err(Error::internal("missing INSERT"));
            };
            let SqlStatementKind::Insert(mut insert) = statement.kind else {
                return Err(Error::internal("not an INSERT"));
            };
            insert.catalog = name.clone();
            insert.schema = name.clone();
            insert.table = name.clone();
            insert.columns = vec![name.clone()];
            if let Some(reference) = &mut insert.table_ref {
                reference.alias = name.clone();
            }
            let info = insert
                .on_conflict_info
                .as_mut()
                .ok_or_else(|| Error::internal("missing ON CONFLICT"))?;
            info.indexed_columns = vec![name.clone()];
            info.set_info
                .as_mut()
                .ok_or_else(|| Error::internal("missing UPDATE SET"))?
                .columns = vec![name];
            assert_eq!(insert.to_sql_bytes().as_bytes(), expected, "native INSERT SQL");
            Ok(())
        }
        "join" => check::<SelectStatement>(&decode(fields[2])?, &expected, |value| Ok(value.to_sql_bytes())),
        "copy" => check::<ParseInfo>(&decode(fields[2])?, &expected, |value| {
            let ParseInfo::Copy(info) = value else {
                return Err(Error::internal("not COPY info"));
            };
            assert_eq!(info.catalog, name);
            assert_eq!(info.schema, name);
            assert_eq!(info.table, name);
            assert_eq!(info.select_list, vec![name.clone()]);
            Ok(info.table.clone())
        }),
        _ => Err(Error::invalid_input("unknown statement-name oracle kind")),
    }
}
