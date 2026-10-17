//! `json_structure` and the schema inference of `read_json` (DuckDB's `json_structure.cpp`):
//! a tree of the types seen at each key/array position, merged over many documents and
//! turned into a logical type.

use std::collections::{HashMap, HashSet};

use rustdb_common::{Error, Result};
use rustdb_function::{FunctionState, ScalarFunction, ScalarFunctionSet};
use rustdb_types::{DataChunk, LogicalType, LogicalTypeId, Vector};

use super::input_types;
use crate::json_common::{val_to_string, val_type_to_logical_type_id};
use crate::json_executors::{StrBuilder, unary_execute};
use crate::json_type;
use crate::yyjson::{Doc, MutVal, ValId, ValType, tag};

/// The types observed at one position (`JSONStructureNode`).
#[derive(Clone, Debug, Default)]
pub(crate) struct StructureNode {
    /// The object key of this position (none for array elements and the root).
    pub(crate) key: Option<String>,
    /// Whether the candidate types of a VARCHAR description were initialized.
    pub(crate) initialized: bool,
    pub(crate) descriptions: Vec<StructureDescription>,
    /// How many values were seen here, and how many were `null`.
    pub(crate) count: u64,
    pub(crate) null_count: u64,
}

/// One type observed at a position (`JSONStructureDescription`).
#[derive(Clone, Debug)]
pub(crate) struct StructureDescription {
    pub(crate) ty: LogicalTypeId,
    /// Key -> index into `children` (objects).
    pub(crate) key_map: HashMap<String, usize>,
    /// The element node (arrays) or the key nodes (objects).
    pub(crate) children: Vec<StructureNode>,
    /// Types a VARCHAR may be converted to, tried from the back.
    pub(crate) candidate_types: Vec<LogicalTypeId>,
    /// Whether an unsigned integer above `i64::MAX` was seen.
    pub(crate) has_large_ubigint: bool,
}

impl StructureDescription {
    fn new(ty: LogicalTypeId) -> Self {
        Self {
            ty,
            key_map: HashMap::new(),
            children: Vec::new(),
            candidate_types: Vec::new(),
            has_large_ubigint: false,
        }
    }

    /// The element node of an array description.
    pub(crate) fn get_or_create_element(&mut self) -> &mut StructureNode {
        if self.children.is_empty() {
            self.children.push(StructureNode::default());
        }
        let last = self.children.len() - 1;
        &mut self.children[last]
    }

    /// The node of `key` in an object description.
    pub(crate) fn get_or_create_child(&mut self, key: &str) -> &mut StructureNode {
        let index = match self.key_map.get(key) {
            Some(&index) => index,
            None => {
                self.children.push(StructureNode {
                    key: Some(key.to_owned()),
                    ..StructureNode::default()
                });
                let index = self.children.len() - 1;
                self.key_map.insert(key.to_owned(), index);
                index
            }
        };
        &mut self.children[index]
    }
}

/// `IsNumeric`.
fn is_numeric(ty: LogicalTypeId) -> bool {
    matches!(
        ty,
        LogicalTypeId::Double | LogicalTypeId::UBigInt | LogicalTypeId::BigInt | LogicalTypeId::HugeInt
    )
}

/// `MaxNumericType`.
fn max_numeric_type(a: LogicalTypeId, b: LogicalTypeId) -> LogicalTypeId {
    use LogicalTypeId as L;
    if a == L::Double || b == L::Double {
        return L::Double;
    }
    if a == L::HugeInt || b == L::HugeInt {
        return L::HugeInt;
    }
    if (a == L::BigInt && b == L::UBigInt) || (a == L::UBigInt && b == L::BigInt) {
        return L::HugeInt;
    }
    L::BigInt
}

impl StructureNode {
    /// `GetOrCreateDescription`.
    pub(crate) fn get_or_create_description(&mut self, ty: LogicalTypeId) -> &mut StructureDescription {
        if self.descriptions.is_empty() {
            self.descriptions.push(StructureDescription::new(ty));
            return &mut self.descriptions[0];
        }
        if self.descriptions.len() == 1 && self.descriptions[0].ty == LogicalTypeId::SqlNull {
            self.descriptions[0].ty = ty;
            return &mut self.descriptions[0];
        }
        if ty == LogicalTypeId::SqlNull {
            let last = self.descriptions.len() - 1;
            return &mut self.descriptions[last];
        }
        let numeric = is_numeric(ty);
        let mut found = None;
        for (index, description) in self.descriptions.iter_mut().enumerate() {
            if description.ty == ty {
                found = Some(index);
                break;
            }
            if numeric && is_numeric(description.ty) {
                description.ty = max_numeric_type(ty, description.ty);
                found = Some(index);
                break;
            }
        }
        let index = match found {
            Some(index) => index,
            None => {
                self.descriptions.push(StructureDescription::new(ty));
                self.descriptions.len() - 1
            }
        };
        &mut self.descriptions[index]
    }

    /// `ContainsVarchar`.
    pub(crate) fn contains_varchar(&self) -> bool {
        let [description] = self.descriptions.as_slice() else {
            return false;
        };
        description.ty == LogicalTypeId::Varchar
            || description.children.iter().any(StructureNode::contains_varchar)
    }

    /// `InitializeCandidateTypes`.
    pub(crate) fn initialize_candidate_types(
        &mut self,
        max_depth: u64,
        convert_strings_to_integers: bool,
        depth: u64,
    ) {
        if depth >= max_depth {
            return;
        }
        let initialized = self.initialized;
        let [description] = self.descriptions.as_mut_slice() else {
            return;
        };
        if description.ty == LogicalTypeId::Varchar && !initialized {
            description.candidate_types = if convert_strings_to_integers {
                vec![
                    LogicalTypeId::Uuid,
                    LogicalTypeId::BigInt,
                    LogicalTypeId::Timestamp,
                    LogicalTypeId::Date,
                    LogicalTypeId::Time,
                ]
            } else {
                vec![
                    LogicalTypeId::Uuid,
                    LogicalTypeId::Timestamp,
                    LogicalTypeId::Date,
                    LogicalTypeId::Time,
                ]
            };
            self.initialized = true;
        } else {
            for child in &mut description.children {
                child.initialize_candidate_types(max_depth, convert_strings_to_integers, depth + 1);
            }
        }
    }
}

/// `JSONStructure::ExtractStructure`: adds the structure of value `val` to `node`.
pub(crate) fn extract_structure(
    doc: &Doc<'_>,
    val: ValId,
    node: &mut StructureNode,
    ignore_errors: bool,
) -> Result<()> {
    node.count += 1;
    let value_tag = doc.tag(val);
    if value_tag == tag::NULL {
        node.null_count += 1;
    }
    match doc.get_type(val) {
        ValType::Arr => {
            let description = node.get_or_create_description(LogicalTypeId::List);
            let child = description.get_or_create_element();
            for element in doc.arr_iter(val) {
                extract_structure(doc, element, child, ignore_errors)?;
            }
        }
        ValType::Obj => {
            let description = node.get_or_create_description(LogicalTypeId::Struct);
            let mut keys: HashSet<&[u8]> = HashSet::new();
            let mut ci_keys: HashMap<String, String> = HashMap::new();
            for (key, value) in doc.obj_iter(val) {
                let key_bytes = doc.str(key);
                let key_text = String::from_utf8_lossy(key_bytes).into_owned();
                if !keys.insert(key_bytes) && !ignore_errors {
                    return Err(Error::invalid_input(format!(
                        "Duplicate key \"{key_text}\" in object {}",
                        val_to_string(doc, val, None)
                    )));
                }
                let lower = key_text.to_ascii_lowercase();
                if let Some(existing) = ci_keys.get(&lower) {
                    if !ignore_errors {
                        return Err(Error::invalid_input(format!(
                            "Duplicate key (different case) \"{key_text}\" and \"{existing}\" in object {}",
                            val_to_string(doc, val, None)
                        )));
                    }
                } else {
                    ci_keys.insert(lower, key_text.clone());
                }
                let child = description.get_or_create_child(&key_text);
                extract_structure(doc, value, child, ignore_errors)?;
            }
        }
        _ => {
            let ty = val_type_to_logical_type_id(doc, val);
            let description = node.get_or_create_description(ty);
            if ty == LogicalTypeId::UBigInt && doc.get_uint(val) > i64::MAX as u64 {
                description.has_large_ubigint = true;
            }
        }
    }
    Ok(())
}

/// The name of a type id as `EnumUtil::ToChars<LogicalTypeId>` writes it.
pub(crate) fn type_id_name(ty: LogicalTypeId) -> &'static str {
    use LogicalTypeId as L;
    match ty {
        L::SqlNull => "NULL",
        L::Boolean => "BOOLEAN",
        L::BigInt => "BIGINT",
        L::UBigInt => "UBIGINT",
        L::HugeInt => "HUGEINT",
        L::Double => "DOUBLE",
        L::Varchar => "VARCHAR",
        L::Uuid => "UUID",
        L::Timestamp => "TIMESTAMP",
        L::Date => "DATE",
        L::Time => "TIME",
        L::List => "LIST",
        L::Struct => "STRUCT",
        _ => "INVALID",
    }
}

/// `ConvertStructure`: the structure as a JSON value of type names.
fn convert_structure(node: &StructureNode) -> MutVal {
    let [description] = node.descriptions.as_slice() else {
        if node.descriptions.is_empty() {
            return MutVal::Str(b"NULL".to_vec());
        }
        return MutVal::Str(b"JSON".to_vec());
    };
    match description.ty {
        LogicalTypeId::List => MutVal::Arr(
            description
                .children
                .iter()
                .take(1)
                .map(convert_structure)
                .collect(),
        ),
        LogicalTypeId::Struct => {
            if description.children.is_empty() {
                return MutVal::Str(b"JSON".to_vec());
            }
            MutVal::Obj(
                description
                    .children
                    .iter()
                    .map(|child| {
                        (
                            child.key.clone().unwrap_or_default().into_bytes(),
                            convert_structure(child),
                        )
                    })
                    .collect(),
            )
        }
        ty => MutVal::Str(type_id_name(ty).as_bytes().to_vec()),
    }
}

fn structure_of(doc: &Doc<'_>, val: ValId, out: &mut StrBuilder) -> Result<()> {
    let mut node = StructureNode::default();
    extract_structure(doc, val, &mut node, true)?;
    let structure = convert_structure(&node);
    out.push_with(|buf| structure.write(buf));
    Ok(())
}

fn structure(args: &DataChunk, state: &mut FunctionState, result: &mut Vector) -> Result<()> {
    unary_execute(args, state, result, &mut structure_of)
}

/// `JSONFunctions::GetStructureFunction`.
pub(crate) fn structure_function() -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new("json_structure");
    for input in input_types() {
        set.add(ScalarFunction::new(
            "json_structure",
            vec![input],
            json_type(),
            structure,
        ));
    }
    set
}

/// `MergeNodeVal`.
fn merge_node_val(merged: &mut StructureNode, child: &StructureDescription, node_initialized: bool) {
    let merged_desc = merged.get_or_create_description(child.ty);
    if child.has_large_ubigint {
        merged_desc.has_large_ubigint = true;
    }
    let merged_ty = merged_desc.ty;
    if merged_ty != LogicalTypeId::Varchar || !node_initialized || merged.descriptions.len() != 1 {
        return;
    }
    let was_initialized = merged.initialized;
    let merged_desc = &mut merged.descriptions[0];
    if !was_initialized {
        merged_desc.candidate_types.clone_from(&child.candidate_types);
    } else if merged_desc.candidate_types.is_empty() != child.candidate_types.is_empty()
        || (!merged_desc.candidate_types.is_empty()
            && merged_desc.candidate_types.last() != child.candidate_types.last())
    {
        merged_desc.candidate_types.clear();
    }
    merged.initialized = true;
}

/// `JSONStructure::MergeNodes`.
pub(crate) fn merge_nodes(merged: &mut StructureNode, node: &StructureNode) {
    merged.count += node.count;
    merged.null_count += node.null_count;
    for child_desc in &node.descriptions {
        match child_desc.ty {
            LogicalTypeId::List => {
                let merged_desc = merged.get_or_create_description(LogicalTypeId::List);
                let merged_child = merged_desc.get_or_create_element();
                for list_child in &child_desc.children {
                    merge_nodes(merged_child, list_child);
                }
            }
            LogicalTypeId::Struct => {
                let merged_desc = merged.get_or_create_description(LogicalTypeId::Struct);
                for struct_child in &child_desc.children {
                    let key = struct_child.key.clone().unwrap_or_default();
                    let merged_child = merged_desc.get_or_create_child(&key);
                    merge_nodes(merged_child, struct_child);
                }
            }
            _ => merge_node_val(merged, child_desc, node.initialized),
        }
    }
}

/// The options of type inference (`StructureToType`'s parameters).
#[derive(Clone, Copy, Debug)]
pub(crate) struct InferenceOptions {
    pub(crate) max_depth: u64,
    pub(crate) field_appearance_threshold: f64,
    pub(crate) map_inference_threshold: u64,
}

/// `CalculateMapAndStructSimilarity`.
fn map_and_struct_similarity(
    map_type: &LogicalType,
    struct_type: &LogicalType,
    swapped: bool,
    max_depth: u64,
    depth: u64,
) -> f64 {
    let Some((_, map_value)) = map_type.map_key_value() else {
        return -1.0;
    };
    let children = struct_type.struct_children().unwrap_or_default();
    let mut total = 0.0;
    for (_, child) in children {
        let similarity = if swapped {
            type_similarity(child, map_value, max_depth, depth + 1)
        } else {
            type_similarity(map_value, child, max_depth, depth + 1)
        };
        if similarity < 0.0 {
            return similarity;
        }
        total += similarity;
    }
    total / children.len() as f64
}

/// `CalculateTypeSimilarity`.
fn type_similarity(merged: &LogicalType, ty: &LogicalType, max_depth: u64, depth: u64) -> f64 {
    if depth >= max_depth || merged.id() == LogicalTypeId::SqlNull || ty.id() == LogicalTypeId::SqlNull {
        return 1.0;
    }
    if crate::json_functions::is_json_type(merged) {
        return -1.0;
    }
    if crate::json_functions::is_json_type(ty) || merged == ty {
        return 1.0;
    }
    match merged.id() {
        LogicalTypeId::Struct => {
            if ty.id() == LogicalTypeId::Map {
                return map_and_struct_similarity(ty, merged, true, max_depth, depth);
            }
            if ty.id() != LogicalTypeId::Struct {
                return -1.0;
            }
            let merged_children = merged.struct_children().unwrap_or_default();
            let lookup: HashMap<&str, &LogicalType> = merged_children
                .iter()
                .map(|(name, child)| (name.as_str(), child))
                .collect();
            let mut total = 0.0;
            for (name, child) in ty.struct_children().unwrap_or_default() {
                let Some(merged_child) = lookup.get(name.as_str()) else {
                    return -1.0;
                };
                let similarity = type_similarity(merged_child, child, max_depth, depth + 1);
                if similarity < 0.0 {
                    return similarity;
                }
                total += similarity;
            }
            total / merged_children.len() as f64
        }
        LogicalTypeId::Map => {
            if ty.id() == LogicalTypeId::Map {
                return match (merged.map_key_value(), ty.map_key_value()) {
                    (Some((_, merged_value)), Some((_, value))) => {
                        type_similarity(merged_value, value, max_depth, depth + 1)
                    }
                    _ => -1.0,
                };
            }
            if ty.id() != LogicalTypeId::Struct {
                return -1.0;
            }
            map_and_struct_similarity(merged, ty, false, max_depth, depth)
        }
        LogicalTypeId::List => {
            if ty.id() != LogicalTypeId::List {
                return -1.0;
            }
            match (merged.child_type(), ty.child_type()) {
                (Some(merged_child), Some(child)) => {
                    type_similarity(merged_child, child, max_depth, depth + 1)
                }
                _ => -1.0,
            }
        }
        _ => 1.0,
    }
}

/// `IsStructureInconsistent`.
fn is_structure_inconsistent(
    desc: &StructureDescription,
    sample_count: u64,
    null_count: u64,
    threshold: f64,
) -> bool {
    let denominator = sample_count.saturating_sub(null_count) as f64;
    let total: f64 = desc
        .children
        .iter()
        .map(|child| child.count as f64 / denominator)
        .sum();
    total / (desc.children.len() as f64) < threshold
}

/// `GetMergedType`: the type of all children of an object merged into one.
fn merged_type(
    node: &StructureNode,
    options: InferenceOptions,
    depth: u64,
    null_type: &LogicalType,
) -> LogicalType {
    let mut merged = StructureNode::default();
    if let Some(desc) = node.descriptions.first() {
        for child in &desc.children {
            merge_nodes(&mut merged, child);
        }
    }
    structure_to_type(&merged, options, depth + 1, null_type)
}

/// `StructureToTypeObject`.
fn structure_to_type_object(
    node: &StructureNode,
    options: InferenceOptions,
    depth: u64,
    null_type: &LogicalType,
) -> LogicalType {
    let desc = &node.descriptions[0];
    let map_inference = options.map_inference_threshold != u64::MAX;
    if desc.children.is_empty() {
        if map_inference {
            return LogicalType::map(LogicalType::VARCHAR, null_type.clone());
        }
        return json_type();
    }
    if map_inference
        && is_structure_inconsistent(
            desc,
            node.count,
            node.null_count,
            options.field_appearance_threshold,
        )
    {
        return LogicalType::map(
            LogicalType::VARCHAR,
            merged_type(node, options, depth + 1, null_type),
        );
    }
    let child_types: Vec<(String, LogicalType)> = desc
        .children
        .iter()
        .map(|child| {
            (
                child.key.clone().unwrap_or_default(),
                structure_to_type(child, options, depth + 1, null_type),
            )
        })
        .collect();
    if desc.children.len() as u64 >= options.map_inference_threshold {
        let mut map_value_type = merged_type(node, options, depth + 1, &LogicalType::SQLNULL);
        let mut total = 0.0;
        for (_, child_type) in &child_types {
            let similarity = type_similarity(&map_value_type, child_type, options.max_depth, depth + 1);
            if similarity < 0.0 {
                total = similarity;
                break;
            }
            total += similarity;
        }
        if total / child_types.len() as f64 >= 0.8 {
            if null_type.id() != LogicalTypeId::SqlNull {
                map_value_type = merged_type(node, options, depth + 1, null_type);
            }
            return LogicalType::map(LogicalType::VARCHAR, map_value_type);
        }
    }
    LogicalType::struct_type(child_types)
}

/// The logical type of a candidate type id.
pub(crate) fn candidate_logical_type(ty: LogicalTypeId) -> LogicalType {
    match ty {
        LogicalTypeId::Uuid => LogicalType::UUID,
        LogicalTypeId::BigInt => LogicalType::BIGINT,
        LogicalTypeId::Timestamp => LogicalType::TIMESTAMP,
        LogicalTypeId::Date => LogicalType::DATE,
        LogicalTypeId::Time => LogicalType::TIME,
        _ => LogicalType::VARCHAR,
    }
}

/// `JSONStructure::StructureToType`.
pub(crate) fn structure_to_type(
    node: &StructureNode,
    options: InferenceOptions,
    depth: u64,
    null_type: &LogicalType,
) -> LogicalType {
    if depth >= options.max_depth {
        return json_type();
    }
    let [desc] = node.descriptions.as_slice() else {
        if node.descriptions.is_empty() {
            return null_type.clone();
        }
        return json_type();
    };
    match desc.ty {
        LogicalTypeId::List => {
            let child = desc.children.first().map_or_else(
                || null_type.clone(),
                |child| structure_to_type(child, options, depth + 1, null_type),
            );
            LogicalType::list(child)
        }
        LogicalTypeId::Struct => structure_to_type_object(node, options, depth, null_type),
        LogicalTypeId::Varchar => desc
            .candidate_types
            .last()
            .map_or(LogicalType::VARCHAR, |&ty| candidate_logical_type(ty)),
        LogicalTypeId::UBigInt => {
            if desc.has_large_ubigint {
                LogicalType::HUGEINT
            } else {
                LogicalType::BIGINT
            }
        }
        LogicalTypeId::SqlNull => null_type.clone(),
        LogicalTypeId::Boolean => LogicalType::BOOLEAN,
        LogicalTypeId::BigInt => LogicalType::BIGINT,
        LogicalTypeId::HugeInt => LogicalType::HUGEINT,
        LogicalTypeId::Double => LogicalType::DOUBLE,
        _ => json_type(),
    }
}
