//! Reading DuckDB write-ahead-log files (unencrypted), entry by entry.
//!
//! A WAL starts with a header object (`WAL_VERSION`), followed by entries of the form
//! `[u64 size][u64 checksum][size bytes]`, each a `BinarySerializer` object whose first
//! property (100) is the `WALType`.

use roastedduck_ast::{AlterInfoKind, CreateInfo, ParseInfo};
use roastedduck_serde::binary::{BinaryDeserializer, BinarySerializer};
use roastedduck_serde::{
    DeserializePointee, Deserializer, Pointee, SerializationCompatibility, SerializationOptions,
    deserialize_present, serialize_present,
};

/// `WALType` values of entries holding a serialized catalog entry (a `CreateInfo`).
pub const CREATE_ENTRY_TYPES: &[(u8, &str)] = &[
    (1, "CREATE_TABLE"),
    (5, "CREATE_VIEW"),
    (8, "CREATE_SEQUENCE"),
    (11, "CREATE_MACRO"),
    (13, "CREATE_TYPE"),
    (21, "CREATE_TABLE_MACRO"),
    (23, "CREATE_INDEX"),
];

/// `WALType::ALTER_INFO`, an entry holding an `AlterInfo` (`ParseInfo`).
pub const ALTER_INFO: u8 = 20;

/// One WAL entry.
#[derive(Debug, Clone, Copy)]
pub struct Entry<'a> {
    pub wal_type: u8,
    /// The whole serialized object.
    pub payload: &'a [u8],
}

/// Splits a WAL file into entries; returns `None` if the file is malformed.
pub fn entries(data: &[u8]) -> Option<Vec<Entry<'_>>> {
    // The header is a bare object: skip it by reading its known properties.
    let mut header = BinaryDeserializer::new(data);
    {
        let d: &mut dyn Deserializer = &mut header;
        d.on_object_begin().ok()?;
        let _: u8 = d.read_property(100, "wal_type").ok()?;
        let _: u64 = d.read_property(101, "version").ok()?;
        let _: Option<Vec<u8>> = d.read_optional(102, "db_identifier").ok()?;
        let _: Option<u64> = d.read_optional(103, "checkpoint_iteration").ok()?;
        d.on_object_end().ok()?;
    }
    let mut position = header.position();
    let mut result = Vec::new();
    while position < data.len() {
        let size = u64::from_le_bytes(data.get(position..position + 8)?.try_into().ok()?);
        let start = position + 16;
        let end = start.checked_add(usize::try_from(size).ok()?)?;
        let payload = data.get(start..end)?;
        let mut d = BinaryDeserializer::new(payload);
        let reader: &mut dyn Deserializer = &mut d;
        reader.on_object_begin().ok()?;
        let wal_type: u8 = reader.read_property(100, "wal_type").ok()?;
        result.push(Entry { wal_type, payload });
        position = end;
    }
    Some(result)
}

/// A catalog entry that did not survive the round trip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mismatch {
    /// What the entry is (`CREATE TABLE_ENTRY`), or `unreadable`.
    pub kind: String,
    pub detail: String,
}

/// Round-trips the catalog entry of a WAL entry (a `CreateInfo` or `AlterInfo`) through
/// roastedduck's parse tree: `None` for entries that hold none, else the entry's kind or
/// how it differs.
pub fn check_entry(
    entry: &Entry<'_>,
    compatibility: &SerializationCompatibility,
) -> Option<Result<String, Mismatch>> {
    if CREATE_ENTRY_TYPES.iter().any(|(t, _)| *t == entry.wal_type) {
        Some(round_trip::<CreateInfo>(
            entry.payload,
            compatibility,
            describe_create,
        ))
    } else if entry.wal_type == ALTER_INFO {
        Some(round_trip::<ParseInfo>(
            entry.payload,
            compatibility,
            describe_parse_info,
        ))
    } else {
        None
    }
}

/// Reads property 101 of a WAL entry as `T`, writes it back, and compares the bytes.
fn round_trip<T: Pointee + DeserializePointee>(
    payload: &[u8],
    compatibility: &SerializationCompatibility,
    describe: impl Fn(&T) -> String,
) -> Result<String, Mismatch> {
    let read_error = |e: roastedduck_common::Error| Mismatch {
        kind: "unreadable".to_owned(),
        detail: e.to_string(),
    };
    let mut deserializer = BinaryDeserializer::new(payload);
    deserializer.on_object_begin().map_err(read_error)?;
    (&mut deserializer as &mut dyn Deserializer)
        .read_property::<u8>(100, "wal_type")
        .map_err(read_error)?;
    deserializer.on_property_begin(101, "info").map_err(read_error)?;
    let start = deserializer.position();
    let value: T = deserialize_present(&mut deserializer, "WAL entry").map_err(read_error)?;
    let original = &payload[start..deserializer.position()];
    let kind = describe(&value);
    let mut serializer = BinarySerializer::new(SerializationOptions {
        compatibility: compatibility.clone(),
        ..SerializationOptions::default()
    });
    serialize_present(&value, &mut serializer).map_err(|e| Mismatch {
        kind: kind.clone(),
        detail: e.to_string(),
    })?;
    let written = serializer.bytes();
    if written == original {
        return Ok(kind);
    }
    let at = written
        .iter()
        .zip(original)
        .position(|(a, b)| a != b)
        .unwrap_or(written.len().min(original.len()));
    let context = |bytes: &[u8]| {
        format!(
            "{:02x?}",
            &bytes[at.saturating_sub(8)..(at + 24).min(bytes.len())]
        )
    };
    Err(Mismatch {
        kind,
        detail: format!(
            "first difference at byte {at} of {} (written {})\n  expected {}\n  actual   {}",
            original.len(),
            written.len(),
            context(original),
            context(written)
        ),
    })
}

fn describe_create(info: &CreateInfo) -> String {
    format!("CREATE {}", info.catalog_type().name())
}

fn describe_parse_info(info: &ParseInfo) -> String {
    match info {
        ParseInfo::Alter(alter) => match &alter.kind {
            AlterInfoKind::AlterTable(table) => format!("ALTER TABLE {}", table.alter_table_type().name()),
            other => format!("ALTER {}", other.alter_type().name()),
        },
        other => other.parse_info_type().name().to_owned(),
    }
}
