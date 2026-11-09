//! Default values that `WritePropertyWithDefault` omits (DuckDB's
//! `SerializationDefaultValue`).

use std::collections::{BTreeMap, BTreeSet};

use roastedduck_common::{ByteString, CaseInsensitiveMap, CaseInsensitiveSet, OrderedMap};

use crate::OptionalIdx;

/// A value with a default that `WritePropertyWithDefault` leaves out: zero, the empty
/// string or container, NULL pointers, the invalid `optional_idx`.
pub trait DefaultValue {
    /// Whether this is the default (DuckDB's `IsDefault`).
    fn is_default(&self) -> bool;
    /// The default, used when the property is absent (DuckDB's `GetDefault`).
    fn default_value() -> Self
    where
        Self: Sized;
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

impl DefaultValue for String {
    fn is_default(&self) -> bool {
        self.is_empty()
    }

    fn default_value() -> Self {
        Self::new()
    }
}

impl DefaultValue for ByteString {
    fn is_default(&self) -> bool {
        self.is_empty()
    }

    fn default_value() -> Self {
        Self::new()
    }
}

// The borrowed forms written by the property structs of hand-written types (the logical
// plan's); they are never read, so they have no `default_value`.
impl DefaultValue for str {
    fn is_default(&self) -> bool {
        self.is_empty()
    }
}

impl<T> DefaultValue for [T] {
    fn is_default(&self) -> bool {
        self.is_empty()
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

impl<T> DefaultValue for Option<T> {
    fn is_default(&self) -> bool {
        self.is_none()
    }

    fn default_value() -> Self {
        None
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

impl<T> DefaultValue for BTreeSet<T> {
    fn is_default(&self) -> bool {
        self.is_empty()
    }

    fn default_value() -> Self {
        Self::new()
    }
}

impl<K, V> DefaultValue for BTreeMap<K, V> {
    fn is_default(&self) -> bool {
        self.is_empty()
    }

    fn default_value() -> Self {
        Self::new()
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

impl<V, K> DefaultValue for CaseInsensitiveMap<V, K> {
    fn is_default(&self) -> bool {
        self.is_empty()
    }

    fn default_value() -> Self {
        Self::new_with_keys()
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
