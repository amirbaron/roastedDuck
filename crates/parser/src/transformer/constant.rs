//! Constants (`transform_constant.cpp`).

use rustdb_ast::ParsedExpression;
use rustdb_common::{Error, Result};
use rustdb_function::cast::ValueCast;
use rustdb_pg_parser::nodes::{AConst, Value};
use rustdb_types::{LogicalType, Value as DuckValue};

use super::Transformer;
use super::build::{self, set_location};

/// `Decimal::MAX_WIDTH_DECIMAL`.
const MAX_WIDTH_DECIMAL: usize = 38;

impl Transformer<'_> {
    /// `TransformConstant`.
    pub(crate) fn transform_constant(&mut self, constant: &AConst) -> Result<ParsedExpression> {
        let mut result = Self::transform_value(&constant.val)?;
        set_location(&mut result, constant.location);
        Ok(result)
    }

    /// `TransformValue`: the constant a literal stands for.
    pub(crate) fn transform_value(value: &Value) -> Result<ParsedExpression> {
        Ok(build::constant(match value {
            Value::Integer(integer) => DuckValue::integer(*integer as i32),
            Value::String(text) | Value::BitString(text) => DuckValue::varchar(text),
            Value::Float(text) => numeric_literal(text)?,
            Value::Null => DuckValue::null(LogicalType::SQLNULL),
        }))
    }
}

/// The value of a numeric literal that is not a 32-bit integer: BIGINT, HUGEINT or
/// UHUGEINT if integral, DECIMAL if it has a decimal point and at most 38 digits, and
/// DOUBLE otherwise.
fn numeric_literal(text: &str) -> Result<DuckValue> {
    let bytes = text.as_bytes();
    let mut try_integer = true;
    let mut try_decimal = true;
    let mut decimal_position = None;
    let mut underscores = 0;
    let mut integer_underscores = 0;
    for (i, &byte) in bytes.iter().enumerate() {
        match byte {
            b'.' => {
                try_integer = false;
                decimal_position = Some(i);
            }
            b'e' | b'E' => {
                try_integer = false;
                try_decimal = false;
            }
            b'_' => {
                underscores += 1;
                if decimal_position.is_none() {
                    integer_underscores += 1;
                }
            }
            _ => {}
        }
    }
    let varchar = DuckValue::varchar(text);
    if try_integer {
        for ty in [LogicalType::BIGINT, LogicalType::HUGEINT, LogicalType::UHUGEINT] {
            if let Ok(value) = varchar.try_cast(&ty) {
                return Ok(value);
            }
        }
    }
    let negative = bytes.first() == Some(&b'-');
    let decimal_offset = if negative { 3 } else { 2 };
    if try_decimal
        && let Some(position) = decimal_position
        && bytes.len() - underscores < MAX_WIDTH_DECIMAL + decimal_offset
    {
        let mut width = bytes.len() - 1 - underscores;
        let scale = width + integer_underscores - position;
        if negative {
            width -= 1;
        }
        if width <= MAX_WIDTH_DECIMAL {
            return varchar.default_cast_as(&LogicalType::decimal(width as u8, scale as u8));
        }
    }
    varchar
        .try_cast(&LogicalType::DOUBLE)
        .map_err(|_| Error::conversion(format!("Could not convert string \"{text}\" to DOUBLE")))
}
