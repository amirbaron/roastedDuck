//! Parsing of the C++ type strings used in specs (`vector<unique_ptr<ParsedExpression>>`).

use crate::Result;

/// A spec member type, reduced to what matters for serialization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CppType {
    Bool,
    U8,
    I8,
    U16,
    I16,
    U32,
    I32,
    U64,
    I64,
    F32,
    F64,
    /// `hugeint_t`
    I128,
    /// `uhugeint_t`
    U128,
    String,
    /// `optional_idx`
    OptionalIdx,
    /// A class, enum or external type.
    Named(String),
    /// `T*`, `unique_ptr<T>`, `shared_ptr<T>`: a nullable value.
    Pointer(Box<CppType>),
    /// `vector<T>` and unordered sets: a list.
    Vector(Box<CppType>),
    /// `set<T>`: a list in sorted order.
    Set(Box<CppType>),
    /// `case_insensitive_map_t<V>`, `InsertionOrderPreservingMap<V>`: a list of `{key, value}`.
    CaseInsensitiveMap(Box<CppType>),
    /// `case_insensitive_set_t`: a list of strings.
    CaseInsensitiveSet,
    /// `unordered_map`, `map`: a list of `{key, value}`.
    Map(Box<CppType>, Box<CppType>),
    /// `pair<A, B>`: `{first, second}`.
    Pair(Box<CppType>, Box<CppType>),
    /// `IndexVector<T, INDEX_TYPE>`: `{internal_vector}`.
    IndexVector(Box<CppType>),
}

/// Splits `a, b<c, d>` at top-level commas.
fn split_arguments(text: &str) -> Vec<&str> {
    let mut depth = 0usize;
    let mut start = 0;
    let mut parts = Vec::new();
    for (i, c) in text.char_indices() {
        match c {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                parts.push(text[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(text[start..].trim());
    parts
}

impl CppType {
    pub fn parse(text: &str) -> Result<Self> {
        let text = text.trim();
        let text = text.strip_prefix("duckdb::").unwrap_or(text);
        let text = text.strip_prefix("std::").unwrap_or(text);
        if let Some(inner) = text.strip_suffix('*') {
            return Ok(Self::Pointer(Box::new(Self::parse(inner)?)));
        }
        if let Some((name, rest)) = text.split_once('<') {
            let inner = rest
                .strip_suffix('>')
                .ok_or_else(|| format!("malformed type '{text}'"))?;
            let args = split_arguments(inner);
            let arg = |i: usize| -> Result<Box<Self>> {
                let text = args
                    .get(i)
                    .ok_or_else(|| format!("missing type argument in '{text}'"))?;
                Ok(Box::new(Self::parse(text)?))
            };
            return Ok(match name {
                "unique_ptr" | "shared_ptr" | "optionally_owned_ptr" => Self::Pointer(arg(0)?),
                "vector" | "unsafe_vector" | "unordered_set" => Self::Vector(arg(0)?),
                "IndexVector" => Self::IndexVector(arg(0)?),
                "set" => Self::Set(arg(0)?),
                // Keys of DuckDB's insertion-ordered map are case-insensitive too.
                "case_insensitive_map_t" | "InsertionOrderPreservingMap" => Self::CaseInsensitiveMap(arg(0)?),
                "unordered_map" | "map" => Self::Map(arg(0)?, arg(1)?),
                "qualified_column_map_t" => {
                    Self::Map(Box::new(Self::Named("QualifiedColumnName".into())), arg(0)?)
                }
                "child_list_t" => Self::Vector(Box::new(Self::Pair(Box::new(Self::String), arg(0)?))),
                "pair" => Self::Pair(arg(0)?, arg(1)?),
                _ => return Err(format!("unsupported template type '{text}'").into()),
            });
        }
        Ok(match text {
            "bool" => Self::Bool,
            "uint8_t" => Self::U8,
            "int8_t" => Self::I8,
            "uint16_t" => Self::U16,
            "int16_t" => Self::I16,
            "uint32_t" => Self::U32,
            "int32_t" | "int" => Self::I32,
            "uint64_t" | "idx_t" | "column_t" | "size_t" => Self::U64,
            "int64_t" => Self::I64,
            "float" => Self::F32,
            "double" => Self::F64,
            "hugeint_t" => Self::I128,
            "uhugeint_t" => Self::U128,
            "string" => Self::String,
            "optional_idx" => Self::OptionalIdx,
            "case_insensitive_set_t" => Self::CaseInsensitiveSet,
            "qualified_column_set_t" => Self::Vector(Box::new(Self::Named("QualifiedColumnName".into()))),
            "create_info_set_t" => Self::Vector(Box::new(Self::Named("LogicalDependency".into()))),
            "GroupingSet" => Self::Set(Box::new(Self::U64)),
            name => Self::Named(name.to_owned()),
        })
    }

    /// Named types mentioned anywhere in this type.
    pub fn named_types(&self, out: &mut Vec<String>) {
        match self {
            Self::Named(name) => out.push(name.clone()),
            Self::Pointer(inner)
            | Self::Vector(inner)
            | Self::Set(inner)
            | Self::CaseInsensitiveMap(inner)
            | Self::IndexVector(inner) => {
                inner.named_types(out);
            }
            Self::Map(a, b) | Self::Pair(a, b) => {
                a.named_types(out);
                b.named_types(out);
            }
            _ => {}
        }
    }
}

/// `has_default_by_default` of `scripts/generate_serialization.py`, applied to the raw
/// spec type: whether `WritePropertyWithDefault` is used without an explicit default.
pub fn has_default_by_default(raw: &str) -> bool {
    let is_pointer = raw.ends_with('*') || raw.starts_with("shared_ptr<");
    let is_container = raw.contains('<') && !raw.contains("CSVOption");
    if is_pointer {
        return true;
    }
    if is_container {
        return !raw.contains("IndexVector");
    }
    matches!(
        raw,
        "string"
            | "bool"
            | "int8_t"
            | "int16_t"
            | "int32_t"
            | "int64_t"
            | "uint8_t"
            | "uint16_t"
            | "uint32_t"
            | "uint64_t"
            | "idx_t"
            | "size_t"
            | "int"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nested_types() {
        let ty = CppType::parse("vector<unique_ptr<ParsedExpression>>").unwrap();
        assert_eq!(
            ty,
            CppType::Vector(Box::new(CppType::Pointer(Box::new(CppType::Named(
                "ParsedExpression".into()
            )))))
        );
        let ty = CppType::parse("unordered_map<string, Value>").unwrap();
        assert_eq!(
            ty,
            CppType::Map(
                Box::new(CppType::String),
                Box::new(CppType::Named("Value".into()))
            )
        );
        assert!(has_default_by_default("case_insensitive_map_t<idx_t>"));
        assert!(!has_default_by_default("case_insensitive_set_t"));
        assert!(!has_default_by_default("double"));
    }
}
