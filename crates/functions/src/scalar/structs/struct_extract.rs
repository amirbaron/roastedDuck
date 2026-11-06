//! `struct_extract` and `struct_extract_at` (DuckDB's `struct_extract.cpp`).

use std::sync::Arc;

use rustdb_common::string_util::{candidates_message_bytes, top_n_jaro_winkler};
use rustdb_common::{ByteString, Error, Result};
use rustdb_function::{BindInput, FunctionData, ScalarFunction, ScalarFunctionSet};
use rustdb_types::{LogicalType, LogicalTypeId};

use crate::scalar::nested::struct_field;
use crate::values::as_bigint;

/// The extracted field (DuckDB's `StructExtractBindData`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FieldIndex(pub(crate) usize);

impl FunctionData for FieldIndex {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// The field types of a STRUCT argument, with DuckDB's checks.
fn struct_children<'a>(input: &'a BindInput<'_>) -> Result<&'a [(ByteString, LogicalType)]> {
    let ty = &input.argument_types[0];
    if ty.id() == LogicalTypeId::Unknown {
        return Err(crate::parameter_not_resolved());
    }
    let children = ty.struct_children().unwrap_or_default();
    if children.is_empty() {
        return Err(Error::internal("Can't extract something from an empty struct"));
    }
    Ok(children)
}

/// DuckDB's `StructExtractBind`: a field by (case-insensitive) name.
fn bind_key(function: &mut ScalarFunction, input: &BindInput<'_>) -> Result<Option<Arc<dyn FunctionData>>> {
    let children = struct_children(input)?;
    if input.argument_types[0].is_unnamed_struct() {
        return Err(Error::binder(
            "struct_extract with a string key cannot be used on an unnamed struct, use a numeric index instead",
        ));
    }
    function.arguments[0] = input.argument_types[0].clone();
    if input.argument_types[1].id() == LogicalTypeId::Unknown {
        return Err(crate::parameter_not_resolved());
    }
    let key = match &input.constants[1] {
        Some(value) if input.argument_types[1].id() == LogicalTypeId::Varchar => value,
        _ => {
            return Err(Error::binder(
                "Key name for struct_extract needs to be a constant string",
            ));
        }
    };
    let key = key.as_bytes().unwrap_or_default();
    if key.is_empty() {
        return Err(Error::binder(
            "Key name for struct_extract needs to be neither NULL nor empty",
        ));
    }
    let key = ByteString::from(key.to_ascii_lowercase());
    let Some(index) = children
        .iter()
        .position(|(name, _)| name.eq_ignore_ascii_case(&key))
    else {
        let names: Vec<ByteString> = children.iter().map(|(name, _)| name.clone()).collect();
        let message = candidates_message_bytes(&top_n_jaro_winkler(&names, &key), b"Candidate Entries");
        return Err(Error::binder(rustdb_common::byte_format!(
            "Could not find key \"{key}\" in struct\n{message}"
        )));
    };
    function.return_type = children[index].1.clone();
    Ok(Some(Arc::new(FieldIndex(index))))
}

/// DuckDB's `StructExtractBindInternal`: a field by 1-based index.
fn bind_index(
    struct_extract: bool,
) -> impl Fn(&mut ScalarFunction, &BindInput<'_>) -> Result<Option<Arc<dyn FunctionData>>> + Send + Sync {
    move |function, input| {
        let children = struct_children(input)?;
        if struct_extract && !input.argument_types[0].is_unnamed_struct() {
            return Err(Error::binder(
                "struct_extract with an integer key can only be used on unnamed structs, use a string key instead",
            ));
        }
        function.arguments[0] = input.argument_types[0].clone();
        if input.argument_types[1].id() == LogicalTypeId::Unknown {
            return Err(crate::parameter_not_resolved());
        }
        let Some(key) = &input.constants[1] else {
            return Err(Error::binder(
                "Key index for struct_extract needs to be a constant value",
            ));
        };
        let index = as_bigint(key)?;
        if index <= 0 || index as u64 > children.len() as u64 {
            return Err(Error::binder(rustdb_common::byte_format!(
                "Key index {index} for struct_extract out of range - expected an index between 1 and {}",
                children.len()
            )));
        }
        let index = index as usize - 1;
        function.return_type = children[index].1.clone();
        Ok(Some(Arc::new(FieldIndex(index))))
    }
}

/// The kernel: references the chosen field (DuckDB's `StructExtractFunction`).
fn extract(
    name: &str,
    key: LogicalType,
    bind: impl Fn(&mut ScalarFunction, &BindInput<'_>) -> Result<Option<Arc<dyn FunctionData>>>
    + Send
    + Sync
    + 'static,
) -> ScalarFunction {
    ScalarFunction::new(
        name,
        vec![LogicalType::simple(LogicalTypeId::Struct), key],
        LogicalType::ANY,
        |args, state, result| {
            let index = state
                .bind_data
                .as_ref()
                .and_then(|data| data.as_any().downcast_ref::<FieldIndex>())
                .ok_or_else(|| Error::internal("struct_extract without bind data"))?;
            *result = struct_field(args.column(0), index.0, args.len())?;
            Ok(())
        },
    )
    .with_bind(bind)
}

/// `struct_extract(struct, 'name')` under `name`.
pub(crate) fn key_extract(name: &str) -> ScalarFunction {
    extract(name, LogicalType::VARCHAR, bind_key)
}

/// `struct_extract(struct, index)` (unnamed structs) under `name`.
pub(crate) fn index_extract(name: &str) -> ScalarFunction {
    extract(name, LogicalType::BIGINT, bind_index(true))
}

/// The `struct_extract` set.
pub fn struct_extract_functions() -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new("struct_extract");
    set.add(key_extract("struct_extract"));
    set.add(index_extract("struct_extract"));
    set
}

/// `struct_extract_at(struct, index)`.
pub fn struct_extract_at_functions() -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new("struct_extract_at");
    set.add(extract(
        "struct_extract_at",
        LogicalType::BIGINT,
        bind_index(false),
    ));
    set
}
