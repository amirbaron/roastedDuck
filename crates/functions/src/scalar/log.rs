//! `write_log` and `parse_duckdb_log_message` (DuckDB's `function/scalar/system/write_log.cpp`
//! and `parse_log_message.cpp`).

use std::any::Any;
use std::sync::Arc;

use rustdb_common::{ByteString, Error, ErrorKind, Result};
use rustdb_function::cast::{CastFunctionSet, CastParameters, cast_vector};
use rustdb_function::logging::{ClientLogger, LogLevel, LogScope};
use rustdb_function::{BindInput, FunctionData, NullHandling, ScalarFunction, ScalarFunctionSet, Stability};
use rustdb_types::{LogicalType, LogicalTypeId, Value, Vector};

/// `WriteLogBindData`.
#[derive(Debug)]
struct WriteLogData {
    logger: Option<Arc<dyn ClientLogger>>,
    disable_logging: bool,
    scope: String,
    level: LogLevel,
    log_type: String,
    output_column: Option<usize>,
}

impl FunctionData for WriteLogData {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// `ThrowIfNotConstant` and the constant value of argument `index`.
fn constant_argument<'a>(input: &'a BindInput<'_>, index: usize) -> Result<&'a Value> {
    input
        .constants
        .get(index)
        .and_then(Option::as_ref)
        .ok_or_else(|| {
            Error::binder(rustdb_common::byte_format!(
                "write_log: argument '{}' must be constant",
                input
                    .aliases
                    .get(index)
                    .map_or(b"".as_slice(), ByteString::as_bytes)
            ))
        })
}

/// `WriteLogBind`: the options are named arguments.
fn bind_write_log(
    function: &mut ScalarFunction,
    input: &BindInput<'_>,
) -> Result<Option<Arc<dyn FunctionData>>> {
    let Some(message_type) = input.argument_types.first() else {
        return Err(Error::binder("write_log takes at least one argument"));
    };
    if message_type.id() != LogicalTypeId::Varchar {
        return Err(Error::new(
            ErrorKind::InvalidType,
            "write_log first argument must be a VARCHAR",
        ));
    }
    let mut data = WriteLogData {
        logger: input.client.logger(),
        disable_logging: false,
        scope: String::new(),
        level: LogLevel::Info,
        log_type: String::new(),
        output_column: None,
    };
    function.return_type = LogicalType::VARCHAR;
    function.arguments = input.argument_types.to_vec();
    for index in 1..input.argument_types.len() {
        let alias = input
            .aliases
            .get(index)
            .map_or(b"".as_slice(), ByteString::as_bytes);
        let ty = &input.argument_types[index];
        match alias {
            b"disable_logging" => {
                let value = constant_argument(input, index)?;
                if ty.id() != LogicalTypeId::Boolean {
                    return Err(Error::binder(
                        "write_log: 'disable_logging' argument must be a boolean",
                    ));
                }
                data.disable_logging = value.as_bool().unwrap_or(false);
            }
            b"scope" | b"level" | b"log_type" => {
                let value = constant_argument(input, index)?;
                if ty.id() != LogicalTypeId::Varchar {
                    return Err(Error::binder(rustdb_common::byte_format!(
                        "write_log: '{alias}' argument must be a string"
                    )));
                }
                let text = value.to_varchar()?;
                match alias {
                    b"scope" => data.scope = text,
                    b"level" => data.level = LogLevel::parse(&text)?,
                    _ => data.log_type = text,
                }
            }
            b"return_value" => {
                function.return_type = ty.clone();
                data.output_column = Some(index);
            }
            _ => {
                return Err(Error::binder(rustdb_common::byte_format!(
                    "write_log: Unknown argument '{alias}'"
                )));
            }
        }
    }
    Ok(Some(Arc::new(data)))
}

/// `write_log(message, [level := ...], [scope := ...], [log_type := ...],
/// [return_value := ...], [disable_logging := ...])`.
fn write_log_functions() -> ScalarFunctionSet {
    let mut function = ScalarFunction::new(
        "write_log",
        vec![LogicalType::VARCHAR],
        LogicalType::ANY,
        |args, state, result| {
            let data = state
                .bind_data
                .as_ref()
                .and_then(|data| data.as_any().downcast_ref::<WriteLogData>())
                .ok_or_else(|| Error::internal("write_log without bind data"))?;
            if !data.disable_logging
                && let Some(logger) = &data.logger
            {
                let scope = match data.scope.as_str() {
                    "" | "connection" | "file_opener" => LogScope::Connection,
                    "database" => LogScope::Database,
                    other => {
                        return Err(Error::invalid_input(format!(
                            "write_log: 'scope' argument unknown: '{other}'. Valid values are [connection, database, \
                             file_opener]"
                        )));
                    }
                };
                let messages = args.column(0);
                for row in 0..args.len() {
                    let message = messages.get_value(row);
                    if message.is_null() || !logger.should_log(scope, &data.log_type, data.level) {
                        continue;
                    }
                    logger.write_log_bytes(scope, &data.log_type, data.level, &message.to_varchar_bytes()?)?;
                }
            }
            *result = match data.output_column {
                Some(column) => args.column(column).clone(),
                None => Vector::constant_null(LogicalType::VARCHAR),
            };
            Ok(())
        },
    )
    .with_bind(bind_write_log);
    function.varargs = Some(LogicalType::ANY);
    function.stability = Stability::Volatile;
    function.null_handling = NullHandling::Special;
    let mut set = ScalarFunctionSet::new("write_log");
    set.add(function);
    set
}

/// The target of `parse_duckdb_log_message` (`ParseLogMessageData`): `None` wraps
/// unstructured messages in `STRUCT(message VARCHAR)`.
#[derive(Debug)]
struct ParseLogData {
    structured: Option<LogicalType>,
}

impl FunctionData for ParseLogData {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// `ParseLogMessageBind`.
fn bind_parse_log_message(
    function: &mut ScalarFunction,
    input: &BindInput<'_>,
) -> Result<Option<Arc<dyn FunctionData>>> {
    if input.argument_types.len() != 2 {
        return Err(Error::binder("structured_log_schema: expects 1 argument"));
    }
    let Some(log_type) = input.constants[0].as_ref() else {
        return Err(Error::binder(rustdb_common::byte_format!(
            "structured_log_schema: argument '{}' must be constant",
            input.aliases.first().map_or(b"".as_slice(), ByteString::as_bytes)
        )));
    };
    if input.argument_types[0].id() != LogicalTypeId::Varchar {
        return Err(Error::binder(
            "structured_log_schema: 'log_type' argument must be a string",
        ));
    }
    let name = log_type.to_varchar()?;
    let found = input
        .client
        .logger()
        .and_then(|logger| logger.lookup_log_type(&name))
        .ok_or_else(|| Error::invalid_input(format!("structured_log_schema: '{name}' not found")))?;
    function.return_type = match &found.structured_type {
        Some(ty) => ty.clone(),
        None => LogicalType::struct_type(vec![("message".to_owned().into(), LogicalType::VARCHAR)]),
    };
    Ok(Some(Arc::new(ParseLogData {
        structured: found.structured_type,
    })))
}

/// `parse_duckdb_log_message(log_type, message)`: the message parsed into the log
/// type's structure.
fn parse_log_message_functions() -> ScalarFunctionSet {
    let function = ScalarFunction::new(
        "parse_duckdb_log_message",
        vec![LogicalType::VARCHAR, LogicalType::VARCHAR],
        LogicalType::ANY,
        |args, state, result| {
            let data = state
                .bind_data
                .as_ref()
                .and_then(|data| data.as_any().downcast_ref::<ParseLogData>())
                .ok_or_else(|| Error::internal("parse_duckdb_log_message without bind data"))?;
            let messages = args.column(1);
            *result = if let Some(ty) = &data.structured {
                let mut params = CastParameters::new();
                params.strict = true;
                cast_vector(&CastFunctionSet::new(), messages, ty, args.len(), &mut params)?
            } else {
                let ty = result.logical_type().clone();
                let mut out = Vector::flat(ty, args.len().max(1));
                for row in 0..args.len() {
                    let message = messages.get_value(row);
                    let value = Value::struct_value(vec![("message".to_owned().into(), message)]);
                    out.set_value(row, &value)?;
                }
                out
            };
            Ok(())
        },
    )
    .with_bind(bind_parse_log_message);
    let mut set = ScalarFunctionSet::new("parse_duckdb_log_message");
    set.add(function);
    set
}

/// The logging scalar functions.
pub fn log_functions() -> Vec<ScalarFunctionSet> {
    vec![write_log_functions(), parse_log_message_functions()]
}
