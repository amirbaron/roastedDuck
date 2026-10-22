//! Registration of the JSON functions and the bind data shared by the read functions
//! (DuckDB's `json_functions.hpp` / `json_functions.cpp`).

pub(crate) mod copy_json;
pub(crate) mod json_array_length;
pub(crate) mod json_contains;
pub(crate) mod json_create;
pub(crate) mod json_exists;
pub(crate) mod json_extract;
pub(crate) mod json_keys;
pub(crate) mod json_merge_patch;
pub(crate) mod json_pretty;
pub mod json_serialize_plan;
pub mod json_serialize_sql;
pub(crate) mod json_structure;
pub(crate) mod json_table_in_out;
pub(crate) mod json_transform;
pub(crate) mod json_type;
pub(crate) mod json_valid;
pub(crate) mod json_value;
pub(crate) mod read_json;

use std::any::Any;
use std::sync::Arc;

use rustdb_common::{Error, Result};
use rustdb_function::{
    BindInput, CopyFunction, FunctionData, ScalarFunction, ScalarFunctionSet, TableFunctionSet,
};
use rustdb_types::{LogicalType, LogicalTypeId, Value};

use crate::json_common::{JsonPathType, validate_path};
use crate::json_type;

/// The bind data of a read function with one path (`JSONReadFunctionData`).
#[derive(Clone, Debug)]
pub(crate) struct JsonReadFunctionData {
    /// Whether the path is a non-NULL constant.
    pub(crate) constant: bool,
    /// The path in canonical form (`$...` or `/...`).
    pub(crate) path: Vec<u8>,
    pub(crate) path_type: JsonPathType,
}

impl Default for JsonReadFunctionData {
    fn default() -> Self {
        Self {
            constant: false,
            path: Vec::new(),
            path_type: JsonPathType::Regular,
        }
    }
}

impl FunctionData for JsonReadFunctionData {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn equals(&self, other: &dyn FunctionData) -> bool {
        other.as_any().downcast_ref::<Self>().is_some_and(|other| {
            self.constant == other.constant && self.path == other.path && self.path_type == other.path_type
        })
    }
}

/// The bind data of a read function with a list of paths (`JSONReadManyFunctionData`).
#[derive(Clone, Debug, Default)]
pub(crate) struct JsonReadManyFunctionData {
    pub(crate) paths: Vec<Vec<u8>>,
}

impl FunctionData for JsonReadManyFunctionData {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn equals(&self, other: &dyn FunctionData) -> bool {
        other
            .as_any()
            .downcast_ref::<Self>()
            .is_some_and(|other| self.paths == other.paths)
    }
}

/// `JSONReadFunctionData::CheckPath`: the canonical form of a constant path and whether it
/// has wildcards.
pub(crate) fn check_path(path_val: &Value) -> Result<(Vec<u8>, JsonPathType)> {
    if path_val.is_null() {
        return Err(Error::binder("JSON path cannot be NULL"));
    }
    let text = path_val.to_varchar()?;
    let bytes = text.as_bytes();
    if bytes.is_empty() {
        return Ok((Vec::new(), JsonPathType::Regular));
    }
    let path = if bytes[0] == b'/' || bytes[0] == b'$' {
        bytes.to_vec()
    } else if path_val.logical_type().is_integral() {
        [b"$[".as_slice(), bytes, b"]"].concat()
    } else if bytes.contains(&b'"') {
        [b"/".as_slice(), bytes].concat()
    } else {
        [b"$.\"".as_slice(), bytes, b"\""].concat()
    };
    let path_type = if path[0] == b'$' {
        validate_path(&path, true)?
    } else {
        JsonPathType::Regular
    };
    Ok((path, path_type))
}

/// `JSONReadFunctionData::Bind`.
pub(crate) fn read_bind(
    function: &mut ScalarFunction,
    input: &BindInput<'_>,
) -> Result<Option<Arc<dyn FunctionData>>> {
    let mut data = JsonReadFunctionData::default();
    if let Some(Some(path_val)) = input.constants.get(1)
        && !path_val.is_null()
    {
        let (path, path_type) = check_path(path_val)?;
        data.constant = true;
        data.path = path;
        data.path_type = path_type;
    }
    let path_is_integral = input.argument_types.get(1).is_some_and(LogicalType::is_integral);
    if function.arguments.len() > 1 {
        function.arguments[1] = if path_is_integral {
            LogicalType::BIGINT
        } else {
            LogicalType::VARCHAR
        };
    }
    if data.path_type == JsonPathType::Wildcard {
        function.return_type = LogicalType::list(function.return_type.clone());
    }
    Ok(Some(Arc::new(data)))
}

/// `JSONReadManyFunctionData::Bind`.
pub(crate) fn read_many_bind(
    _function: &mut ScalarFunction,
    input: &BindInput<'_>,
) -> Result<Option<Arc<dyn FunctionData>>> {
    let Some(Some(paths_val)) = input.constants.get(1) else {
        return Err(Error::binder("List of paths must be constant"));
    };
    let mut paths = Vec::new();
    for path_val in paths_val.children().unwrap_or_default() {
        let (path, path_type) = check_path(path_val)?;
        if path_type == JsonPathType::Wildcard {
            return Err(Error::binder(
                "Cannot have wildcards in JSON path when supplying multiple paths",
            ));
        }
        paths.push(path);
    }
    Ok(Some(Arc::new(JsonReadManyFunctionData { paths })))
}

/// `AddAliases`: the same overloads under several names.
fn add_aliases(names: &[&str], set: &ScalarFunctionSet, functions: &mut Vec<ScalarFunctionSet>) {
    for name in names {
        let mut aliased = ScalarFunctionSet::new(*name);
        for function in &set.functions {
            let mut function = function.clone();
            function.name = Arc::from(*name);
            aliased.add(function);
        }
        functions.push(aliased);
    }
}

/// Whether `ty` is the JSON type (`LogicalType::IsJSONType`).
pub(crate) fn is_json_type(ty: &LogicalType) -> bool {
    ty.id() == LogicalTypeId::Varchar && ty.alias() == Some("JSON")
}

/// The input types of the read functions: VARCHAR and JSON.
pub(crate) fn input_types() -> [LogicalType; 2] {
    [LogicalType::VARCHAR, json_type()]
}

/// `JSONFunctions::GetScalarFunctions`.
pub fn json_scalar_functions() -> Vec<ScalarFunctionSet> {
    let mut functions = Vec::new();
    add_aliases(
        &["json_extract", "json_extract_path"],
        &json_extract::extract_functions(),
        &mut functions,
    );
    add_aliases(
        &["json_extract_string", "json_extract_path_text", "->>"],
        &json_extract::extract_string_functions(),
        &mut functions,
    );
    functions.push(json_create::array_function());
    functions.push(json_create::object_function());
    add_aliases(
        &["to_json", "json_quote"],
        &json_create::to_json_function(),
        &mut functions,
    );
    functions.push(json_create::array_to_json_function());
    functions.push(json_create::row_to_json_function());
    functions.push(json_merge_patch::merge_patch_function());
    functions.push(json_structure::structure_function());
    add_aliases(
        &["json_transform", "from_json"],
        &json_transform::transform_function(),
        &mut functions,
    );
    add_aliases(
        &["json_transform_strict", "from_json_strict"],
        &json_transform::transform_strict_function(),
        &mut functions,
    );
    functions.push(json_array_length::array_length_function());
    functions.push(json_contains::contains_function());
    functions.push(json_exists::exists_function());
    functions.push(json_keys::keys_function());
    functions.push(json_type::type_function());
    functions.push(json_valid::valid_function());
    functions.push(json_value::value_function());
    functions.push(json_serialize_sql::serialize_sql_function());
    functions.push(json_serialize_plan::serialize_plan_function());
    functions.push(json_serialize_sql::deserialize_sql_function());
    functions.push(json_pretty::pretty_function());
    functions
}

/// `JSONFunctions::GetTableFunctions`.
pub fn json_table_functions() -> Vec<TableFunctionSet> {
    let mut functions = read_json::read_json_functions();
    functions.extend(json_table_in_out::in_out_functions());
    functions.push(json_serialize_sql::execute_serialized_sql_function());
    functions
}

/// The `json`, `ndjson` and `jsonl` COPY formats.
pub fn json_copy_functions() -> Vec<CopyFunction> {
    copy_json::json_copy_functions()
}
