//! The descriptions of the built-in functions (DuckDB's `StaticFunctionDefinition`
//! entries of the function lists with `FillFunctionDescriptions`): parameter names,
//! description, examples and categories of each variant, as `duckdb_functions()` lists
//! them, and which function a name is an alias of.

mod definitions;

use definitions::FUNCTION_DEFINITIONS;
use roastedduck_types::{LogicalType, LogicalTypeId};

/// One entry of a function list: the strings of its function struct, with variants
/// separated by `\u{1}` and examples of a variant by `\u{2}`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct StaticFunctionDefinition {
    pub(crate) name: &'static str,
    pub(crate) alias_of: &'static str,
    pub(crate) parameters: &'static str,
    pub(crate) description: &'static str,
    pub(crate) example: &'static str,
    pub(crate) categories: &'static str,
}

/// The description of one variant of a function (DuckDB's `FunctionDescription`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FunctionDescription {
    pub parameter_names: Vec<String>,
    /// `ANY` for parameters without a type.
    pub parameter_types: Vec<LogicalType>,
    pub description: String,
    pub examples: Vec<String>,
    pub categories: Vec<String>,
}

/// What the function list records about a built-in function.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FunctionInfo {
    /// The function the name is an alias of (the name itself for a function).
    pub alias_of: String,
    pub descriptions: Vec<FunctionDescription>,
}

/// `StringUtil::Split` on a character: no empty parts.
fn split(text: &str, separator: char) -> Vec<String> {
    text.split(separator)
        .filter(|part| !part.is_empty())
        .map(str::to_owned)
        .collect()
}

/// `StringUtil::SplitWithParentheses`: splits on `separator` outside parentheses.
fn split_with_parentheses(text: &str, separator: char) -> Vec<String> {
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut current = String::new();
    for c in text.chars() {
        match c {
            '(' => {
                depth += 1;
                current.push(c);
            }
            ')' => {
                depth = depth.saturating_sub(1);
                current.push(c);
            }
            c if c == separator && depth == 0 => {
                if !current.is_empty() {
                    parts.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts
}

/// `DBConfig::ParseLogicalType`: `T[]`, `T[n]`, `MAP(K, V)`, `UNION(...)`, `STRUCT(...)`
/// and the names of built-in types (`None` for an unrecognized name).
pub fn parse_logical_type(text: &str) -> Option<LogicalType> {
    if let Some(child) = text.strip_suffix("[]") {
        return Some(LogicalType::list(parse_logical_type(child)?));
    }
    if text.ends_with(']') {
        let open = text.rfind('[').filter(|&open| open > 0)?;
        let size: u32 = text[open + 1..text.len() - 1].parse().ok()?;
        return Some(LogicalType::array(parse_logical_type(&text[..open])?, size));
    }
    let members = |inner: &str| -> Option<Vec<(roastedduck_common::ByteString, LogicalType)>> {
        split_with_parentheses(inner, ',')
            .iter()
            .map(|member| {
                let parts = split_with_parentheses(member.trim(), ' ');
                match parts.as_slice() {
                    [name, ty] => Some((name.trim().into(), parse_logical_type(ty.trim())?)),
                    _ => None,
                }
            })
            .collect()
    };
    if let Some(inner) = text.strip_prefix("MAP(").and_then(|rest| rest.strip_suffix(')')) {
        let arguments = split_with_parentheses(inner, ',');
        let [key, value] = arguments.as_slice() else {
            return None;
        };
        return Some(LogicalType::map(
            parse_logical_type(key.trim())?,
            parse_logical_type(value.trim())?,
        ));
    }
    if let Some(inner) = text
        .strip_prefix("UNION(")
        .and_then(|rest| rest.strip_suffix(')'))
    {
        return Some(LogicalType::union_type(members(inner)?));
    }
    if let Some(inner) = text
        .strip_prefix("STRUCT(")
        .and_then(|rest| rest.strip_suffix(')'))
    {
        return Some(LogicalType::struct_type(members(inner)?));
    }
    if text.eq_ignore_ascii_case("ANY") {
        return Some(LogicalType::ANY);
    }
    roastedduck_types::builtin_type_id(text).map(LogicalType::simple)
}

/// `GetExamplesForFunctionAlias`: the examples that use the function's own name (or
/// those of the aliased function, renamed), else all of them.
fn examples_for_alias(name: &str, alias_of: &str, all: Vec<String>) -> Vec<String> {
    let is_operator = |name: &str| name.chars().next().is_some_and(|c| !c.is_ascii_alphabetic());
    let operator = is_operator(name);
    let filtered: Vec<String> = all
        .iter()
        .filter(|example| example.starts_with(name) || (operator && example.contains(name)))
        .cloned()
        .collect();
    if !filtered.is_empty() {
        return filtered;
    }
    if !alias_of.is_empty() && !is_operator(alias_of) && !operator {
        let renamed: Vec<String> = all
            .iter()
            .filter_map(|example| example.strip_prefix(alias_of).map(|rest| format!("{name}{rest}")))
            .collect();
        if !renamed.is_empty() {
            return renamed;
        }
    }
    all
}

/// `FillFunctionDescriptions` for one function list entry.
fn fill_function_descriptions(definition: &StaticFunctionDefinition) -> Vec<FunctionDescription> {
    let mut variants = split(definition.parameters, '\u{1}');
    let descriptions = split(definition.description, '\u{1}');
    let examples = split(definition.example, '\u{1}');
    let categories = split(definition.categories, '\u{1}');
    if variants.is_empty() {
        variants.push(String::new());
    }
    let count = variants.len();
    variants
        .iter()
        .enumerate()
        .map(|(index, variant)| {
            let mut description = FunctionDescription::default();
            for parameter in split_with_parentheses(variant, ',') {
                let parts: Vec<&str> = parameter.split("::").collect();
                match parts.as_slice() {
                    [name] => {
                        description.parameter_names.push((*name).to_owned());
                        description.parameter_types.push(LogicalType::ANY);
                    }
                    [name, ty] => {
                        description.parameter_names.push((*name).to_owned());
                        description
                            .parameter_types
                            .push(parse_logical_type(ty).unwrap_or(LogicalType::ANY));
                    }
                    _ => {}
                }
            }
            if descriptions.len() == count {
                description.description.clone_from(&descriptions[index]);
            } else if descriptions.len() == 1 {
                description.description.clone_from(&descriptions[0]);
            }
            let variant_examples = if examples.len() == count {
                split(&examples[index], '\u{2}')
            } else if examples.len() == 1 {
                split(&examples[0], '\u{2}')
            } else {
                Vec::new()
            };
            description.examples = examples_for_alias(definition.name, definition.alias_of, variant_examples);
            if index < categories.len() {
                description.categories = split(&categories[index], ',');
            } else if categories.len() == 1 {
                description.categories = split(&categories[0], ',');
            }
            description
        })
        .collect()
}

/// The function list entry of built-in function `name`, if it has one.
pub fn function_info(name: impl AsRef<[u8]>) -> Option<FunctionInfo> {
    FUNCTION_DEFINITIONS
        .iter()
        .find(|definition| definition.name.as_bytes() == name.as_ref())
        .map(|definition| FunctionInfo {
            alias_of: definition.alias_of.to_owned(),
            descriptions: fill_function_descriptions(definition),
        })
}

/// `CalcDescriptionSpecificity`: the number of `ANY` parameters a matching description
/// needs (`None`: no match).
fn description_specificity(
    description: &FunctionDescription,
    parameter_types: &[LogicalType],
) -> Option<usize> {
    if description.parameter_types.len() != parameter_types.len() {
        return None;
    }
    let mut any_count = 0;
    for (described, actual) in description.parameter_types.iter().zip(parameter_types) {
        if described.id() == LogicalTypeId::Any {
            any_count += 1;
        } else if described != actual {
            return None;
        }
    }
    Some(any_count)
}

/// `GetFunctionDescriptionIndex`: the description of the overload with
/// `parameter_types` (a single description applies unless a typed parameter differs;
/// otherwise the most specific matching one).
pub fn description_index(
    descriptions: &[FunctionDescription],
    parameter_types: &[LogicalType],
) -> Option<usize> {
    if let [only] = descriptions {
        let mismatch = only
            .parameter_types
            .iter()
            .zip(parameter_types)
            .any(|(described, actual)| described.id() != LogicalTypeId::Any && described != actual);
        return (!mismatch).then_some(0);
    }
    let mut best: Option<(usize, usize)> = None;
    for (index, description) in descriptions.iter().enumerate() {
        if let Some(score) = description_specificity(description, parameter_types)
            && best.is_none_or(|(_, best_score)| score < best_score)
        {
            best = Some((index, score));
        }
    }
    best.map(|(index, _)| index)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describes_functions() {
        let corr = function_info("corr").unwrap();
        assert_eq!(corr.alias_of, "corr");
        assert_eq!(corr.descriptions[0].parameter_names, vec!["y", "x"]);
        let mean = function_info("mean").unwrap();
        assert_eq!(mean.alias_of, "avg");
        assert_eq!(
            parse_logical_type("MAP(ANY,ANY)"),
            Some(LogicalType::map(LogicalType::ANY, LogicalType::ANY))
        );
        assert_eq!(
            parse_logical_type("VARCHAR[]"),
            Some(LogicalType::list(LogicalType::VARCHAR))
        );
    }
}
