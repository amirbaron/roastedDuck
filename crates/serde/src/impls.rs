//! (De)serialization of primitives, containers and the shared index/collection types,
//! mirroring the `WriteValue` / `Read<T>` overloads of DuckDB's (de)serializer.

use std::collections::{BTreeMap, BTreeSet};

use rustdb_common::{
    CaseInsensitiveMap, CaseInsensitiveSet, LogicalIndex, OrderedMap, PhysicalIndex, Result,
};

use crate::{Deserialize, Deserializer, Serialize, Serializer};

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
/// An index held as `usize`, written as DuckDB's `idx_t`.
impl Serialize for usize {
    fn serialize(&self, serializer: &mut dyn Serializer) -> Result<()> {
        serializer.write_u64(*self as u64);
        Ok(())
    }
}

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

/// DuckDB's `IndexVector<T, INDEX_TYPE>`: an object holding the vector as
/// `internal_vector`.
#[derive(Clone, Copy, Debug)]
pub struct IndexVector<'a, T>(pub &'a [T]);

impl<T: Serialize> Serialize for IndexVector<'_, T> {
    fn serialize(&self, serializer: &mut dyn Serializer) -> Result<()> {
        serializer.on_object_begin();
        serializer.write_property(100, "internal_vector", self.0)?;
        serializer.on_object_end();
        Ok(())
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

/// `std::map`: written in key order.
impl<K: Serialize, V: Serialize> Serialize for BTreeMap<K, V> {
    fn serialize(&self, serializer: &mut dyn Serializer) -> Result<()> {
        write_map_entries(serializer, self.iter())
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
