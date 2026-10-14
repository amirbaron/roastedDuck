//! (De)serialization of primitives, containers and the shared index/collection types,
//! mirroring the `WriteValue` / `Read<T>` overloads of DuckDB's (de)serializer.

use std::collections::BTreeSet;
use std::sync::Arc;

use rustdb_common::{
    CaseInsensitiveMap, CaseInsensitiveSet, Error, LogicalIndex, OrderedMap, PhysicalIndex, Result,
};

use crate::{Deserialize, Deserializer, Serialize, Serializer};

/// Values with a "default" that `WritePropertyWithDefault` may omit
/// (DuckDB's `SerializationDefaultValue`).
pub trait DefaultValue {
    fn is_default(&self) -> bool;
    fn default_value() -> Self
    where
        Self: Sized;
}

/// DuckDB's `optional_idx`: written as an `idx_t`, with `None` as `INVALID_INDEX`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct OptionalIdx(pub Option<u64>);

macro_rules! primitive {
    ($ty:ty, $write:ident, $read:ident) => {
        impl Serialize for $ty {
            fn serialize(&self, serializer: &mut dyn Serializer) -> Result<()> {
                serializer.$write(*self);
                Ok(())
            }
        }

        impl Deserialize for $ty {
            fn deserialize(deserializer: &mut dyn Deserializer) -> Result<Self> {
                deserializer.$read()
            }
        }
    };
}

macro_rules! zero_default {
    ($($ty:ty),*) => {
        $(impl DefaultValue for $ty {
            fn is_default(&self) -> bool {
                *self == <$ty>::default()
            }

            fn default_value() -> Self {
                <$ty>::default()
            }
        })*
    };
}

primitive!(bool, write_bool, read_bool);
primitive!(u8, write_u8, read_u8);
primitive!(i8, write_i8, read_i8);
primitive!(u16, write_u16, read_u16);
primitive!(i16, write_i16, read_i16);
primitive!(u32, write_u32, read_u32);
primitive!(i32, write_i32, read_i32);
primitive!(u64, write_u64, read_u64);
primitive!(i64, write_i64, read_i64);
primitive!(i128, write_i128, read_i128);
primitive!(u128, write_u128, read_u128);
primitive!(f32, write_f32, read_f32);
primitive!(f64, write_f64, read_f64);
zero_default!(bool, u8, i8, u16, i16, u32, i32, u64, i64);

macro_rules! float_default {
    ($($ty:ty),*) => {
        $(impl DefaultValue for $ty {
            // DuckDB compares against exactly zero; `-0.0` counts as default too.
            #[allow(clippy::float_cmp)]
            fn is_default(&self) -> bool {
                *self == 0.0
            }

            fn default_value() -> Self {
                0.0
            }
        })*
    };
}

float_default!(f32, f64);

impl Serialize for str {
    fn serialize(&self, serializer: &mut dyn Serializer) -> Result<()> {
        serializer.write_str(self);
        Ok(())
    }
}

impl Serialize for String {
    fn serialize(&self, serializer: &mut dyn Serializer) -> Result<()> {
        serializer.write_str(self);
        Ok(())
    }
}

impl Deserialize for String {
    fn deserialize(deserializer: &mut dyn Deserializer) -> Result<Self> {
        deserializer.read_string()
    }
}

impl DefaultValue for String {
    fn is_default(&self) -> bool {
        self.is_empty()
    }

    fn default_value() -> Self {
        Self::new()
    }
}

impl Serialize for OptionalIdx {
    fn serialize(&self, serializer: &mut dyn Serializer) -> Result<()> {
        serializer.write_u64(self.0.unwrap_or(u64::MAX));
        Ok(())
    }
}

impl Deserialize for OptionalIdx {
    fn deserialize(deserializer: &mut dyn Deserializer) -> Result<Self> {
        let index = deserializer.read_u64()?;
        Ok(Self((index != u64::MAX).then_some(index)))
    }
}

impl DefaultValue for OptionalIdx {
    fn is_default(&self) -> bool {
        self.0.is_none()
    }

    fn default_value() -> Self {
        Self(None)
    }
}

impl Serialize for LogicalIndex {
    fn serialize(&self, serializer: &mut dyn Serializer) -> Result<()> {
        serializer.write_u64(self.0);
        Ok(())
    }
}

impl Deserialize for LogicalIndex {
    fn deserialize(deserializer: &mut dyn Deserializer) -> Result<Self> {
        deserializer.read_u64().map(Self)
    }
}

impl Serialize for PhysicalIndex {
    fn serialize(&self, serializer: &mut dyn Serializer) -> Result<()> {
        serializer.write_u64(self.0);
        Ok(())
    }
}

impl Deserialize for PhysicalIndex {
    fn deserialize(deserializer: &mut dyn Deserializer) -> Result<Self> {
        deserializer.read_u64().map(Self)
    }
}

impl<T: Serialize + ?Sized> Serialize for &T {
    fn serialize(&self, serializer: &mut dyn Serializer) -> Result<()> {
        (**self).serialize(serializer)
    }
}

impl<T: Serialize + ?Sized> Serialize for Box<T> {
    fn serialize(&self, serializer: &mut dyn Serializer) -> Result<()> {
        (**self).serialize(serializer)
    }
}

impl<T: Deserialize> Deserialize for Box<T> {
    fn deserialize(deserializer: &mut dyn Deserializer) -> Result<Self> {
        T::deserialize(deserializer).map(Box::new)
    }
}

/// A class DuckDB holds through a pointer (`unique_ptr` / `shared_ptr`).
///
/// Pointers are written as nullable values: a presence flag, then the object. Types
/// implement the object part here; `Option<T>` adds the flag (absent for `None`), and the
/// `Serialize`/`Deserialize` impls generated by [`impl_pointee_serde!`](crate::impl_pointee_serde)
/// add it for values that are always present (vector elements, required children).
pub trait Pointee: Sized {
    fn serialize_pointee(&self, serializer: &mut dyn Serializer) -> Result<()>;
    fn deserialize_pointee(deserializer: &mut dyn Deserializer) -> Result<Self>;
}

impl<T: Pointee> Pointee for Box<T> {
    fn serialize_pointee(&self, serializer: &mut dyn Serializer) -> Result<()> {
        (**self).serialize_pointee(serializer)
    }

    fn deserialize_pointee(deserializer: &mut dyn Deserializer) -> Result<Self> {
        T::deserialize_pointee(deserializer).map(Box::new)
    }
}

impl<T: Pointee> Pointee for Arc<T> {
    fn serialize_pointee(&self, serializer: &mut dyn Serializer) -> Result<()> {
        (**self).serialize_pointee(serializer)
    }

    fn deserialize_pointee(deserializer: &mut dyn Deserializer) -> Result<Self> {
        T::deserialize_pointee(deserializer).map(Arc::new)
    }
}

/// `Option` is a nullable pointer.
impl<T: Pointee> Serialize for Option<T> {
    fn serialize(&self, serializer: &mut dyn Serializer) -> Result<()> {
        serializer.on_nullable_begin(self.is_some());
        if let Some(value) = self {
            value.serialize_pointee(serializer)?;
        }
        serializer.on_nullable_end();
        Ok(())
    }
}

impl<T: Pointee> Deserialize for Option<T> {
    fn deserialize(deserializer: &mut dyn Deserializer) -> Result<Self> {
        let present = deserializer.on_nullable_begin()?;
        let value = if present {
            Some(T::deserialize_pointee(deserializer)?)
        } else {
            None
        };
        deserializer.on_nullable_end()?;
        Ok(value)
    }
}

/// Writes a pointer that is known to be non-NULL.
pub fn serialize_present<T: Pointee>(value: &T, serializer: &mut dyn Serializer) -> Result<()> {
    serializer.on_nullable_begin(true);
    value.serialize_pointee(serializer)?;
    serializer.on_nullable_end();
    Ok(())
}

/// Reads a pointer that must not be NULL.
pub fn deserialize_present<T: Pointee>(deserializer: &mut dyn Deserializer, type_name: &str) -> Result<T> {
    if !deserializer.on_nullable_begin()? {
        return Err(Error::serialization(format!(
            "Failed to deserialize: unexpected NULL {type_name}"
        )));
    }
    let value = T::deserialize_pointee(deserializer)?;
    deserializer.on_nullable_end()?;
    Ok(value)
}

/// Unwraps a pointer property that must be present and non-NULL.
pub fn required<T>(value: Option<T>, class: &str, tag: &str) -> Result<T> {
    value.ok_or_else(|| {
        Error::serialization(format!(
            "Failed to deserialize: property \"{tag}\" of {class} must not be NULL"
        ))
    })
}

impl<T> DefaultValue for Option<T> {
    fn is_default(&self) -> bool {
        self.is_none()
    }

    fn default_value() -> Self {
        None
    }
}

impl<T: Serialize> Serialize for [T] {
    fn serialize(&self, serializer: &mut dyn Serializer) -> Result<()> {
        serializer.write_list(self.iter())
    }
}

impl<T: Serialize> Serialize for Vec<T> {
    fn serialize(&self, serializer: &mut dyn Serializer) -> Result<()> {
        serializer.write_list(self.iter())
    }
}

impl<T: Deserialize> Deserialize for Vec<T> {
    fn deserialize(deserializer: &mut dyn Deserializer) -> Result<Self> {
        let count = deserializer.on_list_begin()?;
        let mut result = Self::with_capacity(count.min(1024));
        for _ in 0..count {
            result.push(T::deserialize(deserializer)?);
        }
        deserializer.on_list_end()?;
        Ok(result)
    }
}

impl<T> DefaultValue for Vec<T> {
    fn is_default(&self) -> bool {
        self.is_empty()
    }

    fn default_value() -> Self {
        Self::new()
    }
}

/// `std::set`: written as a list in sorted order.
impl<T: Serialize> Serialize for BTreeSet<T> {
    fn serialize(&self, serializer: &mut dyn Serializer) -> Result<()> {
        serializer.write_list(self.iter())
    }
}

impl<T: Deserialize + Ord> Deserialize for BTreeSet<T> {
    fn deserialize(deserializer: &mut dyn Deserializer) -> Result<Self> {
        let count = deserializer.on_list_begin()?;
        let mut result = Self::new();
        for _ in 0..count {
            result.insert(T::deserialize(deserializer)?);
        }
        deserializer.on_list_end()?;
        Ok(result)
    }
}

impl<T> DefaultValue for BTreeSet<T> {
    fn is_default(&self) -> bool {
        self.is_empty()
    }

    fn default_value() -> Self {
        Self::new()
    }
}

/// `std::pair`: an object with `first` and `second`.
impl<A: Serialize, B: Serialize> Serialize for (A, B) {
    fn serialize(&self, serializer: &mut dyn Serializer) -> Result<()> {
        serializer.on_object_begin();
        serializer.write_property(0, "first", &self.0)?;
        serializer.write_property(1, "second", &self.1)?;
        serializer.on_object_end();
        Ok(())
    }
}

impl<A: Deserialize, B: Deserialize> Deserialize for (A, B) {
    fn deserialize(deserializer: &mut dyn Deserializer) -> Result<Self> {
        deserializer.on_object_begin()?;
        let first = deserializer.read_property(0, "first")?;
        let second = deserializer.read_property(1, "second")?;
        deserializer.on_object_end()?;
        Ok((first, second))
    }
}

fn write_map_entries<'a, K: Serialize + ?Sized + 'a, V: Serialize + 'a>(
    serializer: &mut dyn Serializer,
    entries: impl ExactSizeIterator<Item = (&'a K, &'a V)>,
) -> Result<()> {
    serializer.on_list_begin(entries.len());
    for (key, value) in entries {
        serializer.on_object_begin();
        serializer.write_property(0, "key", key)?;
        serializer.write_property(1, "value", value)?;
        serializer.on_object_end();
    }
    serializer.on_list_end();
    Ok(())
}

fn read_map_entries<K: Deserialize, V: Deserialize>(
    deserializer: &mut dyn Deserializer,
    mut insert: impl FnMut(K, V),
) -> Result<()> {
    let count = deserializer.on_list_begin()?;
    for _ in 0..count {
        deserializer.on_object_begin()?;
        let key = deserializer.read_property(0, "key")?;
        let value = deserializer.read_property(1, "value")?;
        deserializer.on_object_end()?;
        insert(key, value);
    }
    deserializer.on_list_end()
}

/// Maps are written as a list of `{key, value}` objects in iteration order.
impl<K: Serialize, V: Serialize> Serialize for OrderedMap<K, V> {
    fn serialize(&self, serializer: &mut dyn Serializer) -> Result<()> {
        write_map_entries(serializer, self.iter())
    }
}

impl<K: Deserialize + PartialEq, V: Deserialize> Deserialize for OrderedMap<K, V> {
    fn deserialize(deserializer: &mut dyn Deserializer) -> Result<Self> {
        let mut map = Self::new();
        read_map_entries(deserializer, |k, v| {
            map.insert(k, v);
        })?;
        Ok(map)
    }
}

impl<K, V> DefaultValue for OrderedMap<K, V> {
    fn is_default(&self) -> bool {
        self.is_empty()
    }

    fn default_value() -> Self {
        Self::new()
    }
}

impl<V: Serialize> Serialize for CaseInsensitiveMap<V> {
    fn serialize(&self, serializer: &mut dyn Serializer) -> Result<()> {
        write_map_entries(serializer, self.iter())
    }
}

impl<V: Deserialize> Deserialize for CaseInsensitiveMap<V> {
    fn deserialize(deserializer: &mut dyn Deserializer) -> Result<Self> {
        let mut map = Self::new();
        read_map_entries(deserializer, |k: String, v| {
            map.insert(k, v);
        })?;
        Ok(map)
    }
}

impl<V> DefaultValue for CaseInsensitiveMap<V> {
    fn is_default(&self) -> bool {
        self.is_empty()
    }

    fn default_value() -> Self {
        Self::new()
    }
}

impl Serialize for CaseInsensitiveSet {
    fn serialize(&self, serializer: &mut dyn Serializer) -> Result<()> {
        serializer.on_list_begin(self.len());
        for value in self.iter() {
            serializer.write_str(value);
        }
        serializer.on_list_end();
        Ok(())
    }
}

impl Deserialize for CaseInsensitiveSet {
    fn deserialize(deserializer: &mut dyn Deserializer) -> Result<Self> {
        let mut set = Self::new();
        let count = deserializer.on_list_begin()?;
        for _ in 0..count {
            set.insert(deserializer.read_string()?);
        }
        deserializer.on_list_end()?;
        Ok(set)
    }
}

impl DefaultValue for CaseInsensitiveSet {
    fn is_default(&self) -> bool {
        self.is_empty()
    }

    fn default_value() -> Self {
        Self::new()
    }
}

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
