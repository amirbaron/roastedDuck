//! `json_serialize_plan` (DuckDB's `json_serialize_plan.cpp`): the logical plans of SQL
//! statements as JSON, optionally optimized.

use std::any::Any;
use std::sync::Arc;

use rustdb_common::{Error, ErrorKind, Result};
use rustdb_function::{
    BindInput, FunctionClient, FunctionData, FunctionState, ScalarFunction, ScalarFunctionSet,
};
use rustdb_optimizer::{Optimizer, OptimizerConfig};
use rustdb_planner::{ClientContext, ColumnBindingResolver, Planner};
use rustdb_serde::SerializationOptions;
use rustdb_serde::json::{JsonOptions, JsonSerializer, JsonValue, write_json};
use rustdb_types::{DataChunk, LogicalType, LogicalTypeId, Vector};

use crate::json_executors::{ColumnBuilder, StrBuilder, StrInput, addressable, all_constant};
use crate::json_type;

/// The options of a `json_serialize_plan` call (`JsonSerializePlanBindData`), and the
/// connection that plans the statements.
#[derive(Debug)]
struct SerializePlanBindData {
    json: JsonOptions,
    format: bool,
    optimize: bool,
    client: Arc<FunctionClient>,
}

impl FunctionData for SerializePlanBindData {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// `JsonSerializePlanBind`.
fn serialize_plan_bind(
    _: &mut ScalarFunction,
    input: &BindInput<'_>,
) -> Result<Option<Arc<dyn FunctionData>>> {
    let Some(first) = input.argument_types.first() else {
        return Err(Error::binder("json_serialize_plan takes at least one argument"));
    };
    if first.id() != LogicalTypeId::Varchar && first.id() != LogicalTypeId::StringLiteral {
        return Err(Error::new(
            ErrorKind::InvalidType,
            "json_serialize_plan first argument must be a VARCHAR",
        ));
    }
    let mut json = JsonOptions::default();
    let mut format = false;
    let mut optimize = false;
    for i in 1..input.argument_types.len() {
        if input.argument_types[i].id() == LogicalTypeId::Unknown {
            return Err(rustdb_planner::expression_binder::parameter_not_resolved());
        }
        let Some(Some(value)) = input.constants.get(i) else {
            return Err(Error::binder("json_serialize_plan: arguments must be constant"));
        };
        let alias = input.aliases.get(i).map_or("", String::as_str);
        let flag = |name: &str| -> Result<bool> {
            if input.argument_types[i].id() != LogicalTypeId::Boolean {
                return Err(Error::binder(format!(
                    "json_serialize_plan: '{name}' argument must be a boolean"
                )));
            }
            Ok(value.as_bool().unwrap_or(false))
        };
        match alias {
            "skip_null" => json.skip_null = flag(alias)?,
            "skip_empty" => json.skip_empty = flag(alias)?,
            "skip_default" => json.skip_default = flag(alias)?,
            "format" => format = flag(alias)?,
            "optimize" => optimize = flag(alias)?,
            _ => {
                return Err(Error::binder(format!(
                    "json_serialize_plan: Unknown argument '{alias}'"
                )));
            }
        }
    }
    Ok(Some(Arc::new(SerializePlanBindData {
        json,
        format,
        optimize,
        client: Arc::clone(input.client),
    })))
}

/// The plan of each statement of `sql` (the body of `JsonSerializePlanFunction`).
fn serialize_plans(sql: &str, data: &SerializePlanBindData) -> Result<Vec<JsonValue>> {
    let statements = rustdb_parser::parse(sql)?;
    let context = data
        .client
        .transaction_context()
        .and_then(|context| context.downcast::<ClientContext>().ok())
        .ok_or_else(|| Error::invalid_input("json_serialize_plan: No client context available"))?;
    let mut plans = Vec::with_capacity(statements.len());
    for statement in statements {
        let planned = Planner::new(Arc::clone(&context)).create_plan(statement)?;
        let mut plan = planned
            .plan
            .ok_or_else(|| Error::invalid_input("json_serialize_plan: not all parameters were bound"))?;
        if data.optimize {
            // `Optimizer::Optimize`, which runs whether or not `enable_optimizer` is set.
            let mut config = data
                .client
                .settings()
                .map_or_else(OptimizerConfig::default, |settings| {
                    OptimizerConfig::from_settings(settings.as_ref())
                });
            config.enabled = true;
            let mut optimizer = Optimizer::new(Arc::clone(&context), config);
            optimizer.reserve_table_indexes(planned.table_index_count);
            plan = optimizer.optimize(plan)?;
        }
        ColumnBindingResolver::resolve(&mut plan)?;
        plan.resolve_operator_types()?;
        if let Some(operator) = plan.unserializable_operator() {
            return Err(Error::invalid_input(format!(
                "Operator '{}' does not support serialization",
                operator.name()
            )));
        }
        plans.push(JsonSerializer::serialize(
            &plan,
            data.json,
            SerializationOptions::default(),
        )?);
    }
    Ok(plans)
}

/// `{"error":false,"plans":[...]}`, or the error as `{"error":true,...}`.
fn serialize_plan_document(sql: &str, data: &SerializePlanBindData) -> String {
    let document = match serialize_plans(sql, data) {
        Ok(plans) => JsonValue::Object(vec![
            ("error".to_owned(), JsonValue::Bool(false)),
            ("plans".to_owned(), JsonValue::Array(plans)),
        ]),
        Err(error) => {
            let mut entries = vec![
                ("error".to_owned(), JsonValue::Bool(true)),
                (
                    "error_type".to_owned(),
                    JsonValue::Str(error.kind().display_name().to_ascii_lowercase()),
                ),
                (
                    "error_message".to_owned(),
                    JsonValue::Str(error.raw_message().into_owned()),
                ),
            ];
            entries.extend(
                error
                    .extra_info()
                    .into_iter()
                    .map(|(key, value)| (key.to_owned(), JsonValue::Str(value.to_owned()))),
            );
            JsonValue::Object(entries)
        }
    };
    write_json(&document, data.format)
}

/// `JsonSerializePlanFunction`.
fn serialize_plan_rows(args: &DataChunk, state: &mut FunctionState, result: &mut Vector) -> Result<()> {
    let data = state
        .bind_data
        .as_deref()
        .and_then(|data| data.as_any().downcast_ref::<SerializePlanBindData>())
        .ok_or_else(|| Error::invalid_input("json_serialize_plan: No client context available"))?;
    let constant = all_constant(args);
    let rows = if constant { 1 } else { args.len() };
    let column = addressable(args.column(0), rows);
    let input = StrInput::new(&column);
    let mut builder = StrBuilder::default();
    for row in 0..rows {
        match input.get(row) {
            Some(sql) => {
                let sql = String::from_utf8_lossy(sql);
                builder.push(serialize_plan_document(&sql, data).as_bytes());
            }
            None => builder.push_null(),
        }
    }
    *result = builder.finish(&json_type())?;
    if constant {
        result.make_constant();
    }
    Ok(())
}

/// `JSONFunctions::GetSerializePlanFunction`: the SQL and up to four BOOLEAN options.
pub(crate) fn serialize_plan_function() -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new("json_serialize_plan");
    for options in 0..=4 {
        let mut arguments = vec![LogicalType::VARCHAR];
        arguments.extend(std::iter::repeat_n(LogicalType::BOOLEAN, options));
        set.add(
            ScalarFunction::new("json_serialize_plan", arguments, json_type(), serialize_plan_rows)
                .with_bind(serialize_plan_bind),
        );
    }
    set
}
