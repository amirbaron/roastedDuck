//! Casting single values through the vectorised kernels (DuckDB's `Value::TryCastAs`,
//! `Value::DefaultCastAs` and `VectorOperations::TryCast`).

use rustdb_common::{Error, Result};
use rustdb_types::{LogicalType, Value, Vector};

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
        Ok(result.get_value(0))
    }

    fn try_cast_as(&self, set: &CastFunctionSet, target: &LogicalType, strict: bool) -> Result<Self> {
        let mut params = CastParameters::try_cast();
        params.strict = strict;
        let value = self.cast_with(set, target, &mut params)?;
        match params.take_error_message() {
            Some(message) => Err(Error::new_bytes(rustdb_common::ErrorKind::Conversion, message)),
            None => Ok(value),
        }
    }

    fn default_cast_as(&self, target: &LogicalType) -> Result<Self> {
        if target.id() == rustdb_types::LogicalTypeId::Any {
            return Ok(self.clone());
        }
        self.try_cast(target).map_err(|error| {
            if error.kind() == rustdb_common::ErrorKind::Conversion {
                Error::invalid_input(format!("Failed to cast value: {}", error.raw_message()))
            } else {
                error
            }
        })
    }
}
