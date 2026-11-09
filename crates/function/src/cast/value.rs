//! Casting single values through the vectorised kernels (DuckDB's `Value::TryCastAs`,
//! `Value::DefaultCastAs` and `VectorOperations::TryCast`).

use roastedduck_common::{Error, Result};
use roastedduck_types::{LogicalType, Value, Vector};

use super::{CastFunctionSet, CastParameters};

/// Casts the first `count` rows of `source` to `target` using `set`
/// (DuckDB's `VectorOperations::TryCast`). Under `TRY_CAST` parameters failing rows are
/// NULL and the first message is recorded in `params`.
pub fn cast_vector(
    set: &CastFunctionSet,
    source: &Vector,
    target: &LogicalType,
    count: usize,
    params: &mut CastParameters,
) -> Result<Vector> {
    let cast = set.get_cast_function(source.logical_type(), target)?;
    let mut result = Vector::flat(target.clone(), count);
    cast.execute(source, &mut result, count, params)?;
    Ok(result)
}

/// Casting of [`Value`]s, implemented on top of the vector casts so that both paths
/// produce identical results and errors.
pub trait ValueCast: Sized {
    /// Casts with explicit `CAST`/`TRY_CAST` parameters; under `TRY_CAST` a failed
    /// conversion yields NULL and records the message in `params`.
    fn cast_with(
        &self,
        set: &CastFunctionSet,
        target: &LogicalType,
        params: &mut CastParameters,
    ) -> Result<Self>;

    /// DuckDB's `Value::TryCastAs`: the converted value, or the conversion error.
    fn try_cast_as(&self, set: &CastFunctionSet, target: &LogicalType, strict: bool) -> Result<Self>;

    /// [`ValueCast::try_cast_as`] with DuckDB's default casts (`Value::DefaultTryCastAs`).
    fn try_cast(&self, target: &LogicalType) -> Result<Self> {
        self.try_cast_as(&CastFunctionSet::new(), target, false)
    }

    /// DuckDB's `Value::DefaultCastAs`: failures become
    /// `Invalid Input Error: Failed to cast value: <message>`.
    fn default_cast_as(&self, target: &LogicalType) -> Result<Self>;
}

impl ValueCast for Value {
    fn cast_with(
        &self,
        set: &CastFunctionSet,
        target: &LogicalType,
        params: &mut CastParameters,
    ) -> Result<Self> {
        if self.logical_type() == target {
            return Ok(self.clone());
        }
        let input = Vector::constant(self)?;
        let result = cast_vector(set, &input, target, 1, params)?;
        let value = result.get_value(0);
        // Vector::GetValue materializes VARCHAR through Value(string), unlike
        // native vector/string storage, which can hold unchecked bytes.
        if value.logical_type().id() == roastedduck_types::LogicalTypeId::Varchar
            && let Some(bytes) = value.as_bytes()
        {
            std::str::from_utf8(bytes)
                .map_err(|_| roastedduck_common::utf8::invalid_unicode_error(bytes, "value construction"))?;
        }
        Ok(value)
    }

    fn try_cast_as(&self, set: &CastFunctionSet, target: &LogicalType, strict: bool) -> Result<Self> {
        let mut params = CastParameters::try_cast();
        params.strict = strict;
        let value = self.cast_with(set, target, &mut params)?;
        match params.take_error_message() {
            Some(message) => Err(Error::new_bytes(
                roastedduck_common::ErrorKind::Conversion,
                message,
            )),
            None => Ok(value),
        }
    }

    fn default_cast_as(&self, target: &LogicalType) -> Result<Self> {
        if target.id() == roastedduck_types::LogicalTypeId::Any {
            return Ok(self.clone());
        }
        let mut params = CastParameters::try_cast();
        let value = self.cast_with(&CastFunctionSet::new(), target, &mut params)?;
        // C++ wraps a false TryCast result, but exceptions from binding/execution
        // escape unchanged (e.g. an unnamed STRUCT cast to VARIANT).
        match params.take_error_message() {
            Some(message) => Err(Error::new_bytes(
                roastedduck_common::ErrorKind::InvalidInput,
                [b"Failed to cast value: ".as_slice(), message.as_slice()].concat(),
            )),
            None => Ok(value),
        }
    }
}
