//! `printf` and `format` (DuckDB's `printf.cpp`), on a port of the bundled `{fmt}` 6
//! fork: `printf` interprets C-style `%` conversions and `format` Python-style `{}`
//! fields, both with DuckDB's thousands-separator extensions.

mod grisu;
mod parse;
mod writer;

use std::sync::Arc;

use roastedduck_common::{Error, Result};
use roastedduck_function::{BindInput, FunctionData, ScalarFunction, ScalarFunctionSet};
use roastedduck_types::{DataChunk, LogicalType, LogicalTypeId, Vector};

use self::parse::Arg;
use crate::scalar::executor::{ScalarOutput, StrOut, addressable, str_at};

/// DuckDB's `BindPrintfFunction`: each argument is cast to one of the types `{fmt}`
/// formats natively.
#[allow(clippy::unnecessary_wraps, reason = "the signature of a bind callback")]
fn bind_printf(
    function: &mut ScalarFunction,
    input: &BindInput<'_>,
) -> Result<Option<Arc<dyn FunctionData>>> {
    function.arguments.truncate(1);
    for ty in &input.argument_types[1..] {
        use LogicalTypeId as L;
        function.arguments.push(match ty.id() {
            L::Boolean => LogicalType::BOOLEAN,
            L::TinyInt | L::SmallInt | L::Integer | L::BigInt => LogicalType::BIGINT,
            L::UTinyInt | L::USmallInt | L::UInteger | L::UBigInt => LogicalType::UBIGINT,
            L::HugeInt => LogicalType::HUGEINT,
            L::UHugeInt => LogicalType::UHUGEINT,
            L::Float | L::Double | L::Decimal => LogicalType::DOUBLE,
            L::Unknown => LogicalType::ANY,
            _ => LogicalType::VARCHAR,
        });
    }
    Ok(None)
}

/// The `{fmt}` argument in row `row` of a (non-NULL) column.
fn argument(column: &Vector, row: usize) -> Result<Arg<'_>> {
    let value = |missing: &str| Error::internal(format!("printf argument without a {missing} value"));
    Ok(match column.logical_type().id() {
        LogicalTypeId::Boolean => Arg::Bool(
            column
                .unified::<bool>()
                .get(row)
                .ok_or_else(|| value("BOOLEAN"))?,
        ),
        LogicalTypeId::BigInt => {
            Arg::LongLong(column.unified::<i64>().get(row).ok_or_else(|| value("BIGINT"))?)
        }
        LogicalTypeId::UBigInt => {
            Arg::ULongLong(column.unified::<u64>().get(row).ok_or_else(|| value("UBIGINT"))?)
        }
        LogicalTypeId::HugeInt => Arg::Int128(
            column
                .unified::<i128>()
                .get(row)
                .ok_or_else(|| value("HUGEINT"))?,
        ),
        LogicalTypeId::UHugeInt => Arg::UInt128(
            column
                .unified::<u128>()
                .get(row)
                .ok_or_else(|| value("UHUGEINT"))?,
        ),
        LogicalTypeId::Double => {
            Arg::Double(column.unified::<f64>().get(row).ok_or_else(|| value("DOUBLE"))?)
        }
        LogicalTypeId::Varchar => Arg::Str(str_at(column, row).ok_or_else(|| value("VARCHAR"))?),
        _ => return Err(Error::internal("Unexpected type for printf format")),
    })
}

/// DuckDB's `PrintfFunction`: a constant NULL argument makes the result NULL; otherwise
/// rows with a NULL argument are NULL.
fn printf_rows(
    args: &DataChunk,
    result: &mut Vector,
    run: fn(&[u8], &[Arg<'_>]) -> Result<Vec<u8>>,
) -> Result<()> {
    let count = args.len();
    let columns: Vec<_> = args
        .columns()
        .iter()
        .map(|column| addressable(column, count))
        .collect();
    if columns
        .iter()
        .any(|column| column.is_constant() && column.is_null(0))
    {
        *result = Vector::constant_null(LogicalType::VARCHAR);
        return Ok(());
    }
    let constant = columns.iter().all(|column| column.is_constant());
    let rows = if constant { 1 } else { count };
    let mut arguments = Vec::with_capacity(columns.len().saturating_sub(1));
    StrOut::write(result, rows, |row, out| {
        if columns.iter().any(|column| column.is_null(row)) {
            return Ok(false);
        }
        let format =
            str_at(&columns[0], row).ok_or_else(|| Error::internal("printf without a format string"))?;
        arguments.clear();
        for column in &columns[1..] {
            arguments.push(argument(column, row)?);
        }
        let formatted = run(format, &arguments)?;
        if std::str::from_utf8(&formatted).is_err() {
            return Err(Error::invalid_input(format!(
                "Invalid UTF8 produced by format string \"{}\" - note that %c writes a single byte, use chr(...) to \
                 write a Unicode code point",
                String::from_utf8_lossy(format)
            )));
        }
        out.extend_from_slice(&formatted);
        Ok(true)
    })?;
    if constant {
        result.make_constant();
    }
    Ok(())
}

fn printf_set(name: &'static str, run: fn(&[u8], &[Arg<'_>]) -> Result<Vec<u8>>) -> ScalarFunctionSet {
    let mut function = ScalarFunction::new(
        name,
        vec![LogicalType::VARCHAR],
        LogicalType::VARCHAR,
        move |args, _, result| printf_rows(args, result, run),
    )
    .with_bind(bind_printf);
    function.varargs = Some(LogicalType::ANY);
    let mut set = ScalarFunctionSet::new(name);
    set.add(function);
    set
}

/// `printf(format, ...)`.
pub fn printf_functions() -> ScalarFunctionSet {
    printf_set("printf", parse::sprintf)
}

/// `format(format, ...)`.
pub fn format_functions() -> ScalarFunctionSet {
    printf_set("format", parse::format)
}
