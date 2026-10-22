//! Functions in serialized plans (DuckDB's `FunctionSerializer::Serialize`): the
//! function's name, arguments and catalog entry, and its bind data when it can write it.

use rustdb_common::Result;
use rustdb_function::{AggregateFunction, FunctionData, ScalarFunction, TableFunction};
use rustdb_serde::Serializer;
use rustdb_types::LogicalType;

/// Writes the bind data of a function (`function_data`).
pub(crate) type FunctionDataWriter<'a> = &'a dyn Fn(&mut dyn Serializer) -> Result<()>;

/// The properties of a function call that identify the function.
pub(crate) struct FunctionProperties<'a> {
    pub name: &'a str,
    pub arguments: &'a [LogicalType],
    pub original_arguments: &'a [LogicalType],
    pub catalog_name: &'a str,
    pub schema_name: &'a str,
}

impl FunctionProperties<'_> {
    /// Writes the function, then its bind data with `function_data` (DuckDB's
    /// `HasSerializationCallbacks`: without it, the function is bound again from the
    /// arguments).
    pub(crate) fn serialize(
        &self,
        serializer: &mut dyn Serializer,
        function_data: Option<FunctionDataWriter<'_>>,
    ) -> Result<()> {
        serializer.write_property(500, "name", self.name)?;
        serializer.write_property(501, "arguments", self.arguments)?;
        serializer.write_property(502, "original_arguments", self.original_arguments)?;
        // Written out of numeric order: older databases do not have them.
        serializer.write_property_with_explicit_default(505, "catalog_name", self.catalog_name, "")?;
        serializer.write_property_with_explicit_default(506, "schema_name", self.schema_name, "")?;
        serializer.write_property(503, "has_serialize", &function_data.is_some())?;
        if let Some(function_data) = function_data {
            serializer.write_object_property(504, "function_data", function_data)?;
        }
        Ok(())
    }
}

/// `FunctionSerializer::Serialize` of a scalar function.
pub(crate) fn serialize_scalar_function(
    serializer: &mut dyn Serializer,
    function: &ScalarFunction,
    bind_info: Option<&dyn FunctionData>,
) -> Result<()> {
    let write = function
        .serialize
        .as_ref()
        .map(|serialize| move |serializer: &mut dyn Serializer| serialize(serializer, bind_info, function));
    FunctionProperties {
        name: &function.name,
        arguments: &function.arguments,
        original_arguments: &function.original_arguments,
        catalog_name: &function.catalog_name,
        schema_name: &function.schema_name,
    }
    .serialize(serializer, write.as_ref().map(|write| write as _))
}

/// `FunctionSerializer::Serialize` of an aggregate function.
pub(crate) fn serialize_aggregate_function(
    serializer: &mut dyn Serializer,
    function: &AggregateFunction,
    bind_info: Option<&dyn FunctionData>,
) -> Result<()> {
    let write = function
        .serialize
        .as_ref()
        .map(|serialize| move |serializer: &mut dyn Serializer| serialize(serializer, bind_info, function));
    FunctionProperties {
        name: &function.name,
        arguments: &function.arguments,
        original_arguments: &function.original_arguments,
        catalog_name: &function.catalog_name,
        schema_name: &function.schema_name,
    }
    .serialize(serializer, write.as_ref().map(|write| write as _))
}

/// `FunctionSerializer::Serialize` of a table function; `function_data` overrides the
/// function's own bind data writer (the table scan's, whose table rustdb keeps in the
/// `LogicalGet`).
pub(crate) fn serialize_table_function(
    serializer: &mut dyn Serializer,
    function: &TableFunction,
    bind_data: Option<&dyn FunctionData>,
    function_data: Option<FunctionDataWriter<'_>>,
) -> Result<()> {
    let write = function
        .serialize
        .as_ref()
        .map(|serialize| move |serializer: &mut dyn Serializer| serialize(serializer, bind_data, function));
    let properties = FunctionProperties {
        name: &function.name,
        arguments: &function.arguments,
        original_arguments: &[],
        catalog_name: &function.catalog_name,
        schema_name: &function.schema_name,
    };
    match function_data {
        Some(function_data) => properties.serialize(serializer, Some(function_data)),
        None => properties.serialize(serializer, write.as_ref().map(|write| write as _)),
    }
}
