//! STRUCT functions (DuckDB's `function/scalar/struct` and `core_functions/scalar/struct`).

pub mod remap_struct;
pub mod struct_extract;

use std::collections::HashSet;
use std::sync::Arc;

use rustdb_common::{Error, Result};
use rustdb_function::{BindInput, FunctionData, NullHandling, ScalarFunction, ScalarFunctionSet};
use rustdb_row::sort::{NullOrder, OrderModifiers, OrderType, create_sort_keys};
use rustdb_types::{DataChunk, LogicalType, LogicalTypeId, Value, Vector};

use crate::scalar::executor::addressable;
use crate::scalar::nested::{all_constant, serialize_variable_return, struct_field};

/// Builds a STRUCT vector of `rows` rows from its field vectors.
pub fn build_struct(ty: LogicalType, fields: Vec<Vector>, rows: usize, constant: bool) -> Result<Vector> {
    let mut result = Vector::flat(ty, rows.max(1));
    {
        let children = result
            .struct_children_mut()
            .ok_or_else(|| Error::internal("struct result without fields"))?;
        if children.len() != fields.len() {
            return Err(Error::internal("struct field count mismatch"));
        }
        for (child, field) in children.iter_mut().zip(fields) {
            *child = field.flattened(rows.max(1));
        }
    }
    if constant {
        result.make_constant();
    }
    Ok(result)
}

/// Lowercased names, for DuckDB's case-insensitive name sets.
fn lower(name: &str) -> String {
    name.to_lowercase()
}

/// DuckDB's `StructPackBind`: field names from the argument aliases (`struct_pack`) or
/// unnamed fields (`row`).
fn bind_struct_pack(
    named: bool,
) -> impl Fn(&mut ScalarFunction, &BindInput<'_>) -> Result<Option<Arc<dyn FunctionData>>> + Send + Sync {
    move |function, input| {
        if input.argument_types.is_empty() {
            return Err(Error::invalid_input("Can't pack nothing into a struct"));
        }
        let mut seen = HashSet::new();
        let mut children = Vec::with_capacity(input.argument_types.len());
        for (ty, alias) in input.argument_types.iter().zip(input.aliases) {
            let name = if named {
                if alias.is_empty() {
                    return Err(Error::binder(
                        "Need named argument for struct pack, e.g. STRUCT_PACK(a := b)",
                    ));
                }
                if !seen.insert(lower(alias)) {
                    return Err(Error::binder(format!("Duplicate struct entry name \"{alias}\"")));
                }
                alias.clone()
            } else {
                String::new()
            };
            children.push((name, ty.clone()));
        }
        function.return_type = LogicalType::struct_type(children);
        Ok(None)
    }
}

/// DuckDB's `StructPackFunction`: the arguments become the fields.
fn struct_pack(args: &DataChunk, result: &mut Vector) -> Result<()> {
    let constant = all_constant(args.columns());
    let rows = if constant { 1 } else { args.len() };
    let fields = args
        .columns()
        .iter()
        .map(|column| addressable(column, rows).into_owned())
        .collect();
    *result = build_struct(result.logical_type().clone(), fields, rows, constant)?;
    Ok(())
}

fn struct_pack_set(name: &str, named: bool) -> ScalarFunctionSet {
    let mut function = ScalarFunction::new(
        name,
        Vec::new(),
        LogicalType::simple(LogicalTypeId::Struct),
        |args, _, result| struct_pack(args, result),
    )
    .with_bind(bind_struct_pack(named));
    function.varargs = Some(LogicalType::ANY);
    function.null_handling = NullHandling::Special;
    function.serialize = Some(Arc::new(serialize_variable_return));
    let mut set = ScalarFunctionSet::new(name);
    set.add(function);
    set
}

/// `struct_pack(name := value, ...)`.
pub fn struct_pack_functions() -> ScalarFunctionSet {
    struct_pack_set("struct_pack", true)
}

/// `row(value, ...)`: an unnamed struct.
pub fn row_functions() -> ScalarFunctionSet {
    struct_pack_set("row", false)
}

/// DuckDB's `StructConcatBind`.
fn bind_struct_concat(
    function: &mut ScalarFunction,
    input: &BindInput<'_>,
) -> Result<Option<Arc<dyn FunctionData>>> {
    if input.argument_types.is_empty() {
        return Err(Error::invalid_input(
            "struct_concat: At least one argument is required",
        ));
    }
    let mut combined = Vec::new();
    let mut names: Vec<String> = Vec::new();
    let mut has_unnamed = false;
    for (index, ty) in input.argument_types.iter().enumerate() {
        if ty.id() == LogicalTypeId::Unknown {
            return Err(crate::parameter_not_resolved());
        }
        let Some(children) = ty.struct_children().filter(|_| ty.id() == LogicalTypeId::Struct) else {
            return Err(Error::invalid_input(format!(
                "struct_concat: Argument at position \"{}\" is not a STRUCT",
                index + 1
            )));
        };
        for (name, child) in children {
            if name.is_empty() {
                has_unnamed = true;
            } else {
                if let Some(existing) = names.iter().find(|existing| lower(existing) == lower(name)) {
                    if existing == name {
                        return Err(Error::invalid_input(format!(
                            "struct_concat: Arguments contain duplicate STRUCT entry \"{name}\""
                        )));
                    }
                    return Err(Error::invalid_input(format!(
                        "struct_concat: Arguments contain case-insensitive duplicate STRUCT entry \"{name}\" and \
                         \"{existing}\""
                    )));
                }
                names.push(name.clone());
            }
            combined.push((name.clone(), child.clone()));
        }
    }
    if has_unnamed && !names.is_empty() {
        return Err(Error::invalid_input(
            "struct_concat: Cannot mix named and unnamed STRUCTs",
        ));
    }
    function.return_type = LogicalType::struct_type(combined);
    Ok(None)
}

/// `struct_concat(struct, ...)`.
pub fn struct_concat_functions() -> ScalarFunctionSet {
    let mut function = ScalarFunction::new(
        "struct_concat",
        Vec::new(),
        LogicalType::simple(LogicalTypeId::Struct),
        |args, _, result| {
            let constant = all_constant(args.columns());
            let rows = if constant { 1 } else { args.len() };
            let mut fields = Vec::new();
            for column in args.columns() {
                let count = column.logical_type().struct_children().map_or(0, <[_]>::len);
                for index in 0..count {
                    fields.push(struct_field(column, index, rows)?);
                }
            }
            *result = build_struct(result.logical_type().clone(), fields, rows, constant)?;
            Ok(())
        },
    )
    .with_bind(bind_struct_concat);
    function.varargs = Some(LogicalType::ANY);
    function.null_handling = NullHandling::Special;
    let mut set = ScalarFunctionSet::new("struct_concat");
    set.add(function);
    set
}

/// The aliases of the arguments after the first, with DuckDB's checks for `struct_insert`
/// and `struct_update`.
fn named_arguments(input: &BindInput<'_>, what: &str) -> Result<Vec<(String, LogicalType)>> {
    let mut named = Vec::new();
    for (ty, alias) in input.argument_types.iter().zip(input.aliases).skip(1) {
        if alias.is_empty() {
            return Err(Error::binder(format!(
                "Need named argument for struct {what}, e.g., a := b"
            )));
        }
        named.push((alias.clone(), ty.clone()));
    }
    Ok(named)
}

fn check_struct_argument(
    input: &BindInput<'_>,
    name: &str,
    verb: &str,
) -> Result<Vec<(String, LogicalType)>> {
    let Some(first) = input.argument_types.first() else {
        return Err(Error::invalid_input(format!(
            "Missing required arguments for {name} function."
        )));
    };
    if first.id() != LogicalTypeId::Struct {
        return Err(Error::invalid_input(format!(
            "The first argument to {name} must be a STRUCT"
        )));
    }
    if input.argument_types.len() < 2 {
        return Err(Error::invalid_input(format!(
            "Can't {verb} nothing into a STRUCT"
        )));
    }
    Ok(first.struct_children().unwrap_or_default().to_vec())
}

/// `struct_insert(struct, name := value, ...)`.
pub fn struct_insert_functions() -> ScalarFunctionSet {
    let mut function = ScalarFunction::new(
        "struct_insert",
        Vec::new(),
        LogicalType::simple(LogicalTypeId::Struct),
        |args, _, result| {
            let constant = all_constant(args.columns());
            let rows = if constant { 1 } else { args.len() };
            let base = args.column(0);
            let count = base.logical_type().struct_children().map_or(0, <[_]>::len);
            let mut fields = (0..count)
                .map(|index| struct_field(base, index, rows))
                .collect::<Result<Vec<_>>>()?;
            fields.extend(
                args.columns()
                    .iter()
                    .skip(1)
                    .map(|column| addressable(column, rows).into_owned()),
            );
            *result = build_struct(result.logical_type().clone(), fields, rows, constant)?;
            Ok(())
        },
    )
    .with_bind(|function, input| {
        let mut children = check_struct_argument(input, "struct_insert", "insert")?;
        let mut seen: HashSet<String> = children.iter().map(|(name, _)| lower(name)).collect();
        for (alias, ty) in named_arguments(input, "insert")? {
            if !seen.insert(lower(&alias)) {
                return Err(Error::binder(format!("Duplicate struct entry name \"{alias}\"")));
            }
            children.push((alias, ty));
        }
        function.return_type = LogicalType::struct_type(children);
        Ok(None)
    });
    function.varargs = Some(LogicalType::ANY);
    function.null_handling = NullHandling::Special;
    function.serialize = Some(Arc::new(serialize_variable_return));
    let mut set = ScalarFunctionSet::new("struct_insert");
    set.add(function);
    set
}

/// The layout of a `struct_update` result: for each output field, the input it comes from
/// (`Ok(field)` of the struct, `Err(argument)` of the arguments).
#[derive(Debug, Clone, PartialEq, Eq)]
struct StructUpdateLayout(Vec<std::result::Result<usize, usize>>);

impl FunctionData for StructUpdateLayout {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// `struct_update(struct, name := value, ...)`: replaces or adds fields.
pub fn struct_update_functions() -> ScalarFunctionSet {
    let mut function = ScalarFunction::new(
        "struct_update",
        Vec::new(),
        LogicalType::simple(LogicalTypeId::Struct),
        |args, state, result| {
            let layout = state
                .bind_data
                .as_ref()
                .and_then(|data| data.as_any().downcast_ref::<StructUpdateLayout>())
                .ok_or_else(|| Error::internal("struct_update without bind data"))?;
            let constant = all_constant(args.columns());
            let rows = if constant { 1 } else { args.len() };
            let fields = layout
                .0
                .iter()
                .map(|source| match source {
                    Ok(field) => struct_field(args.column(0), *field, rows),
                    Err(argument) => Ok(addressable(args.column(*argument), rows).into_owned()),
                })
                .collect::<Result<Vec<_>>>()?;
            *result = build_struct(result.logical_type().clone(), fields, rows, constant)?;
            Ok(())
        },
    )
    .with_bind(|function, input| {
        let existing = check_struct_argument(input, "struct_update", "update")?;
        let incoming = named_arguments(input, "update")?;
        for (index, (alias, _)) in incoming.iter().enumerate() {
            if incoming[..index]
                .iter()
                .any(|(other, _)| lower(other) == lower(alias))
            {
                return Err(Error::invalid_input(format!(
                    "Duplicate named argument provided for {alias}"
                )));
            }
        }
        let mut is_new = vec![true; incoming.len()];
        let mut children = Vec::new();
        let mut layout = Vec::new();
        for (field, (name, ty)) in existing.iter().enumerate() {
            if let Some(argument) = incoming.iter().position(|(alias, _)| lower(alias) == lower(name)) {
                children.push(incoming[argument].clone());
                layout.push(Err(argument + 1));
                is_new[argument] = false;
            } else {
                children.push((name.clone(), ty.clone()));
                layout.push(Ok(field));
            }
        }
        for (argument, child) in incoming.iter().enumerate() {
            if is_new[argument] {
                children.push(child.clone());
                layout.push(Err(argument + 1));
            }
        }
        function.return_type = LogicalType::struct_type(children);
        Ok(Some(Arc::new(StructUpdateLayout(layout))))
    });
    function.varargs = Some(LogicalType::ANY);
    function.null_handling = NullHandling::Special;
    function.serialize = Some(Arc::new(serialize_variable_return));
    let mut set = ScalarFunctionSet::new("struct_update");
    set.add(function);
    set
}

/// `struct_keys(struct) -> VARCHAR[]`.
pub fn struct_keys_functions() -> ScalarFunctionSet {
    let function = ScalarFunction::new(
        "struct_keys",
        vec![LogicalType::ANY],
        LogicalType::list(LogicalType::VARCHAR),
        |args, _, result| {
            let input = args.column(0);
            let keys: Vec<Value> = input
                .logical_type()
                .struct_children()
                .unwrap_or_default()
                .iter()
                .map(|(name, _)| Value::varchar(name))
                .collect();
            let keys = Value::list(LogicalType::VARCHAR, keys);
            let null = Value::null(LogicalType::list(LogicalType::VARCHAR));
            let rows = if input.is_constant() { 1 } else { args.len() };
            let mut out = Vector::flat(LogicalType::list(LogicalType::VARCHAR), rows.max(1));
            for row in 0..rows {
                out.set_value(row, if input.is_null(row) { &null } else { &keys })?;
            }
            if input.is_constant() {
                out.make_constant();
            }
            *result = out;
            Ok(())
        },
    )
    .with_bind(|_, input| {
        let ty = &input.argument_types[0];
        if ty.id() != LogicalTypeId::Struct {
            return Err(Error::invalid_input("struct_keys() expects a STRUCT argument"));
        }
        if ty.is_unnamed_struct() {
            return Err(Error::invalid_input(
                "struct_keys() cannot be applied to an unnamed STRUCT",
            ));
        }
        Ok(None)
    });
    let mut set = ScalarFunctionSet::new("struct_keys");
    set.add(function);
    set
}

/// `struct_values(struct)`: the same fields, unnamed.
pub fn struct_values_functions() -> ScalarFunctionSet {
    let function = ScalarFunction::new(
        "struct_values",
        vec![LogicalType::simple(LogicalTypeId::Struct)],
        LogicalType::simple(LogicalTypeId::Struct),
        |args, _, result| {
            let input = args.column(0);
            let rows = if input.is_constant() { 1 } else { args.len() };
            let count = input.logical_type().struct_children().map_or(0, <[_]>::len);
            let fields = (0..count)
                .map(|index| struct_field(input, index, rows))
                .collect::<Result<Vec<_>>>()?;
            let mut out = build_struct(result.logical_type().clone(), fields, rows, false)?;
            for row in 0..rows {
                if input.is_null(row) {
                    out.set_null(row);
                }
            }
            if input.is_constant() {
                out.make_constant();
            }
            *result = out;
            Ok(())
        },
    )
    .with_bind(|function, input| {
        let ty = input.argument_types[0].clone();
        if ty.id() == LogicalTypeId::Unknown {
            return Err(crate::parameter_not_resolved());
        }
        let unnamed = ty
            .struct_children()
            .unwrap_or_default()
            .iter()
            .map(|(_, child)| (String::new(), child.clone()))
            .collect();
        function.arguments[0] = ty;
        function.return_type = LogicalType::struct_type(unnamed);
        Ok(None)
    });
    let mut set = ScalarFunctionSet::new("struct_values");
    set.add(function);
    set
}

/// DuckDB's `StructContainsBind`: the target is cast to the common type of the fields.
fn bind_struct_contains(
    function: &mut ScalarFunction,
    input: &BindInput<'_>,
) -> Result<Option<Arc<dyn FunctionData>>> {
    let ty = &input.argument_types[0];
    if ty.id() == LogicalTypeId::Unknown {
        return Err(crate::parameter_not_resolved());
    }
    if ty.id() == LogicalTypeId::SqlNull {
        function.arguments = vec![LogicalType::UNKNOWN, LogicalType::UNKNOWN];
        function.return_type = LogicalType::SQLNULL;
        return Ok(None);
    }
    let children = ty.struct_children().unwrap_or_default();
    if children.is_empty() {
        return Err(Error::internal("Can't check for containment in an empty struct"));
    }
    if !ty.is_unnamed_struct() {
        return Err(Error::binder(format!(
            "{} can only be used on unnamed structs",
            function.name
        )));
    }
    let mut max_type = input.argument_types[1].clone();
    let mut new_children = Vec::with_capacity(children.len());
    for (name, child) in children {
        match LogicalType::try_max_logical_type(child, &max_type) {
            Some(max) => {
                max_type = max;
                new_children.push((name.clone(), max_type.clone()));
                function.arguments[1] = max_type.clone();
            }
            None => new_children.push((name.clone(), child.clone())),
        }
    }
    function.arguments[0] = LogicalType::struct_type(new_children);
    Ok(None)
}

/// `struct_contains`/`struct_position` (DuckDB's `StructSearchFunction`), comparing values
/// through their sort keys.
fn struct_search(args: &DataChunk, result: &mut Vector, position: bool) -> Result<()> {
    if result.logical_type().id() == LogicalTypeId::SqlNull {
        *result = Vector::constant_null(LogicalType::SQLNULL);
        return Ok(());
    }
    let constant = all_constant(args.columns());
    let rows = if constant { 1 } else { args.len() };
    let input = addressable(args.column(0), rows);
    let target = addressable(args.column(1), rows);
    let modifiers = [OrderModifiers::new(OrderType::Ascending, NullOrder::NullsLast)];
    let target_keys = create_sort_keys(&[&target], &modifiers, rows)?;
    let target_physical = target.logical_type().physical_type();
    let children = input
        .logical_type()
        .struct_children()
        .unwrap_or_default()
        .to_vec();
    let mut members = Vec::new();
    for (index, (_, ty)) in children.iter().enumerate() {
        if ty.physical_type() == target_physical {
            let field = struct_field(&input, index, rows)?;
            let keys = create_sort_keys(&[&field], &modifiers, rows)?;
            members.push(Some((field, keys)));
        } else {
            members.push(None);
        }
    }
    if position && members.iter().all(Option::is_none) {
        *result = Vector::constant_null(result.logical_type().clone());
        return Ok(());
    }
    let mut out = Vector::flat(result.logical_type().clone(), rows.max(1));
    for row in 0..rows {
        if input.is_null(row) {
            out.set_null(row);
            continue;
        }
        let target_valid = !target.is_null(row);
        if !position && !target_valid {
            out.set_null(row);
            continue;
        }
        let mut found = None;
        for (index, member) in members.iter().enumerate() {
            let Some((field, keys)) = member else {
                continue;
            };
            let field_valid = !field.is_null(row);
            let matches = (position && !field_valid && !target_valid)
                || (field_valid && target_valid && keys.key(row) == target_keys.key(row));
            if matches {
                found = Some(index);
            }
        }
        match (found, position) {
            (Some(index), true) => out.values_mut::<i32>()[row] = index as i32 + 1,
            (Some(_), false) => out.values_mut::<bool>()[row] = true,
            (None, true) => out.validity_mut().set_invalid(row, rows),
            (None, false) => out.values_mut::<bool>()[row] = false,
        }
    }
    if constant {
        out.make_constant();
    }
    *result = out;
    Ok(())
}

/// `struct_contains`/`struct_has`.
pub fn struct_contains_functions() -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new("struct_contains");
    set.add(
        ScalarFunction::new(
            "struct_contains",
            vec![LogicalType::simple(LogicalTypeId::Struct), LogicalType::ANY],
            LogicalType::BOOLEAN,
            |args, _, result| struct_search(args, result, false),
        )
        .with_bind(bind_struct_contains),
    );
    set
}

/// `struct_position`/`struct_indexof`.
pub fn struct_position_functions() -> ScalarFunctionSet {
    let mut function = ScalarFunction::new(
        "struct_position",
        vec![LogicalType::simple(LogicalTypeId::Struct), LogicalType::ANY],
        LogicalType::INTEGER,
        |args, _, result| struct_search(args, result, true),
    )
    .with_bind(bind_struct_contains);
    function.null_handling = NullHandling::Special;
    let mut set = ScalarFunctionSet::new("struct_position");
    set.add(function);
    set
}
