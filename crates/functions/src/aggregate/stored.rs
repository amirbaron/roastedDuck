//! Owned copies of input values kept in aggregate states, and their ordering (DuckDB's
//! `MinMaxFixedValue`, `MinMaxStringValue`, `MinMaxFallbackValue` and
//! `sort_key_helpers.hpp`).
//!
//! Fixed-width values are stored as themselves, strings as owned bytes, and every other
//! type (LIST, STRUCT, ARRAY, ...) as its normalized sort key, which orders like the value
//! and decodes back into it.

use std::cmp::Ordering;
use std::marker::PhantomData;

use rustdb_common::Result;
use rustdb_row::sort::{
    NullOrder, OrderModifiers, OrderType, SortKeyBuffer, create_sort_keys, decode_sort_key,
};
use rustdb_types::compare::SqlOrd;
use rustdb_types::{Interval, NativeType, StringT, Unified, Vector};

/// The SQL order of a fixed-width type (DuckDB's `LessThan`/`GreaterThan` operators).
pub(crate) trait FixedOrd: NativeType {
    fn sql_cmp(self, other: Self) -> Ordering;

    #[inline]
    fn sql_less(self, other: Self) -> bool {
        self.sql_cmp(other) == Ordering::Less
    }
}

macro_rules! fixed_ord_sql {
    ($($ty:ty),*) => {$(
        impl FixedOrd for $ty {
            #[inline]
            fn sql_cmp(self, other: Self) -> Ordering {
                if self.sql_gt(other) {
                    Ordering::Greater
                } else if other.sql_gt(self) {
                    Ordering::Less
                } else {
                    Ordering::Equal
                }
            }

            #[inline]
            fn sql_less(self, other: Self) -> bool {
                other.sql_gt(self)
            }
        }
    )*};
}
fixed_ord_sql!(bool, i8, i16, i32, i64, i128, u8, u16, u32, u64, u128, f32, f64);

impl FixedOrd for Interval {
    /// Intervals compare after normalisation (DuckDB's `Interval::GreaterThan`).
    fn sql_cmp(self, other: Self) -> Ordering {
        self.normalized_parts().cmp(&other.normalized_parts())
    }
}

/// How an aggregate state stores input values of some physical representation.
pub(crate) trait StoredKind: Send + Sync + 'static {
    /// The owned copy kept in a state.
    type Owned: Clone + Default + Send + 'static;
    /// A borrowed input value.
    type Ref<'a>: Copy;
    /// A chunk of input prepared for reading values.
    type Input<'a>;

    /// Prepares the first `count` rows of `vector`.
    fn prepare(vector: &Vector, count: usize) -> Result<Self::Input<'_>>;

    /// The value of row `row`, or `None` if it is NULL.
    fn get<'a>(input: &'a Self::Input<'_>, row: usize) -> Option<Self::Ref<'a>>;

    fn to_owned(value: Self::Ref<'_>) -> Self::Owned;

    /// Overwrites `target` with `value`, reusing its allocation where possible.
    fn assign(target: &mut Self::Owned, value: Self::Ref<'_>) {
        *target = Self::to_owned(value);
    }

    fn borrow(owned: &Self::Owned) -> Self::Ref<'_>;

    /// The order used by MIN/MAX-like aggregates.
    fn cmp(left: Self::Ref<'_>, right: Self::Ref<'_>) -> Ordering;

    /// Writes `value` into row `row` of a flat `result`.
    fn write(value: &Self::Owned, result: &mut Vector, row: usize) -> Result<()>;
}

/// Fixed-width values, stored by value.
pub(crate) struct Fixed<T>(PhantomData<fn() -> T>);

impl<T: FixedOrd> StoredKind for Fixed<T> {
    type Owned = T;
    type Ref<'a> = T;
    type Input<'a> = Unified<'a, T>;

    fn prepare(vector: &Vector, _: usize) -> Result<Unified<'_, T>> {
        Ok(vector.unified::<T>())
    }

    #[inline]
    fn get(input: &Unified<'_, T>, row: usize) -> Option<T> {
        input.get(row)
    }

    #[inline]
    fn to_owned(value: T) -> T {
        value
    }

    #[inline]
    fn borrow(owned: &T) -> T {
        *owned
    }

    #[inline]
    fn cmp(left: T, right: T) -> Ordering {
        left.sql_cmp(right)
    }

    fn write(value: &T, result: &mut Vector, row: usize) -> Result<()> {
        result.values_mut::<T>()[row] = *value;
        Ok(())
    }
}

/// VARCHAR/BLOB/BIT values, stored as owned bytes and compared byte-wise.
pub(crate) struct Bytes;

/// A string column prepared for reading.
pub(crate) struct StringInput<'a> {
    vector: &'a Vector,
    values: Unified<'a, StringT>,
}

impl StoredKind for Bytes {
    type Owned = Vec<u8>;
    type Ref<'a> = &'a [u8];
    type Input<'a> = StringInput<'a>;

    fn prepare(vector: &Vector, _: usize) -> Result<StringInput<'_>> {
        Ok(StringInput {
            vector,
            values: vector.unified::<StringT>(),
        })
    }

    #[inline]
    fn get<'a>(input: &'a StringInput<'_>, row: usize) -> Option<&'a [u8]> {
        let slot = input.values.slot(row);
        input
            .values
            .validity
            .is_valid(slot)
            .then(|| input.vector.string_value(&input.values.values[slot]))
    }

    fn to_owned(value: &[u8]) -> Vec<u8> {
        value.to_vec()
    }

    fn assign(target: &mut Vec<u8>, value: &[u8]) {
        target.clear();
        target.extend_from_slice(value);
    }

    fn borrow(owned: &Vec<u8>) -> &[u8] {
        owned
    }

    #[inline]
    fn cmp(left: &[u8], right: &[u8]) -> Ordering {
        left.cmp(right)
    }

    fn write(value: &Vec<u8>, result: &mut Vector, row: usize) -> Result<()> {
        let string = result.add_string(value);
        result.values_mut::<StringT>()[row] = string;
        Ok(())
    }
}

/// Any other type, stored as its normalized sort key (`DESC` keys order descending).
pub(crate) struct SortKey<const DESC: bool>;

impl<const DESC: bool> SortKey<DESC> {
    pub(crate) const MODIFIERS: OrderModifiers = OrderModifiers::new(
        if DESC {
            OrderType::Descending
        } else {
            OrderType::Ascending
        },
        NullOrder::NullsLast,
    );
}

/// Sort keys of a chunk plus the validity of its rows.
pub(crate) struct KeyInput<'a> {
    vector: &'a Vector,
    keys: SortKeyBuffer,
}

impl<const DESC: bool> StoredKind for SortKey<DESC> {
    type Owned = Vec<u8>;
    type Ref<'a> = &'a [u8];
    type Input<'a> = KeyInput<'a>;

    fn prepare(vector: &Vector, count: usize) -> Result<KeyInput<'_>> {
        let keys = create_sort_keys(&[vector], &[Self::MODIFIERS], count)?;
        Ok(KeyInput { vector, keys })
    }

    #[inline]
    fn get<'a>(input: &'a KeyInput<'_>, row: usize) -> Option<&'a [u8]> {
        (!input.vector.is_null(row)).then(|| input.keys.key(row))
    }

    fn to_owned(value: &[u8]) -> Vec<u8> {
        value.to_vec()
    }

    fn assign(target: &mut Vec<u8>, value: &[u8]) {
        target.clear();
        target.extend_from_slice(value);
    }

    fn borrow(owned: &Vec<u8>) -> &[u8] {
        owned
    }

    #[inline]
    fn cmp(left: &[u8], right: &[u8]) -> Ordering {
        left.cmp(right)
    }

    fn write(value: &Vec<u8>, result: &mut Vector, row: usize) -> Result<()> {
        decode_sort_key(value, std::slice::from_mut(result), row, &[Self::MODIFIERS]).map(|_| ())
    }
}

/// Dispatches `$body` with `$K` bound to the [`StoredKind`] for values of `$ty`; `$desc`
/// selects descending sort keys for types stored as keys.
macro_rules! with_stored_kind {
    ($ty:expr, $desc:literal, $K:ident => $body:expr) => {{
        use crate::aggregate::stored::{Bytes, Fixed, SortKey};
        use rustdb_types::PhysicalType as P;
        match $ty.physical_type() {
            P::Varchar => {
                type $K = Bytes;
                $body
            }
            P::Interval => {
                type $K = Fixed<rustdb_types::Interval>;
                $body
            }
            P::List | P::Struct | P::Array | P::Invalid | P::Unknown | P::Bit => {
                type $K = SortKey<$desc>;
                $body
            }
            physical => crate::dispatch::with_numeric_type!(physical, T => {
                type $K = Fixed<T>;
                $body
            }, _ => {
                type $K = SortKey<$desc>;
                $body
            }),
        }
    }};
}

pub(crate) use with_stored_kind;

/// A stored value ordered by its kind's SQL order, usable as a map key (DuckDB keys
/// `std::map`s with the type's `operator<`).
pub(crate) struct OrdKey<K: StoredKind>(pub(crate) K::Owned);

impl<K: StoredKind> Clone for OrdKey<K> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<K: StoredKind> PartialEq for OrdKey<K> {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl<K: StoredKind> Eq for OrdKey<K> {}

impl<K: StoredKind> PartialOrd for OrdKey<K> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<K: StoredKind> Ord for OrdKey<K> {
    fn cmp(&self, other: &Self) -> Ordering {
        K::cmp(K::borrow(&self.0), K::borrow(&other.0))
    }
}
