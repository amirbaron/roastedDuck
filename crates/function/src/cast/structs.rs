//! Casts from STRUCT and UNION and to UNION (DuckDB's `struct_cast.cpp` and
//! `union_casts.cpp`).

use std::sync::Arc;

use roastedduck_common::{ByteString, Error, ErrorKind, Result, byte_format as format};
use roastedduck_types::format::{NestedQuoting, write_nested_string};
use roastedduck_types::{ListEntry, LogicalType, LogicalTypeId, ValidityMask, Vector};

use super::executor::{StringColumn, cast_rows_to_string};
use super::nested::{NestedInput, cast_child, child_quoting, missing_child, write_element};
use super::{BoundCastInfo, CastFunctionSet, CastParameters};

/// `Type A does not match with B. <message>` (DuckDB's `TypeMismatchException`).
fn type_mismatch(source: &LogicalType, target: &LogicalType, message: &str) -> Error {
    Error::new_bytes(
        ErrorKind::MismatchType,
        format!("Type {source} does not match with {target}. {message}").into_bytes(),
    )
}

/// How the fields of a STRUCT map onto the fields of another (DuckDB's
/// `StructBoundCastData`).
pub(super) struct StructCastPlan {
    /// `(source field, target field, cast)`.
    children: Vec<(usize, usize, BoundCastInfo)>,
    /// Target fields without a source: always NULL.
    null_targets: Vec<usize>,
}

impl StructCastPlan {
    /// DuckDB's `BindStructToStructCast`: fields match by name (case-insensitively) unless
    /// either side is unnamed, in which case they match by position.
    pub(super) fn bind(set: &CastFunctionSet, source: &LogicalType, target: &LogicalType) -> Result<Self> {
        let source_fields = source.struct_children().ok_or_else(|| missing_child(source))?;
        let target_fields = target.struct_children().ok_or_else(|| missing_child(target))?;
        let unnamed = target_fields.is_empty()
            || target.is_unnamed_struct()
            || source_fields.is_empty()
            || source.is_unnamed_struct();
        if unnamed && source_fields.len() != target_fields.len() {
            return Err(type_mismatch(
                source,
                target,
                "Cannot cast STRUCTs of different size",
            ));
        }
        let mut unmatched: Vec<Option<usize>> = (0..target_fields.len()).map(Some).collect();
        if !unnamed {
            for (i, (name, _)) in target_fields.iter().enumerate() {
                if target_fields[..i]
                    .iter()
                    .any(|(other, _)| other.eq_ignore_ascii_case(name))
                {
                    return Err(Error::new_bytes(
                        ErrorKind::NotImplemented,
                        format!("Error while casting - duplicate name \"{name}\" in struct").into_bytes(),
                    ));
                }
            }
        }
        let mut children = Vec::new();
        for (source_index, (name, source_type)) in source_fields.iter().enumerate() {
            let target_index = if unnamed {
                source_index
            } else {
                let found = unmatched
                    .iter_mut()
                    .find(|slot| slot.is_some_and(|index| target_fields[index].0.eq_ignore_ascii_case(name)));
                match found.and_then(Option::take) {
                    Some(index) => index,
                    None => continue,
                }
            };
            if unnamed {
                unmatched[target_index] = None;
            }
            let cast = set.get_cast_function(source_type, &target_fields[target_index].1)?;
            children.push((source_index, target_index, cast));
        }
        if !unnamed && children.is_empty() {
            return Err(Error::binder(
                "STRUCT to STRUCT cast must have at least one matching member",
            ));
        }
        Ok(Self {
            children,
            null_targets: unmatched.into_iter().flatten().collect(),
        })
    }

    /// DuckDB's `StructToStructCast`.
    pub(super) fn execute(
        &self,
        source: &Vector,
        result: &mut Vector,
        count: usize,
        params: &mut CastParameters,
    ) -> Result<bool> {
        let input = NestedInput::new(source, count);
        let target = result.logical_type().clone();
        let target_fields = target.struct_children().ok_or_else(|| missing_child(&target))?;
        let source_children = input
            .vector
            .struct_children()
            .ok_or_else(|| missing_child(&target))?;
        let mut out = Vector::flat(target.clone(), input.rows);
        let mut all_converted = true;
        {
            let out_children = out.struct_children_mut().ok_or_else(|| missing_child(&target))?;
            for (source_index, target_index, cast) in &self.children {
                let (child, converted) = cast_child(
                    cast,
                    &source_children[*source_index],
                    &target_fields[*target_index].1,
                    input.rows,
                    params,
                )?;
                out_children[*target_index] = child;
                all_converted &= converted;
            }
            for &index in &self.null_targets {
                out_children[index] = Vector::constant_null(target_fields[index].1.clone());
            }
        }
        out.set_validity(input.vector.validity().clone());
        *result = input.finish(out);
        Ok(all_converted)
    }
}

/// STRUCT -> VARCHAR: `{'name': value, ...}`, or `(v1, v2)` for unnamed structs.
fn struct_to_varchar(
    plan: &StructCastPlan,
    text_type: &LogicalType,
    source: &Vector,
    result: &mut Vector,
    count: usize,
    params: &mut CastParameters,
) -> Result<bool> {
    let source_type = source.logical_type().clone();
    let fields = source_type
        .struct_children()
        .ok_or_else(|| missing_child(&source_type))?;
    let unnamed = source_type.is_unnamed_struct();
    let mut text = Vector::flat(text_type.clone(), count);
    plan.execute(source, &mut text, count, params)?;
    let children = text.struct_children().ok_or_else(|| missing_child(text_type))?;
    let quoting: Vec<Option<NestedQuoting>> = fields.iter().map(|(_, ty)| child_quoting(ty)).collect();
    cast_rows_to_string(&text, result, count, params, |slot, out| {
        out.push(if unnamed { b'(' } else { b'{' });
        for (i, ((name, _), child)) in fields.iter().zip(children).enumerate() {
            if i > 0 {
                out.extend_from_slice(b", ");
            }
            if !unnamed {
                write_nested_string(name.as_bytes(), NestedQuoting::Key, out);
                out.extend_from_slice(b": ");
            }
            write_element(child, slot, quoting[i], out);
        }
        out.push(if unnamed { b')' } else { b'}' });
        Ok(())
    })
}

/// STRUCT -> MAP: one entry per field, keyed by the field name (DuckDB's
/// `StructToMapCast`).
fn struct_to_map(
    key_cast: &BoundCastInfo,
    value_casts: &[BoundCastInfo],
    source: &Vector,
    result: &mut Vector,
    count: usize,
    params: &mut CastParameters,
) -> Result<bool> {
    let input = NestedInput::new(source, count);
    let target = result.logical_type().clone();
    let (key_type, value_type) = target.map_key_value().ok_or_else(|| missing_child(&target))?;
    let source_type = source.logical_type().clone();
    let fields = source_type
        .struct_children()
        .ok_or_else(|| missing_child(&source_type))?;
    let children = input
        .vector
        .struct_children()
        .ok_or_else(|| missing_child(&source_type))?;
    let field_count = fields.len();
    let total = input.rows * field_count;

    let mut names = StringColumn::new();
    for _ in 0..input.rows {
        for (name, _) in fields {
            names.push(Some(name.as_bytes()));
        }
    }
    let (keys, keys_converted) = cast_child(key_cast, &names.finish(), key_type, total, params)?;
    // Interleave the converted fields: entry `row * field_count + field` is row `row` of
    // field `field`.
    let mut values_converted = true;
    let mut converted_fields = Vec::with_capacity(field_count);
    for (child, cast) in children.iter().zip(value_casts) {
        let (values, converted) = cast_child(cast, child, value_type, input.rows, params)?;
        values_converted &= converted;
        converted_fields.push(values);
    }
    let mut value_rows = Vec::with_capacity(total);
    for row in 0..input.rows {
        for field in 0..field_count {
            value_rows.push(Some((field * input.rows + row) as u32));
        }
    }
    let values = concat_vectors(&converted_fields, input.rows, value_type)?.select_rows(&value_rows);

    let entry_type = target.child_type().ok_or_else(|| missing_child(&target))?.clone();
    let mut entries = Vector::flat(entry_type, total);
    if let Some([key_slot, value_slot]) = entries.struct_children_mut() {
        *key_slot = keys;
        *value_slot = values;
    }
    let mut out = Vector::flat(target.clone(), input.rows);
    for (row, entry) in out
        .values_mut::<ListEntry>()
        .iter_mut()
        .enumerate()
        .take(input.rows)
    {
        *entry = ListEntry::new(row * field_count, field_count);
    }
    out.set_validity(input.vector.validity().clone());
    out.set_list_child(entries, total);
    *result = input.finish(out);
    Ok(keys_converted && values_converted)
}

/// Stacks equally typed vectors of `rows` rows each into one flat vector.
fn concat_vectors(parts: &[Vector], rows: usize, ty: &LogicalType) -> Result<Vector> {
    let mut out = Vector::flat(ty.clone(), parts.len() * rows);
    for (index, part) in parts.iter().enumerate() {
        for row in 0..rows {
            out.set_value(index * rows + row, &part.get_value(row))?;
        }
    }
    Ok(out)
}

/// DuckDB's `StructCastSwitch`.
fn from_struct(
    set: &CastFunctionSet,
    source: &LogicalType,
    target: &LogicalType,
) -> Result<Option<BoundCastInfo>> {
    Ok(Some(match target.id() {
        LogicalTypeId::Struct => {
            let plan = StructCastPlan::bind(set, source, target)?;
            BoundCastInfo::new(move |source, result, count, params| {
                plan.execute(source, result, count, params)
            })
        }
        LogicalTypeId::Varchar => {
            let fields = source.struct_children().ok_or_else(|| missing_child(source))?;
            let text_type = LogicalType::struct_type(
                fields
                    .iter()
                    .map(|(name, _)| (name.clone(), LogicalType::VARCHAR))
                    .collect(),
            );
            let plan = StructCastPlan::bind(set, source, &text_type)?;
            BoundCastInfo::new(move |source, result, count, params| {
                struct_to_varchar(&plan, &text_type, source, result, count, params)
            })
        }
        LogicalTypeId::Map => {
            if source.is_unnamed_struct() {
                return Err(type_mismatch(
                    source,
                    target,
                    "Cannot cast unnamed STRUCTs to MAP",
                ));
            }
            let (key_type, value_type) = target.map_key_value().ok_or_else(|| missing_child(target))?;
            let key_cast = set.get_cast_function(&LogicalType::VARCHAR, key_type)?;
            let value_casts = source
                .struct_children()
                .ok_or_else(|| missing_child(source))?
                .iter()
                .map(|(_, ty)| set.get_cast_function(ty, value_type))
                .collect::<Result<Vec<_>>>()?;
            BoundCastInfo::new(move |source, result, count, params| {
                struct_to_map(&key_cast, &value_casts, source, result, count, params)
            })
        }
        _ => return Ok(None),
    }))
}

/// How the members of a UNION map onto the members of another.
struct UnionCastPlan {
    /// Target member for every source member.
    tag_map: Vec<usize>,
    member_casts: Vec<BoundCastInfo>,
}

impl UnionCastPlan {
    /// DuckDB's `BindUnionToUnionCast`: members match by name, case-insensitively.
    fn bind(set: &CastFunctionSet, source: &LogicalType, target: &LogicalType) -> Result<Self> {
        let source_members = source.union_members().ok_or_else(|| missing_child(source))?;
        let target_members = target.union_members().ok_or_else(|| missing_child(target))?;
        let mut tag_map = Vec::with_capacity(source_members.len());
        let mut member_casts = Vec::with_capacity(source_members.len());
        for (name, member_type) in source_members {
            let Some(index) = target_members
                .iter()
                .position(|(target_name, _)| target_name.eq_ignore_ascii_case(name))
            else {
                return Err(Error::new_bytes(ErrorKind::Conversion, format!(
                    "Type {source} can't be cast as {target}. The member '{name}' is not present in target union"
                ).into_bytes()));
            };
            tag_map.push(index);
            member_casts.push(set.get_cast_function(member_type, &target_members[index].1)?);
        }
        Ok(Self {
            tag_map,
            member_casts,
        })
    }

    /// DuckDB's `UnionToUnionCast`.
    fn execute(
        &self,
        source: &Vector,
        result: &mut Vector,
        count: usize,
        params: &mut CastParameters,
    ) -> Result<bool> {
        let input = NestedInput::new(source, count);
        let target = result.logical_type().clone();
        let target_fields = target.struct_children().ok_or_else(|| missing_child(&target))?;
        let source_children = input
            .vector
            .struct_children()
            .ok_or_else(|| missing_child(&target))?;
        let mut out = Vector::flat(target.clone(), input.rows);
        let mut all_converted = true;
        {
            let out_children = out.struct_children_mut().ok_or_else(|| missing_child(&target))?;
            let mut mapped = vec![false; target_fields.len()];
            for (member, (target_member, cast)) in self.tag_map.iter().zip(&self.member_casts).enumerate() {
                // Children are the tag followed by the members.
                let (child, converted) = cast_child(
                    cast,
                    &source_children[member + 1],
                    &target_fields[target_member + 1].1,
                    input.rows,
                    params,
                )?;
                out_children[target_member + 1] = child;
                mapped[target_member + 1] = true;
                all_converted &= converted;
            }
            for (index, (_, ty)) in target_fields.iter().enumerate().skip(1) {
                if !mapped[index] {
                    out_children[index] = Vector::constant_null(ty.clone());
                }
            }
            let source_tags = &source_children[0];
            let mut tags = Vector::flat(LogicalType::UTINYINT, input.rows);
            let mut tag_validity = ValidityMask::all_valid();
            {
                let tag_values = tags.values_mut::<u8>();
                for (row, tag) in tag_values.iter_mut().enumerate().take(input.rows) {
                    match source_tags.get_value(row).as_u8() {
                        Some(source_tag) => {
                            let mapped_tag = self.tag_map.get(usize::from(source_tag)).copied().unwrap_or(0);
                            *tag = mapped_tag as u8;
                        }
                        None => tag_validity.set_invalid(row, input.rows),
                    }
                }
            }
            tags.set_validity(tag_validity);
            out_children[0] = tags;
        }
        let mut validity = input.vector.validity().clone();
        for row in 0..input.rows {
            if input.vector.is_null(row) || source_children[0].is_null(row) {
                validity.set_invalid(row, input.rows);
            }
        }
        out.set_validity(validity);
        *result = input.finish(out);
        Ok(all_converted)
    }
}

/// UNION -> VARCHAR: the text of the active member (`NULL` if the member is NULL).
fn union_to_varchar(
    plan: &UnionCastPlan,
    text_type: &LogicalType,
    source: &Vector,
    result: &mut Vector,
    count: usize,
    params: &mut CastParameters,
) -> Result<bool> {
    let mut text = Vector::flat(text_type.clone(), count);
    plan.execute(source, &mut text, count, params)?;
    let children = text.struct_children().ok_or_else(|| missing_child(text_type))?;
    cast_rows_to_string(&text, result, count, params, |slot, out| {
        let tag = children[0].get_value(slot).as_u8().map_or(0, usize::from);
        match children.get(tag + 1) {
            Some(member) if !member.is_null(slot) => out.extend_from_slice(member.string_bytes(slot)),
            _ => out.extend_from_slice(b"NULL"),
        }
        Ok(())
    })
}

/// DuckDB's `UnionCastSwitch`.
fn from_union(
    set: &CastFunctionSet,
    source: &LogicalType,
    target: &LogicalType,
) -> Result<Option<BoundCastInfo>> {
    Ok(Some(match target.id() {
        LogicalTypeId::Union => {
            let plan = UnionCastPlan::bind(set, source, target)?;
            BoundCastInfo::new(move |source, result, count, params| {
                plan.execute(source, result, count, params)
            })
        }
        LogicalTypeId::Varchar => {
            let members = source.union_members().ok_or_else(|| missing_child(source))?;
            let text_type = LogicalType::union_type(
                members
                    .iter()
                    .map(|(name, _)| (name.clone(), LogicalType::VARCHAR))
                    .collect(),
            );
            let plan = UnionCastPlan::bind(set, source, &text_type)?;
            let text_type = Arc::new(text_type);
            BoundCastInfo::new(move |source, result, count, params| {
                union_to_varchar(&plan, &text_type, source, result, count, params)
            })
        }
        _ => return Ok(None),
    }))
}

/// Any non-UNION value -> UNION: the value becomes the member it casts to most cheaply
/// (DuckDB's `ImplicitToUnionCast`).
/// `StructToUnionCast::AllowImplicitCastFromStruct`: a STRUCT laid out like the union
/// (same tag type, member names and types, or VARCHAR members) converts field by field.
fn struct_matches_union(source: &LogicalType, target: &LogicalType) -> bool {
    if source.id() != LogicalTypeId::Struct {
        return false;
    }
    let (Some(fields), Some(target_fields)) = (source.struct_children(), target.struct_children()) else {
        return false;
    };
    if fields.len() != target_fields.len() {
        return false;
    }
    fields
        .iter()
        .zip(target_fields)
        .enumerate()
        .all(|(index, ((name, ty), (target_name, target_ty)))| {
            if index == 0 {
                return ty == target_ty;
            }
            name.eq_ignore_ascii_case(target_name) && (ty == target_ty || *ty == LogicalType::VARCHAR)
        })
}

/// `StructToUnionCast::Cast`: the fields cast one by one, then the union checked
/// (`UnionVector::CheckUnionValidity`).
fn struct_to_union(
    set: &CastFunctionSet,
    source: &LogicalType,
    target: &LogicalType,
) -> Result<BoundCastInfo> {
    let (Some(fields), Some(target_fields)) = (source.struct_children(), target.struct_children()) else {
        return Err(missing_child(target));
    };
    let child_casts = fields
        .iter()
        .zip(target_fields)
        .map(|((_, from), (_, to))| set.get_cast_function(from, to))
        .collect::<Result<Vec<_>>>()?;
    Ok(BoundCastInfo::new(move |source, result, count, params| {
        let target = result.logical_type().clone();
        let source = source.flattened(count);
        let target_fields = target
            .struct_children()
            .ok_or_else(|| missing_child(&target))?
            .to_vec();
        let source_children = source.struct_children().ok_or_else(|| missing_child(&target))?;
        let mut children = Vec::with_capacity(child_casts.len());
        for ((cast, child), (_, ty)) in child_casts.iter().zip(source_children).zip(&target_fields) {
            let (converted, _) = cast_child(cast, child, ty, count, params)?;
            children.push(converted.flattened(count));
        }
        let null_rows: Vec<usize> = (0..count)
            .filter(|&row| source.is_null(row) || children[0].is_null(row))
            .collect();
        let mut out = Vector::flat(target.clone(), count.max(1));
        if let Some(slots) = out.struct_children_mut() {
            for (slot, child) in slots.iter_mut().zip(&children) {
                *slot = child.clone();
            }
        }
        // `FlatVector::SetNull`: the members of a NULL union row are NULL too.
        for row in null_rows {
            out.set_null(row);
        }
        // `CheckUnionValidity`.
        let members = children.len() - 1;
        for row in 0..count {
            if out.is_null(row) {
                continue;
            }
            let tag = children[0].get_value(row).as_i128().unwrap_or(-1);
            if tag < 0 || tag as usize >= members {
                return Err(Error::conversion(
                    "One or more of the tags do not point to a valid union member",
                ));
            }
            let mut found = false;
            for (member, child) in children.iter().enumerate().skip(1) {
                if child.is_null(row) {
                    continue;
                }
                if found {
                    return Err(Error::conversion(
                        "One or more rows in the produced UNION have validity set for more than 1 member",
                    ));
                }
                found = true;
                if tag as usize != member - 1 {
                    return Err(Error::conversion(
                        "One or more rows in the produced UNION have tags that don't point to the valid member",
                    ));
                }
            }
        }
        *result = out;
        Ok(true)
    }))
}

pub(super) fn to_union(
    set: &CastFunctionSet,
    source: &LogicalType,
    target: &LogicalType,
) -> Result<BoundCastInfo> {
    if struct_matches_union(source, target) {
        return struct_to_union(set, source, target);
    }
    let members = target.union_members().ok_or_else(|| missing_child(target))?;
    let mut candidates: Vec<(i64, usize)> = members
        .iter()
        .enumerate()
        .filter_map(|(index, (_, ty))| {
            let cost = set.implicit_cast_cost(source, ty);
            (cost != -1).then_some((cost, index))
        })
        .collect();
    if candidates.is_empty() {
        let names: Vec<ByteString> = members.iter().map(|(_, ty)| ty.to_bytes()).collect();
        return Err(Error::new_bytes(ErrorKind::Conversion, format!(
            "Type {source} can't be cast as {target}. {source} can't be implicitly cast to any of the union member types: {}",
            ByteString::join(&names, ", ")
        ).into_bytes()));
    }
    candidates.sort_by_key(|&(cost, _)| cost);
    let best = candidates[0].0;
    let tied: Vec<&(i64, usize)> = candidates.iter().filter(|(cost, _)| *cost == best).collect();
    if tied.len() > 1 {
        let options: Vec<ByteString> = tied
            .iter()
            .map(|&&(_, index)| format!("'{} ({})'", members[index].0, members[index].1))
            .collect();
        return Err(Error::new_bytes(ErrorKind::Conversion, format!(
            "Type {source} can't be cast as {target}. The cast is ambiguous, multiple possible members in target: {}. \
             Disambiguate the target type by using the 'union_value(<tag> := <arg>)' function to promote the source \
             value to a single member union before casting.",
            ByteString::join(&options, ", ")
        ).into_bytes()));
    }
    let tag = candidates[0].1;
    let member_type = members[tag].1.clone();
    let member_cast = set.get_cast_function(source, &member_type)?;
    Ok(BoundCastInfo::new(move |source, result, count, params| {
        let target = result.logical_type().clone();
        let (member, converted) = cast_child(&member_cast, source, &member_type, count, params)?;
        if !converted {
            return Ok(false);
        }
        let constant = member.is_constant();
        let rows = if constant { 1 } else { count };
        let mut out = Vector::flat(target.clone(), rows);
        {
            let fields = target.struct_children().ok_or_else(|| missing_child(&target))?;
            let children = out.struct_children_mut().ok_or_else(|| missing_child(&target))?;
            let mut tags = Vector::flat(LogicalType::UTINYINT, rows);
            tags.values_mut::<u8>()[..rows].fill(tag as u8);
            children[0] = tags;
            for (index, (_, ty)) in fields.iter().enumerate().skip(1) {
                children[index] = if index == tag + 1 {
                    member.clone()
                } else {
                    Vector::constant_null(ty.clone())
                };
            }
        }
        // NULL inputs keep their tag: the union is valid, its member is NULL.
        if constant {
            out.make_constant();
        }
        *result = out;
        Ok(true)
    }))
}

/// Casts whose source is STRUCT or UNION.
pub(super) fn cast_switch(
    set: &CastFunctionSet,
    source: &LogicalType,
    target: &LogicalType,
) -> Result<Option<BoundCastInfo>> {
    match source.id() {
        LogicalTypeId::Struct => from_struct(set, source, target),
        LogicalTypeId::Union => from_union(set, source, target),
        _ => Ok(None),
    }
}
