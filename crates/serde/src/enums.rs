//! Enums: written by name in JSON, by number in the binary format.

use rustdb_common::{Error, Result};

use crate::{Deserializer, Serializer};

/// Writes an enum: by name when the serializer stores enums as strings, else by number.
pub fn write_enum(
    serializer: &mut dyn Serializer,
    name: &str,
    write_number: impl FnOnce(&mut dyn Serializer),
) {
    if serializer.options().serialize_enum_as_string {
        serializer.write_str(name);
    } else {
        write_number(serializer);
    }
}

/// Reads an enum stored either by name (case-insensitive) or by number.
pub fn read_enum<E>(
    deserializer: &mut dyn Deserializer,
    enum_name: &str,
    from_name: impl FnOnce(&str) -> Option<E>,
    read_number: impl FnOnce(&mut dyn Deserializer) -> Result<Option<E>>,
) -> Result<E> {
    if deserializer.deserialize_enum_from_string() {
        let text = deserializer.read_string()?;
        return from_name(&text).ok_or_else(|| {
            Error::not_implemented(format!(
                "Enum value: unrecognized value \"{text}\" for enum \"{enum_name}\""
            ))
        });
    }
    read_number(deserializer)?.ok_or_else(|| {
        Error::serialization(format!(
            "Failed to deserialize: invalid value for enum \"{enum_name}\""
        ))
    })
}
