//! UNION functions (DuckDB's `core_functions/scalar/union`): `union_extract`,
//! `union_tag` and `union_value`.
//!
//! A UNION vector is a STRUCT vector whose first field holds the member tag (UTINYINT)
//! and whose other fields hold the members.

use std::sync::Arc;

use rustdb_common::string_util::{candidates_message, top_n_jaro_winkler};
use rustdb_common::{Error, Result};
use rustdb_function::{BindInput, FunctionData, NullHandling, ScalarFunction, ScalarFunctionSet};
use rustdb_types::{DataChunk, LogicalType, LogicalTypeId, Value, Vector};

use crate::scalar::executor::addressable;
use crate::scalar::nested::{all_constant, struct_field};
use crate::scalar::structs::build_struct;
use crate::scalar::structs::struct_extract::FieldIndex;

type BindResult = Result<Option<Arc<dyn FunctionData>>>;

/// The members of the UNION argument, or DuckDB's bind error.
fn union_members<'a>(input: &'a BindInput<'_>, error: &str) -> Result<&'a [(String, LogicalType)]> {
    let ty = &input.argument_types[0];
    if ty.id() == LogicalTypeId::Unknown {
        return Err(crate::parameter_not_resolved());
    }
    if ty.id() != LogicalTypeId::Union {
        return Err(Error::binder(error.to_owned()));
    }
    match ty.union_members() {
        Some(members) if !members.is_empty() => Ok(members),
        _ => Err(Error::internal("Can't extract something from an empty union")),
    }
}

/// DuckDB's `UnionExtractBind`: resolves the member by (case-insensitive) name.
fn bind_union_extract(function: &mut ScalarFunction, input: &BindInput<'_>) -> BindResult {
    let members = union_members(input, "union_extract can only take a union parameter")?;
    function.arguments[0] = input.argument_types[0].clone();
    if input.argument_types[1].id() == LogicalTypeId::Unknown {
        return Err(crate::parameter_not_resolved());
    }
    let key = match &input.constants[1] {
        Some(value) if input.argument_types[1].id() == LogicalTypeId::Varchar => value,
        _ => {
            return Err(Error::binder(
                "Key name for union_extract needs to be a constant string",
            ));
        }
    };
    let key = key.as_str().unwrap_or_default();
    if key.is_empty() {
        return Err(Error::binder(
            "Key name for union_extract needs to be neither NULL nor empty",
        ));
    }
    let key = key.to_lowercase();
    let Some(index) = members.iter().position(|(name, _)| name.to_lowercase() == key) else {
        let names: Vec<String> = members.iter().map(|(name, _)| name.clone()).collect();
        let message = candidates_message(&top_n_jaro_winkler(&names, &key), "Candidate Entries");
        return Err(Error::binder(format!(
            "Could not find key \"{key}\" in union\n{message}"
        )));
    };
    function.return_type = members[index].1.clone();
    // The member is field `index + 1` of the underlying struct (field 0 is the tag).
    Ok(Some(Arc::new(FieldIndex(index + 1))))
}

/// `union_extract(union, 'member')`: the member's value, NULL for rows holding another
/// member.
pub fn union_extract_functions() -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new("union_extract");
    set.add(
        ScalarFunction::new(
            "union_extract",
            vec![LogicalType::simple(LogicalTypeId::Union), LogicalType::VARCHAR],
            LogicalType::ANY,
            |args, state, result| {
                let index = state
                    .bind_data
                    .as_ref()
                    .and_then(|data| data.as_any().downcast_ref::<FieldIndex>())
                    .ok_or_else(|| Error::internal("union_extract without bind data"))?;
                *result = struct_field(args.column(0), index.0, args.len())?;
                Ok(())
            },
        )
        .with_bind(bind_union_extract),
    );
    set
}

/// DuckDB's `UnionTagBind`: the result is an ENUM of the member names.
fn bind_union_tag(function: &mut ScalarFunction, input: &BindInput<'_>) -> BindResult {
    let members = union_members(
        input,
        "First argument to union_tag function must be a union type.",
    )?;
    function.arguments[0] = input.argument_types[0].clone();
    function.return_type = LogicalType::enum_type(members.iter().map(|(name, _)| name.clone()).collect());
    Ok(None)
}

/// `union_tag(union)`: the name of the member each row holds.
pub fn union_tag_functions() -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new("union_tag");
    set.add(
        ScalarFunction::new(
            "union_tag",
            vec![LogicalType::simple(LogicalTypeId::Union)],
            LogicalType::ANY,
            |args, _, result| {
                let tags = struct_field(args.column(0), 0, args.len())?;
                *result = tags.reinterpret(result.logical_type().clone())?;
                Ok(())
            },
        )
        .with_bind(bind_union_tag),
    );
    set
}

/// DuckDB's `UnionValueBind`: a one-member UNION named by the argument alias.
fn bind_union_value(function: &mut ScalarFunction, input: &BindInput<'_>) -> BindResult {
    if input.argument_types.len() != 1 {
        return Err(Error::binder("union_value takes exactly one argument"));
    }
    let alias = &input.aliases[0];
    if alias.is_empty() {
        return Err(Error::binder(
            "Need named argument for union tag, e.g. UNION_VALUE(a := b)",
        ));
    }
    function.return_type = LogicalType::union_type(vec![(alias.clone(), input.argument_types[0].clone())]);
    Ok(None)
}

/// DuckDB's `UnionValueFunction`: tag 0 and the argument as the only member.
fn union_value(args: &DataChunk, result: &mut Vector) -> Result<()> {
    let constant = all_constant(args.columns());
    let rows = if constant { 1 } else { args.len() };
    let member = addressable(args.column(0), rows).into_owned();
    let tags = Vector::constant(&Value::utinyint(0))?.flattened(rows);
    *result = build_struct(result.logical_type().clone(), vec![tags, member], rows, constant)?;
    Ok(())
}

/// `union_value(name := value)`.
pub fn union_value_functions() -> ScalarFunctionSet {
    let mut function = ScalarFunction::new(
        "union_value",
        Vec::new(),
        LogicalType::simple(LogicalTypeId::Union),
        |args, _, result| union_value(args, result),
    )
    .with_bind(bind_union_value);
    function.varargs = Some(LogicalType::ANY);
    function.null_handling = NullHandling::Special;
    function.serialize = Some(std::sync::Arc::new(
        crate::scalar::nested::serialize_variable_return,
    ));
    let mut set = ScalarFunctionSet::new("union_value");
    set.add(function);
    set
}
