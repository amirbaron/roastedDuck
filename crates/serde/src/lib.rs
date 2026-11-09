//! DuckDB's serialization framework.
//!
//! DuckDB persists catalog entries, WAL records and plans, and prints parse trees, through
//! one property-based framework: every class writes numbered, named properties
//! (`Serializer::WriteProperty(100, "name", value)`), and a backend decides the encoding.
//! This crate ports that framework and its two backends:
//!
//! - [`binary`]: the compact format of the WAL and the database file, byte-identical to
//!   DuckDB's `BinarySerializer`.
//! - [`json`]: the format of `json_serialize_sql`, byte-identical to the json extension's
//!   `JsonSerializer` output.
//!
//! Types implement [`Serialize`]/[`Deserialize`] (values) or
//! [`SerializeObject`]/[`DeserializeObject`] (classes made of properties); most
//! implementations are generated from DuckDB's own `*.json` serialization specs by
//! `tools/codegen`.

pub mod binary;
mod default_value;
mod deserializer;
mod enums;
mod generated;
mod impls;
pub mod json;
mod options;
mod pointee;
mod serializer;

pub use default_value::DefaultValue;
pub use deserializer::{Deserialize, DeserializeObject, Deserializer};
pub use enums::{read_enum, write_enum};
pub use impls::{IndexVector, OptionalIdx};
pub use options::{SerializationCompatibility, SerializationOptions};
pub use pointee::{DeserializePointee, Pointee, deserialize_present, required, serialize_present};
pub use roastedduck_common::{Error, Result};
pub use serializer::{Serialize, SerializeObject, Serializer};

/// Numeric id of a property within an object (DuckDB's `field_id_t`).
pub type FieldId = u16;

/// Field id terminating every object in the binary format.
pub const MESSAGE_TERMINATOR_FIELD_ID: FieldId = 0xFFFF;

/// Implements [`Serialize`] and [`Deserialize`] for a class by wrapping its
/// [`SerializeObject`]/[`DeserializeObject`] properties in an object, which is how DuckDB
/// writes any class with a `Serialize` method.
#[macro_export]
macro_rules! impl_object_serde {
    ($($ty:ty),* $(,)?) => {
        $(
            impl $crate::Serialize for $ty {
                fn serialize(&self, serializer: &mut dyn $crate::Serializer) -> $crate::Result<()> {
                    serializer.write_object(self)
                }
            }

            impl $crate::Deserialize for $ty {
                fn deserialize(deserializer: &mut dyn $crate::Deserializer) -> $crate::Result<Self> {
                    deserializer.read_object()
                }
            }
        )*
    };
}

/// Implements [`Pointee`] for a class from its [`SerializeObject`]/[`DeserializeObject`]
/// impls, plus [`Serialize`]/[`Deserialize`] for non-NULL values (DuckDB classes that are
/// always held by `unique_ptr`).
#[macro_export]
macro_rules! impl_pointee_serde {
    ($($ty:ty),* $(,)?) => {
        $(
            impl $crate::Pointee for $ty {
                fn serialize_pointee(&self, serializer: &mut dyn $crate::Serializer) -> $crate::Result<()> {
                    serializer.write_object(self)
                }
            }

            impl $crate::DeserializePointee for $ty {
                fn deserialize_pointee(deserializer: &mut dyn $crate::Deserializer) -> $crate::Result<Self> {
                    deserializer.read_object()
                }
            }

            impl $crate::Serialize for $ty {
                fn serialize(&self, serializer: &mut dyn $crate::Serializer) -> $crate::Result<()> {
                    $crate::serialize_present(self, serializer)
                }
            }

            impl $crate::Deserialize for $ty {
                fn deserialize(deserializer: &mut dyn $crate::Deserializer) -> $crate::Result<Self> {
                    $crate::deserialize_present(deserializer, stringify!($ty))
                }
            }
        )*
    };
}

/// Implements [`Pointee`] and [`Serialize`] for a class from its [`SerializeObject`] impl:
/// the write-only form of [`impl_pointee_serde!`], for classes that are never read back
/// (the logical plan's operators and expressions).
#[macro_export]
macro_rules! impl_pointee_serialize {
    ($($ty:ty),* $(,)?) => {
        $(
            impl $crate::Pointee for $ty {
                fn serialize_pointee(&self, serializer: &mut dyn $crate::Serializer) -> $crate::Result<()> {
                    serializer.write_object(self)
                }
            }

            impl $crate::Serialize for $ty {
                fn serialize(&self, serializer: &mut dyn $crate::Serializer) -> $crate::Result<()> {
                    $crate::serialize_present(self, serializer)
                }
            }
        )*
    };
}
