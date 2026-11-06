//! Casting between types (DuckDB's `src/function/cast`: `CastFunctionSet`, `DefaultCasts`,
//! `BoundCastInfo`, `CastParameters`).
//!
//! A cast is bound once per (source, target) pair into a [`BoundCastInfo`], whose kernel
//! converts whole vectors. Errors follow DuckDB exactly: under `CAST` the first failing row
//! raises a `Conversion Error` with DuckDB's message, under `TRY_CAST` failing rows become
//! NULL and the first message is kept in [`CastParameters`].
//!
//! The binder resolves overloads with [`CastFunctionSet::implicit_cast_cost`] (DuckDB's
//! `CastRules::ImplicitCast` plus registered casts) and binds casts with
//! [`CastFunctionSet::get_cast_function`].

mod bignum;
mod binary;
mod compare;
mod decimal;
mod enums;
pub mod executor;
mod geometry;
mod nested;
pub mod numeric;
mod parse;
mod split;
mod string;
mod string_nested;
mod structs;
mod temporal;
pub mod text_parse;
mod value;
mod variant;

use std::fmt;
use std::sync::Arc;

use rustdb_common::{Error, Result};
use rustdb_types::{LogicalType, LogicalTypeId, Vector};

pub use compare::{
    values_are_equal, values_distinct_greater_than, values_equal, values_greater_than, values_not_distinct,
    variant_order,
};
pub use rustdb_types::implicit_cast_cost;
pub use value::{ValueCast, cast_vector};

/// Per-execution settings and error state of a cast (DuckDB's `CastParameters`).
#[derive(Clone, Debug, Default)]
pub struct CastParameters {
    /// Strict parsing, as used by the CSV sniffer: no leading `+`, no leading zeros, no
    /// underscores or decimals in integers, ...
    pub strict: bool,
    /// The alias of the cast's source expression; error messages then end with
    /// `when casting from source column <alias>`.
    pub source_column: Option<rustdb_common::ByteString>,
    /// The cast expression's location, attached by `HandleCastError::AssignError`.
    /// Exceptions thrown directly by a cast do not acquire this location.
    pub query_location: Option<u64>,
    try_cast: bool,
    /// Under `TRY_CAST`, a list whose elements fail to convert becomes NULL itself, so
    /// the reader can report the row (the CSV scanner's `nullify_parent`).
    pub nullify_parent: bool,
    error_message: Option<Vec<u8>>,
}

impl CastParameters {
    /// `CAST` semantics: the first row that cannot be converted raises an error.
    pub fn new() -> Self {
        Self::default()
    }

    /// `TRY_CAST` semantics: rows that cannot be converted become NULL and the first
    /// error message is kept (see [`CastParameters::error_message`]).
    pub fn try_cast() -> Self {
        Self {
            try_cast: true,
            ..Self::default()
        }
    }

    /// Whether failing rows become NULL instead of raising an error.
    pub fn is_try_cast(&self) -> bool {
        self.try_cast
    }

    /// The first conversion error recorded under `TRY_CAST`.
    pub fn error_message(&self) -> Option<&[u8]> {
        self.error_message.as_deref()
    }

    /// Takes the recorded `TRY_CAST` error message, leaving none.
    pub fn take_error_message(&mut self) -> Option<Vec<u8>> {
        self.error_message.take()
    }

    /// Reports a conversion failure (DuckDB's `HandleCastError::AssignError`): returns the
    /// `Conversion Error` under `CAST`; records the first message under `TRY_CAST`.
    pub fn assign_error(&mut self, message: String) -> Result<()> {
        self.assign_error_bytes(message.into_bytes())
    }

    /// The byte-string variant of `HandleCastError::AssignError`, including messages
    /// supplied by native callbacks.
    pub fn assign_error_bytes(&mut self, mut message: Vec<u8>) -> Result<()> {
        if let Some(column) = &self.source_column {
            message.extend_from_slice(b" when casting from source column ");
            message.extend_from_slice(column.as_bytes());
        }
        if !self.try_cast {
            return Err(Error::new_bytes(rustdb_common::ErrorKind::Conversion, message)
                .with_position(self.query_location));
        }
        // DuckDB fills an empty error string; an empty first message can be replaced.
        if self.error_message.as_ref().is_none_or(Vec::is_empty) {
            self.error_message = Some(message);
        }
        Ok(())
    }
}

/// A vectorised cast kernel: converts the first `count` rows of `source` into `result`
/// (whose type is the cast target). Returns `Ok(false)` if some rows failed under
/// `TRY_CAST` (they are NULL in `result`).
pub type CastKernel = dyn Fn(&Vector, &mut Vector, usize, &mut CastParameters) -> Result<bool> + Send + Sync;

/// A cast bound to a (source, target) pair (DuckDB's `BoundCastInfo`). Nested casts
/// capture the bound casts of their children.
#[derive(Clone)]
pub struct BoundCastInfo {
    kernel: Arc<CastKernel>,
}

impl fmt::Debug for BoundCastInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BoundCastInfo").finish_non_exhaustive()
    }
}

impl BoundCastInfo {
    pub fn new(
        kernel: impl Fn(&Vector, &mut Vector, usize, &mut CastParameters) -> Result<bool> + Send + Sync + 'static,
    ) -> Self {
        Self {
            kernel: Arc::new(kernel),
        }
    }

    /// Casts the first `count` rows of `source` into `result`.
    pub fn execute(
        &self,
        source: &Vector,
        result: &mut Vector,
        count: usize,
        params: &mut CastParameters,
    ) -> Result<bool> {
        (self.kernel)(source, result, count, params)
    }

    /// The cast between identical types: the result references the source.
    pub fn nop() -> Self {
        Self::new(|source, result, _, _| {
            *result = source.clone();
            Ok(true)
        })
    }

    /// Reuses the source data under the target type (identical physical layout).
    pub fn reinterpret() -> Self {
        Self::new(|source, result, _, _| {
            *result = source.reinterpret(result.logical_type().clone())?;
            Ok(true)
        })
    }

    /// Casts from `NULL`-typed values: every row is NULL.
    pub fn null_source() -> Self {
        Self::new(|_, result, _, _| {
            *result = Vector::constant_null(result.logical_type().clone());
            Ok(true)
        })
    }

    /// The fallback for unsupported casts (DuckDB's `TryVectorNullCast`): only succeeds if
    /// every input row is NULL.
    pub fn unsupported(source: &LogicalType, target: &LogicalType) -> Self {
        let message = unimplemented_cast_message(source, target);
        Self::new(move |input, result, count, params| {
            let has_value = (0..count).any(|row| !input.is_null(row));
            *result = Vector::constant_null(result.logical_type().clone());
            if has_value {
                params.assign_error_bytes(message.clone().into_bytes())?;
                return Ok(false);
            }
            Ok(true)
        })
    }
}

/// `Unimplemented type for cast (SOURCE -> TARGET)`.
pub fn unimplemented_cast_message(source: &LogicalType, target: &LogicalType) -> rustdb_common::ByteString {
    rustdb_common::byte_format!("Unimplemented type for cast ({source} -> {target})")
}

/// Binds the cast from `source` to `target`, recursing into `set` for child casts.
pub type BindCastFn =
    Arc<dyn Fn(&CastFunctionSet, &LogicalType, &LogicalType) -> Result<Option<BoundCastInfo>> + Send + Sync>;

/// A cast registered by an extension or client (DuckDB's `MapCastNode`).
#[derive(Clone)]
enum RegisteredCast {
    Bound(BoundCastInfo),
    Bind(BindCastFn),
}

#[derive(Clone)]
struct CastEntry {
    source: LogicalType,
    target: LogicalType,
    cast: RegisteredCast,
    implicit_cost: i64,
}

/// The casts known to a database (DuckDB's `CastFunctionSet`): DuckDB's default casts plus
/// casts registered at runtime, which take precedence.
#[derive(Clone, Default)]
pub struct CastFunctionSet {
    registered: Vec<CastEntry>,
    /// The connection casts are bound for (DuckDB's `GetCastFunctionInput::context`), if
    /// any: extension casts that depend on the connection's settings read them from it.
    client: Option<Arc<crate::FunctionClient>>,
}

impl fmt::Debug for CastFunctionSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CastFunctionSet")
            .field("registered", &self.registered.len())
            .field("client", &self.client.is_some())
            .finish()
    }
}

/// Whether a registered type pattern covers `ty`: exact match, `ANY`, or the same nested
/// type with `ANY` children (DuckDB's `RelaxedTypeMatch`).
fn type_pattern_matches(pattern: &LogicalType, ty: &LogicalType) -> bool {
    if pattern == ty || pattern.id() == LogicalTypeId::Any {
        return true;
    }
    if pattern.id() != ty.id() {
        return false;
    }
    match pattern.id() {
        LogicalTypeId::Decimal | LogicalTypeId::Enum => pattern.info().is_none(),
        LogicalTypeId::List => pattern
            .child_type()
            .is_some_and(|child| child.id() == LogicalTypeId::Any),
        LogicalTypeId::Array => pattern
            .array_child()
            .is_some_and(|(child, _)| child.id() == LogicalTypeId::Any),
        LogicalTypeId::Map => match (pattern.map_key_value(), ty.map_key_value()) {
            (Some((pk, pv)), Some((k, v))) => {
                (pk.id() == LogicalTypeId::Any || pk == k) && (pv.id() == LogicalTypeId::Any || pv == v)
            }
            _ => false,
        },
        LogicalTypeId::Struct | LogicalTypeId::Union => pattern
            .struct_children()
            .is_some_and(|children| children.iter().any(|(_, child)| child.id() == LogicalTypeId::Any)),
        _ => false,
    }
}

impl CastFunctionSet {
    /// DuckDB's default casts only.
    pub fn new() -> Self {
        Self::default()
    }

    /// These casts, bound for the connection `client` (DuckDB's `BindCastInput::context`).
    #[must_use]
    pub fn with_client(&self, client: Arc<crate::FunctionClient>) -> Self {
        Self {
            registered: self.registered.clone(),
            client: Some(client),
        }
    }

    /// The connection the casts are bound for, if any.
    pub fn client(&self) -> Option<&Arc<crate::FunctionClient>> {
        self.client.as_ref()
    }

    /// Registers a bound cast from `source` to `target` with an implicit cast cost
    /// (`-1` to allow it only explicitly).
    pub fn register_cast_function(
        &mut self,
        source: LogicalType,
        target: LogicalType,
        cast: BoundCastInfo,
        implicit_cost: i64,
    ) {
        self.registered.push(CastEntry {
            source,
            target,
            cast: RegisteredCast::Bound(cast),
            implicit_cost,
        });
    }

    /// Registers a bind function producing casts for type patterns (`ANY`, `LIST(ANY)`, ...).
    pub fn register_bind_function(
        &mut self,
        source: LogicalType,
        target: LogicalType,
        bind: BindCastFn,
        implicit_cost: i64,
    ) {
        self.registered.push(CastEntry {
            source,
            target,
            cast: RegisteredCast::Bind(bind),
            implicit_cost,
        });
    }

    fn registered_entry(&self, source: &LogicalType, target: &LogicalType) -> Option<&CastEntry> {
        let exact = self
            .registered
            .iter()
            .rev()
            .find(|entry| &entry.source == source && &entry.target == target);
        exact.or_else(|| {
            self.registered.iter().rev().find(|entry| {
                type_pattern_matches(&entry.source, source) && type_pattern_matches(&entry.target, target)
            })
        })
    }

    /// Whether an extension registered the cast from `source` to `target` (e.g. ICU's
    /// VARCHAR to TIMESTAMPTZ, standing for `DatabaseInstance::ExtensionIsLoaded("icu")`).
    pub fn has_registered_cast(&self, source: &LogicalType, target: &LogicalType) -> bool {
        self.registered
            .iter()
            .any(|entry| &entry.source == source && &entry.target == target)
    }

    /// The cost of implicitly casting `source` to `target`, or `-1` if the binder may not
    /// insert this cast (DuckDB's `CastFunctionSet::ImplicitCastCost`).
    pub fn implicit_cast_cost(&self, source: &LogicalType, target: &LogicalType) -> i64 {
        match self.registered_entry(source, target) {
            Some(entry) => entry.implicit_cost,
            None => implicit_cast_cost(source, target),
        }
    }

    /// Binds the cast from `source` to `target` (DuckDB's `GetCastFunction`). Casts that
    /// do not exist bind to a kernel that fails on the first non-NULL row.
    pub fn get_cast_function(&self, source: &LogicalType, target: &LogicalType) -> Result<BoundCastInfo> {
        if source == target {
            return Ok(BoundCastInfo::nop());
        }
        if let Some(entry) = self.registered_entry(source, target) {
            let bound = match &entry.cast {
                RegisteredCast::Bound(cast) => Some(cast.clone()),
                RegisteredCast::Bind(bind) => bind(self, source, target)?,
            };
            if let Some(bound) = bound {
                return Ok(bound);
            }
        }
        Ok(self
            .default_cast(source, target)?
            .unwrap_or_else(|| BoundCastInfo::unsupported(source, target)))
    }

    /// DuckDB's `DefaultCasts::GetDefaultCastFunction`: dispatch on the source type.
    fn default_cast(&self, source: &LogicalType, target: &LogicalType) -> Result<Option<BoundCastInfo>> {
        use LogicalTypeId as L;
        if target.id() == L::Variant {
            return Ok(Some(variant::to_variant()));
        }
        if target.id() == L::Union && !matches!(source.id(), L::Union | L::SqlNull | L::Variant) {
            return structs::to_union(self, source, target).map(Some);
        }
        if target.id() == L::Bignum && source.id() != L::Bignum {
            return Ok(bignum::to_bignum(source));
        }
        Ok(match source.id() {
            L::Boolean
            | L::TinyInt
            | L::SmallInt
            | L::Integer
            | L::BigInt
            | L::UTinyInt
            | L::USmallInt
            | L::UInteger
            | L::UBigInt
            | L::HugeInt
            | L::UHugeInt
            | L::Float
            | L::Double => numeric::cast_switch(source, target),
            L::Decimal => decimal::cast_switch(source, target),
            L::Varchar => string::cast_switch(self, source, target)?,
            L::Blob | L::Bit | L::Uuid => binary::cast_switch(source, target),
            L::Bignum => bignum::from_bignum(target),
            L::Date
            | L::Time
            | L::TimeNs
            | L::TimeTz
            | L::Timestamp
            | L::TimestampTz
            | L::TimestampNs
            | L::TimestampMs
            | L::TimestampSec
            | L::Interval => temporal::cast_switch(source, target),
            L::Enum => enums::cast_switch(self, source, target)?,
            L::List | L::Array | L::Map => nested::cast_switch(self, source, target)?,
            L::Struct | L::Union => structs::cast_switch(self, source, target)?,
            L::SqlNull => Some(BoundCastInfo::null_source()),
            L::Geometry => geometry::cast_switch(source, target)?,
            L::Variant => variant::cast_switch(target),
            L::AggregateState => Some(aggregate_state_to_blob(source, target)),
            _ => None,
        })
    }
}

/// DuckDB's `AggregateStateToBlobCast`: an exported aggregate state casts only to BLOB,
/// which reuses its bytes; other targets fail when the cast runs.
fn aggregate_state_to_blob(source: &LogicalType, target: &LogicalType) -> BoundCastInfo {
    if target.id() == LogicalTypeId::Blob {
        return BoundCastInfo::reinterpret();
    }
    let message = format!(
        "Type {source} does not match with {target}. Cannot cast AGGREGATE_STATE to anything but BLOB"
    );
    BoundCastInfo::new(move |_, _, _, _| {
        Err(Error::new(
            rustdb_common::ErrorKind::MismatchType,
            message.clone(),
        ))
    })
}

#[cfg(test)]
mod error_tests {
    use super::CastParameters;
    use rustdb_common::ErrorKind;

    #[test]
    fn byte_cast_error_keeps_source_column_context() {
        let mut params = CastParameters::new();
        params.source_column = Some("café".into());
        let error = params.assign_error_bytes(b"bad \xff".to_vec()).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Conversion);
        assert_eq!(
            error.raw_message_bytes(),
            b"bad \xff when casting from source column caf\xc3\xa9"
        );
    }

    #[test]
    fn try_cast_fills_empty_message_and_retains_first_nonempty_error() {
        let mut params = CastParameters::try_cast();
        params.assign_error_bytes(Vec::new()).unwrap();
        params.assign_error_bytes(b"bad \xff".to_vec()).unwrap();
        params.assign_error("later".to_owned()).unwrap();
        assert_eq!(params.take_error_message().unwrap(), b"bad \xff");
        assert!(params.error_message().is_none());
    }
}
