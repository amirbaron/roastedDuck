//! `ALTER TABLE ... ADD / DROP / RENAME COLUMN` of a field inside a nested column
//! (DuckDB's `DuckTableEntry::AddField`, `RemoveField` and `RenameField`): each is an
//! `ALTER COLUMN TYPE` of the top-level column to its new type, converted with
//! `remap_struct(column, NULL::new_type, mapping, defaults)`.

use roastedduck_ast::{AddFieldInfo, ParsedExpression, RemoveFieldInfo, RenameFieldInfo, TableColumnType};
use roastedduck_catalog::TableEntry;
use roastedduck_common::{ByteString, Error, Result};
use roastedduck_types::{LogicalType, LogicalTypeId, Value};

/// The column's new type and the `remap_struct` call converting it.
pub(super) struct FieldChange {
    pub column: ByteString,
    pub new_type: LogicalType,
    pub expression: ParsedExpression,
}

/// `PackExpression`: `struct_pack(expr AS name)`.
fn pack(mut expression: ParsedExpression, name: &ByteString) -> ParsedExpression {
    expression.alias.clone_from(name);
    ParsedExpression::function("struct_pack", vec![expression])
}

/// `GetChildList`: the children of a LIST (`list`), MAP (`key`, `value`) or STRUCT.
fn child_list(ty: &LogicalType) -> Result<Vec<(ByteString, LogicalType)>> {
    match ty.id() {
        LogicalTypeId::List => Ok(vec![(
            ByteString::from("list"),
            ty.child_type().cloned().unwrap_or_default(),
        )]),
        LogicalTypeId::Map => {
            let (key, value) = ty
                .map_key_value()
                .ok_or_else(|| Error::internal("MAP without key and value types"))?;
            Ok(vec![
                (ByteString::from("key"), key.clone()),
                (ByteString::from("value"), value.clone()),
            ])
        }
        LogicalTypeId::Struct => Ok(ty.struct_children().unwrap_or_default().to_vec()),
        _ => Err(Error::binder(roastedduck_common::byte_format!(
            "Can't ConstructMapping for type '{ty}'"
        ))),
    }
}

/// `ConstructNewType`: `original` with the children `children`.
fn construct_new_type(
    original: &LogicalType,
    mut children: Vec<(ByteString, LogicalType)>,
) -> Result<LogicalType> {
    match original.id() {
        LogicalTypeId::Struct => Ok(LogicalType::struct_type(children)),
        LogicalTypeId::List if children.len() == 1 => Ok(LogicalType::list(children.remove(0).1)),
        LogicalTypeId::Map if children.len() == 2 => {
            let value = children.remove(1).1;
            Ok(LogicalType::map(children.remove(0).1, value))
        }
        _ => Err(Error::binder(roastedduck_common::byte_format!(
            "Type '{original}' not supported for ADD COLUMN"
        ))),
    }
}

/// `ConstructMapping`: the identity mapping of a field `name` of type `ty`.
fn construct_mapping(name: &ByteString, ty: &LogicalType) -> Result<Value> {
    if !ty.is_nested() {
        return Value::try_varchar(name.as_bytes());
    }
    let mut mapping = Vec::new();
    for (child_name, child_type) in child_list(ty)? {
        let child_mapping = nested_mapping(
            &child_name,
            &child_type,
            construct_mapping(&child_name, &child_type)?,
        )?;
        mapping.push((child_name, child_mapping));
    }
    Ok(Value::struct_value(mapping))
}

/// The mapping of a nested child is the pair (source name, child mapping).
fn nested_mapping(name: &ByteString, ty: &LogicalType, mapping: Value) -> Result<Value> {
    Ok(if ty.is_nested() {
        Value::struct_value(vec![
            (ByteString::new(), Value::try_varchar(name.as_bytes())?),
            (ByteString::new(), mapping),
        ])
    } else {
        mapping
    })
}

/// Whether `component` names the child `name` of a value of type `ty` (`element` names
/// a list's elements).
fn names_child(ty: &LogicalType, component: &ByteString, name: &ByteString) -> bool {
    (ty.id() == LogicalTypeId::List && component.eq_ignore_ascii_case("element"))
        || name.eq_ignore_ascii_case(component)
}

/// The column's new type and the value of the new field, or the `IF NOT EXISTS` error
/// (`AddFieldToStruct`).
fn add_field_to_struct(
    ty: &LogicalType,
    path: &[ByteString],
    info: &AddFieldInfo,
    depth: usize,
) -> Result<std::result::Result<(LogicalType, ParsedExpression), Error>> {
    if !ty.is_nested() {
        return Err(Error::binder(roastedduck_common::byte_format!(
            "Column '{}' is not a nested type, ADD COLUMN can only be used on nested types",
            path[depth]
        )));
    }
    let mut children = child_list(ty)?;
    let current = &path[depth];
    let field = &info.new_field;
    if depth + 1 == path.len() {
        if ty.id() != LogicalTypeId::Struct {
            return Err(Error::binder(roastedduck_common::byte_format!(
                "Column {current} is not a struct - ALTER TABLE can only add fields to structs"
            )));
        }
        if children
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case(&field.name))
        {
            return Ok(Err(Error::catalog(roastedduck_common::byte_format!(
                "Duplicate field \"{}\" - field already exists in struct {current}",
                field.name
            ))));
        }
        children.push((field.name.clone(), field.logical_type.clone()));
        let default = match (&field.expression, field.category) {
            (Some(default), TableColumnType::Standard) => (**default).clone(),
            _ => ParsedExpression::constant(Value::null(field.logical_type.clone())),
        };
        return Ok(Ok((
            LogicalType::struct_type(children),
            pack(default, &field.name),
        )));
    }
    let next = &path[depth + 1];
    let Some(child) = children.iter_mut().find(|(name, _)| names_child(ty, next, name)) else {
        return Err(Error::binder(roastedduck_common::byte_format!(
            "Sub-field {next} does not exist in column {current}"
        )));
    };
    let (child_type, default) = match add_field_to_struct(&child.1, path, info, depth + 1)? {
        Ok(result) => result,
        Err(error) => return Ok(Err(error)),
    };
    child.1 = child_type;
    let packed_name = if ty.id() == LogicalTypeId::List {
        ByteString::from("list")
    } else {
        child.0.clone()
    };
    Ok(Ok((
        construct_new_type(ty, children)?,
        pack(default, &packed_name),
    )))
}

/// How a struct changes when one field is dropped or renamed: the mapping of the new
/// type's fields to the old ones (`DroppedFieldMapping`).
enum FieldEdit<'a> {
    Drop,
    Rename(&'a ByteString),
}

/// `DropFieldFromStruct` / `RenameFieldFromStruct`: the new type and mapping, or the
/// not-found error that `IF EXISTS` suppresses.
fn edit_field(
    ty: &LogicalType,
    path: &[ByteString],
    edit: &FieldEdit<'_>,
    depth: usize,
) -> Result<std::result::Result<(LogicalType, Value), Error>> {
    if !ty.is_nested() {
        let verb = match edit {
            FieldEdit::Drop => "drop",
            FieldEdit::Rename(_) => "rename",
        };
        return Err(Error::catalog(roastedduck_common::byte_format!(
            "Cannot {verb} field from column \"{}\" - not a nested type",
            path[0]
        )));
    }
    let target = &path[depth];
    let last = depth + 1 == path.len();
    let children = child_list(ty)?;
    let mut found = false;
    let mut mapping = Vec::with_capacity(children.len());
    let mut new_children = Vec::with_capacity(children.len());
    for (name, child_type) in &children {
        let mut field_name = name.clone();
        let (child_mapping, new_type) = if names_child(ty, target, name) {
            found = true;
            if last {
                match edit {
                    FieldEdit::Drop => {
                        if ty.id() != LogicalTypeId::Struct {
                            return Err(Error::catalog(roastedduck_common::byte_format!(
                                "Cannot drop field '{}' from column '{}' - it's not a struct",
                                path[path.len() - 1],
                                path[0]
                            )));
                        }
                        if children.len() == 1 {
                            return Err(Error::catalog(roastedduck_common::byte_format!(
                                "Cannot drop field {} from column {} - it is the last field of the struct",
                                path[path.len() - 1],
                                path[0]
                            )));
                        }
                        continue;
                    }
                    FieldEdit::Rename(new_name) => {
                        if ty.id() != LogicalTypeId::Struct {
                            return Err(Error::catalog(roastedduck_common::byte_format!(
                                "Cannot rename field '{}' from column '{}' - can only rename fields inside a struct",
                                path[path.len() - 1],
                                path[0]
                            )));
                        }
                        if children
                            .iter()
                            .any(|(other, _)| other.eq_ignore_ascii_case(new_name))
                        {
                            return Err(Error::catalog(roastedduck_common::byte_format!(
                                "Cannot rename field {} from column {} to {new_name} - a field with this name already exists",
                                path[path.len() - 1],
                                path[0]
                            )));
                        }
                        (*new_name).clone_into(&mut field_name);
                        (construct_mapping(name, child_type)?, child_type.clone())
                    }
                }
            } else {
                match edit_field(child_type, path, edit, depth + 1)? {
                    Ok((new_type, child_mapping)) => (child_mapping, new_type),
                    Err(error) => return Ok(Err(error)),
                }
            }
        } else {
            (construct_mapping(name, child_type)?, child_type.clone())
        };
        mapping.push((
            field_name.clone(),
            nested_mapping(name, child_type, child_mapping)?,
        ));
        new_children.push((field_name, new_type));
    }
    if !found {
        let message = match edit {
            FieldEdit::Drop => {
                roastedduck_common::byte_format!("Cannot drop field \"{target}\" - it does not exist")
            }
            FieldEdit::Rename(_) => {
                roastedduck_common::byte_format!("Cannot rename field \"{target}\" - it does not exist")
            }
        };
        return Ok(Err(Error::catalog(message)));
    }
    Ok(Ok((
        construct_new_type(ty, new_children)?,
        Value::struct_value(mapping),
    )))
}

/// `remap_struct(column, NULL::new_type, mapping, defaults)`.
fn remap(
    column: &ByteString,
    new_type: &LogicalType,
    mapping: Value,
    defaults: ParsedExpression,
) -> ParsedExpression {
    ParsedExpression::function(
        "remap_struct",
        vec![
            ParsedExpression::column_ref([column]),
            ParsedExpression::constant(Value::null(new_type.clone())),
            ParsedExpression::constant(mapping),
            defaults,
        ],
    )
}

/// `DuckTableEntry::AddField`; `None` if the field exists and `IF NOT EXISTS` is given.
pub(super) fn add_field(table: &TableEntry, info: &AddFieldInfo) -> Result<Option<FieldChange>> {
    let Some((_, column)) = table.column(&info.column_path[0]) else {
        return Err(Error::internal(roastedduck_common::byte_format!(
            "Column with name \"{}\" does not exist",
            info.column_path[0]
        )));
    };
    let (new_type, default) = match add_field_to_struct(&column.logical_type, &info.column_path, info, 0)? {
        Ok(result) => result,
        Err(_) if info.if_field_not_exists => return Ok(None),
        Err(error) => return Err(error),
    };
    let mapping = construct_mapping(&column.name, &column.logical_type)?;
    Ok(Some(FieldChange {
        column: info.column_path[0].clone(),
        expression: remap(&info.column_path[0], &new_type, mapping, default),
        new_type,
    }))
}

/// `DuckTableEntry::RemoveField`; `None` if the column or field is missing and
/// `IF EXISTS` is given.
pub(super) fn remove_field(table: &TableEntry, info: &RemoveFieldInfo) -> Result<Option<FieldChange>> {
    let Some((_, column)) = table.column(&info.column_path[0]) else {
        if info.if_column_exists {
            return Ok(None);
        }
        return Err(Error::catalog(roastedduck_common::byte_format!(
            "Cannot drop field from column \"{}\" - it does not exist",
            info.column_path[0]
        )));
    };
    let (new_type, mapping) = match edit_field(&column.logical_type, &info.column_path, &FieldEdit::Drop, 1)?
    {
        Ok(result) => result,
        Err(_) if info.if_column_exists => return Ok(None),
        Err(error) => return Err(error),
    };
    Ok(Some(FieldChange {
        column: info.column_path[0].clone(),
        expression: remap(
            &info.column_path[0],
            &new_type,
            mapping,
            ParsedExpression::constant(Value::null(LogicalType::SQLNULL)),
        ),
        new_type,
    }))
}

/// `DuckTableEntry::RenameField`.
pub(super) fn rename_field(table: &TableEntry, info: &RenameFieldInfo) -> Result<FieldChange> {
    let Some((_, column)) = table.column(&info.column_path[0]) else {
        return Err(Error::catalog(roastedduck_common::byte_format!(
            "Cannot rename field from column \"{}\" - it does not exist",
            info.column_path[0]
        )));
    };
    let edit = FieldEdit::Rename(&info.new_name);
    let (new_type, mapping) = edit_field(&column.logical_type, &info.column_path, &edit, 1)??;
    Ok(FieldChange {
        column: info.column_path[0].clone(),
        expression: remap(
            &info.column_path[0],
            &new_type,
            mapping,
            ParsedExpression::constant(Value::null(LogicalType::SQLNULL)),
        ),
        new_type,
    })
}
