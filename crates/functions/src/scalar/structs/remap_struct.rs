//! `remap_struct(input, target, remaps, defaults)` (DuckDB's
//! `function/scalar/struct/remap_struct.cpp`): rebuilds nested values (STRUCT, and the
//! structs inside LIST and MAP) in the shape of `target`, taking each target field from
//! a renamed source field or from a constant default.

use std::any::Any;
use std::sync::Arc;

use rustdb_common::{CaseInsensitiveMap, Error, Result};
use rustdb_function::{BindInput, FunctionData, NullHandling, ScalarFunction, ScalarFunctionSet};
use rustdb_types::{DataChunk, LogicalType, LogicalTypeId, Value, Vector};

/// `IsRemappable`: a nested type (`LogicalType::IsNested`) other than VARIANT.
fn is_remappable(ty: &LogicalType) -> bool {
    matches!(
        ty.physical_type(),
        rustdb_types::PhysicalType::Struct
            | rustdb_types::PhysicalType::List
            | rustdb_types::PhysicalType::Array
    ) && ty.id() != LogicalTypeId::Variant
}

/// How one target field is produced (`RemapColumnInfo`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct RemapColumnInfo {
    /// The source field it comes from.
    index: Option<usize>,
    /// The field of the defaults it comes from.
    default_index: Option<usize>,
    /// For nested fields: how the fields inside are produced.
    children: Vec<RemapColumnInfo>,
}

#[derive(Debug)]
struct RemapStructBindData {
    remap: Vec<RemapColumnInfo>,
}

impl FunctionData for RemapStructBindData {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// The fields of a remappable type: STRUCT fields, `list` for a LIST and `key`/`value`
/// for a MAP.
fn type_fields(ty: &LogicalType) -> Result<Vec<(String, LogicalType)>> {
    match ty.id() {
        LogicalTypeId::Struct => Ok(ty.struct_children().unwrap_or_default().to_vec()),
        LogicalTypeId::List => Ok(vec![(
            "list".to_owned(),
            ty.child_type().cloned().unwrap_or(LogicalType::INVALID),
        )]),
        LogicalTypeId::Map => {
            let (key, value) = ty
                .map_key_value()
                .ok_or_else(|| Error::internal("MAP without key and value types"))?;
            Ok(vec![
                ("key".to_owned(), key.clone()),
                ("value".to_owned(), value.clone()),
            ])
        }
        _ => Err(Error::binder(format!("Can't remap type {ty}"))),
    }
}

/// A field of the source or target type (`RemapIndex`).
#[derive(Debug, Clone)]
struct RemapIndex {
    index: usize,
    ty: LogicalType,
    children: Option<CaseInsensitiveMap<RemapIndex>>,
}

impl RemapIndex {
    /// `RemapIndex::GetMap`.
    fn map(ty: &LogicalType) -> Result<CaseInsensitiveMap<Self>> {
        let mut result = CaseInsensitiveMap::default();
        for (index, (name, child)) in type_fields(ty)?.into_iter().enumerate() {
            let children = if is_remappable(&child) {
                Some(Self::map(&child)?)
            } else {
                None
            };
            result.insert(
                name,
                Self {
                    index,
                    ty: child,
                    children,
                },
            );
        }
        Ok(result)
    }
}

/// The remapping of one target field (`RemapEntry`).
#[derive(Debug, Default)]
struct RemapEntry {
    index: Option<usize>,
    default_index: Option<usize>,
    target_type: LogicalType,
    children: Option<CaseInsensitiveMap<RemapEntry>>,
}

/// A struct's field names and values, or `None` for NULL.
fn struct_fields(value: &Value) -> Option<Vec<(String, Value)>> {
    if value.is_null() {
        return None;
    }
    let names = value.logical_type().struct_children()?;
    let values = value.children()?;
    Some(
        names
            .iter()
            .map(|(name, _)| name.clone())
            .zip(values.iter().cloned())
            .collect(),
    )
}

/// `RemapEntry::PerformRemap`.
fn perform_remap(
    remap_target: &str,
    remap_value: &Value,
    source_map: &CaseInsensitiveMap<RemapIndex>,
    target_map: &CaseInsensitiveMap<RemapIndex>,
    result: &mut CaseInsensitiveMap<RemapEntry>,
) -> Result<()> {
    let (remap_source, struct_value) = match remap_value.logical_type().id() {
        LogicalTypeId::Varchar => (remap_value.to_varchar()?, None),
        LogicalTypeId::Struct => {
            if !remap_value.logical_type().is_unnamed_struct() {
                return Err(Error::binder(
                    "Remap keys for remap_struct needs to be an unnamed struct",
                ));
            }
            let children = remap_value.children().unwrap_or_default();
            let [source, nested] = children else {
                return Err(Error::binder(
                    "Remap keys for remap_struct needs to have two children",
                ));
            };
            if source.logical_type().id() != LogicalTypeId::Varchar
                || nested.logical_type().id() != LogicalTypeId::Struct
            {
                return Err(Error::binder(
                    "Remap keys for remap_struct need to be varchar and struct",
                ));
            }
            (source.to_varchar()?, (!nested.is_null()).then(|| nested.clone()))
        }
        _ => {
            return Err(Error::binder(
                "Remap keys for remap_struct needs to be a string or struct",
            ));
        }
    };
    let source = source_map
        .get(&remap_source)
        .ok_or_else(|| Error::binder(format!("Source value {remap_source} not found")))?;
    let target = target_map
        .get(remap_target)
        .ok_or_else(|| Error::binder(format!("Target value {remap_target} not found")))?;
    let source_nested = is_remappable(&source.ty);
    let target_nested = is_remappable(&target.ty);
    let mut remap = RemapEntry {
        index: Some(source.index),
        target_type: target.ty.clone(),
        ..RemapEntry::default()
    };
    if source_nested || target_nested || struct_value.is_some() {
        if source.ty.id() != target.ty.id() {
            return Err(Error::binder(format!(
                "Can't change source type ({}) to target type ({}), type conversion not allowed",
                source.ty, target.ty
            )));
        }
        if let Some(struct_value) = struct_value {
            if !source_nested || !target_nested {
                return Err(Error::binder(format!(
                    "Found a struct value ({}) as a remap, this is only expected for a nested type, source type is \
                     '{}', target type is '{}'",
                    struct_value.to_varchar()?,
                    source.ty,
                    target.ty
                )));
            }
            let mut children = CaseInsensitiveMap::default();
            let (Some(source_children), Some(target_children)) = (&source.children, &target.children) else {
                return Err(Error::internal("nested remap without child maps"));
            };
            for (name, value) in struct_fields(&struct_value).unwrap_or_default() {
                perform_remap(&name, &value, source_children, target_children, &mut children)?;
            }
            remap.children = Some(children);
        }
    }
    result.insert(remap_target, remap);
    Ok(())
}

/// `RemapEntry::HandleDefault`.
fn handle_default(
    default_index: usize,
    default_target: &str,
    default_type: &LogicalType,
    target_map: &CaseInsensitiveMap<RemapIndex>,
    result: &mut CaseInsensitiveMap<RemapEntry>,
) -> Result<()> {
    let target = target_map
        .get(default_target)
        .ok_or_else(|| Error::binder(format!("Default value {default_target} not found for remap")))?;
    if default_type.id() == LogicalTypeId::Struct {
        if !is_remappable(&target.ty) {
            return Err(Error::binder(format!(
                "Default value is a struct - target value should be a nested type, is '{}'",
                target.ty
            )));
        }
        if let Some(existing) = result.get_mut(default_target) {
            existing.default_index = Some(default_index);
        } else {
            result.insert(
                default_target,
                RemapEntry {
                    default_index: Some(default_index),
                    children: Some(CaseInsensitiveMap::default()),
                    ..RemapEntry::default()
                },
            );
        }
        let entry = result
            .get_mut(default_target)
            .ok_or_else(|| Error::internal("remap entry vanished"))?;
        let (Some(children), Some(target_children)) = (entry.children.as_mut(), target.children.as_ref())
        else {
            return Err(Error::binder("No child remaps found"));
        };
        for (index, (name, child_type)) in default_type
            .struct_children()
            .unwrap_or_default()
            .iter()
            .enumerate()
        {
            handle_default(index, name, child_type, target_children, children)?;
        }
        return Ok(());
    }
    if default_type != &target.ty {
        return Err(Error::binder(format!(
            "Default key {default_target} does not match target type {} - add a cast",
            target.ty
        )));
    }
    if result.contains_key(default_target) {
        return Err(Error::binder(format!(
            "Duplicate value provided for target {default_target}"
        )));
    }
    result.insert(
        default_target,
        RemapEntry {
            default_index: Some(default_index),
            ..RemapEntry::default()
        },
    );
    Ok(())
}

/// `RemapEntry::ConstructMap`: the remapping of every field of `ty`.
fn construct_map(
    ty: &LogicalType,
    remap_map: &CaseInsensitiveMap<RemapEntry>,
) -> Result<Vec<RemapColumnInfo>> {
    type_fields(ty)?
        .into_iter()
        .map(|(name, child_type)| {
            let entry = remap_map
                .get(&name)
                .ok_or_else(|| Error::binder(format!("Missing target value {name} for remap")))?;
            let children = match (&entry.children, is_remappable(&child_type)) {
                (Some(children), true) => construct_map(&child_type, children)?,
                _ => Vec::new(),
            };
            Ok(RemapColumnInfo {
                index: entry.index,
                default_index: entry.default_index,
                children,
            })
        })
        .collect()
}

/// `RemapEntry::RemapCast`: the source type with remapped fields cast to their target
/// types.
fn remap_cast(ty: &LogicalType, remap_map: &CaseInsensitiveMap<RemapEntry>) -> Result<LogicalType> {
    let fields = type_fields(ty)?;
    let remapped: Vec<(String, LogicalType)> = fields
        .into_iter()
        .enumerate()
        .map(|(index, (name, child_type))| {
            let entry = remap_map.iter().find(|(_, entry)| entry.index == Some(index));
            let child_type = match entry {
                None => child_type,
                Some((_, entry)) => match (&entry.children, is_remappable(&child_type)) {
                    (Some(children), true) => remap_cast(&child_type, children)?,
                    _ => entry.target_type.clone(),
                },
            };
            Ok((name, child_type))
        })
        .collect::<Result<_>>()?;
    match ty.id() {
        LogicalTypeId::Struct => Ok(LogicalType::struct_type(remapped)),
        LogicalTypeId::List => Ok(LogicalType::list(remapped[0].1.clone())),
        LogicalTypeId::Map => Ok(LogicalType::map(remapped[0].1.clone(), remapped[1].1.clone())),
        _ => Err(Error::binder(format!("Can't RemapCast for type '{ty}'"))),
    }
}

/// `RemapStructBind`.
fn bind(function: &mut ScalarFunction, input: &BindInput<'_>) -> Result<Option<Arc<dyn FunctionData>>> {
    let types = input.argument_types;
    for (index, ty) in types.iter().take(3).enumerate() {
        if ty.id() == LogicalTypeId::Unknown {
            return Err(crate::parameter_not_resolved());
        }
        if ty.id() == LogicalTypeId::SqlNull && index == 2 {
            continue;
        }
        if !is_remappable(ty) {
            return Err(Error::binder(format!(
                "Struct remap can only remap nested types, not '{ty}'"
            )));
        }
        if ty.id() == LogicalTypeId::Struct && ty.is_unnamed_struct() {
            return Err(Error::binder("Struct remap can only remap named structs"));
        }
    }
    let (from_type, to_type, defaults_type) = (&types[0], &types[1], &types[3]);
    if !matches!(defaults_type.id(), LogicalTypeId::SqlNull | LogicalTypeId::Struct) {
        return Err(Error::binder(
            "The defaults provided to 'remap_struct' should be of type STRUCT if they're not NULL",
        ));
    }
    if defaults_type.id() == LogicalTypeId::Struct && defaults_type.is_unnamed_struct() {
        return Err(Error::binder(
            "The defaults have to be either NULL or a named STRUCT, not an unnamed struct",
        ));
    }
    if (is_remappable(from_type) || is_remappable(to_type)) && from_type.id() != to_type.id() {
        return Err(Error::binder(format!(
            "Can't change source type ({from_type}) to target type ({to_type}), type conversion not allowed"
        )));
    }
    let Some(remap_value) = input.constants.get(2).cloned().flatten() else {
        return Err(Error::binder(
            "Remap keys for remap_struct needs to be a constant value",
        ));
    };
    let source_map = RemapIndex::map(from_type)?;
    let target_map = RemapIndex::map(to_type)?;
    let mut remap_map = CaseInsensitiveMap::default();
    for (target, value) in struct_fields(&remap_value).unwrap_or_default() {
        perform_remap(&target, &value, &source_map, &target_map, &mut remap_map)?;
    }
    if input.constants.get(3).is_none_or(Option::is_none) {
        return Err(Error::binder("Default values must be constants"));
    }
    if defaults_type.id() != LogicalTypeId::SqlNull {
        for (index, (name, ty)) in defaults_type
            .struct_children()
            .unwrap_or_default()
            .iter()
            .enumerate()
        {
            handle_default(index, name, ty, &target_map, &mut remap_map)?;
        }
    }
    let remap = construct_map(to_type, &remap_map)?;
    function.arguments[0] = remap_cast(from_type, &remap_map)?;
    function.arguments[1] = to_type.clone();
    function.arguments[2] = types[2].clone();
    function.arguments[3] = defaults_type.clone();
    function.return_type = to_type.clone();
    Ok(Some(Arc::new(RemapStructBindData { remap })))
}

/// `RemapChildVectors` for one row: the target fields from the source fields `inputs`.
fn remap_children(
    inputs: &[Value],
    defaults: &Value,
    remap: &[RemapColumnInfo],
    target_types: &[LogicalType],
) -> Result<Vec<Value>> {
    let default_field = |index: usize| -> Result<Value> {
        defaults
            .children()
            .and_then(|fields| fields.get(index))
            .cloned()
            .ok_or_else(|| Error::internal("remap default out of range"))
    };
    remap
        .iter()
        .zip(target_types)
        .map(|(info, ty)| match (info.index, info.default_index) {
            (Some(index), _) if !info.children.is_empty() => {
                let child_default = match info.default_index {
                    Some(default_index) => default_field(default_index)?,
                    None => defaults.clone(),
                };
                remap_nested(&inputs[index], &child_default, &info.children, ty)
            }
            (_, Some(default_index)) => default_field(default_index),
            (Some(index), None) => Ok(inputs[index].clone()),
            (None, None) => Err(Error::internal("remap without source or default")),
        })
        .collect()
}

/// `RemapNested` for one value of the (cast) source type.
fn remap_nested(
    value: &Value,
    defaults: &Value,
    remap: &[RemapColumnInfo],
    ty: &LogicalType,
) -> Result<Value> {
    if value.is_null() {
        return Ok(Value::null(ty.clone()));
    }
    let children = value.children().unwrap_or_default();
    let field_types: Vec<LogicalType> = type_fields(ty)?.into_iter().map(|(_, ty)| ty).collect();
    match ty.id() {
        LogicalTypeId::Struct => Ok(Value::from_children(
            ty.clone(),
            remap_children(children, defaults, remap, &field_types)?,
        )),
        LogicalTypeId::List => {
            let elements = children
                .iter()
                .map(|element| {
                    remap_children(std::slice::from_ref(element), defaults, remap, &field_types)
                        .map(|mut fields| fields.remove(0))
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(Value::from_children(ty.clone(), elements))
        }
        LogicalTypeId::Map => {
            let entry_type = ty.child_type().cloned().unwrap_or(LogicalType::INVALID);
            let entries = children
                .iter()
                .map(|entry| {
                    let fields = entry.children().unwrap_or_default();
                    remap_children(fields, defaults, remap, &field_types)
                        .map(|fields| Value::from_children(entry_type.clone(), fields))
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(Value::from_children(ty.clone(), entries))
        }
        _ => Err(Error::invalid_input(format!("Can't RemapNested for type '{ty}'"))),
    }
}

/// `RemapStructFunction`.
fn kernel(args: &DataChunk, data: &RemapStructBindData, result: &mut Vector) -> Result<()> {
    let count = args.len();
    let ty = result.logical_type().clone();
    let mut out = Vector::flat(ty.clone(), count.max(1));
    for row in 0..count {
        let value = args.column(0).get_value(row);
        let defaults = args.column(3).get_value(row);
        out.set_value(row, &remap_nested(&value, &defaults, &data.remap, &ty)?)?;
    }
    if args.columns().iter().all(Vector::is_constant) {
        out.make_constant();
    }
    *result = out;
    Ok(())
}

/// `remap_struct`.
pub fn remap_struct_functions() -> ScalarFunctionSet {
    let mut function = ScalarFunction::new(
        "remap_struct",
        vec![LogicalType::ANY; 4],
        LogicalType::ANY,
        |args, state, result| {
            let data = state
                .bind_data
                .as_ref()
                .and_then(|data| data.as_any().downcast_ref::<RemapStructBindData>())
                .ok_or_else(|| Error::internal("remap_struct without bind data"))?;
            kernel(args, data, result)
        },
    )
    .with_bind(bind);
    function.null_handling = NullHandling::Special;
    let mut set = ScalarFunctionSet::new("remap_struct");
    set.add(function);
    set
}
