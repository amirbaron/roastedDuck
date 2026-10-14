//! C++ → Rust naming.

/// Splits a C++ identifier into words: `RecursiveCTENode` → `Recursive`, `CTE`, `Node`.
fn words(name: &str) -> Vec<String> {
    let chars: Vec<char> = name.chars().collect();
    let mut words = Vec::new();
    let mut current = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if c == '_' {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
            continue;
        }
        let boundary = c.is_ascii_uppercase()
            && !current.is_empty()
            && (chars[i - 1].is_ascii_lowercase()
                || chars[i - 1].is_ascii_digit()
                || chars.get(i + 1).is_some_and(char::is_ascii_lowercase));
        if boundary {
            words.push(std::mem::take(&mut current));
        }
        current.push(c);
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

fn capitalize(word: &str) -> String {
    let lower = word.to_ascii_lowercase();
    let mut chars = lower.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_ascii_uppercase().to_string() + chars.as_str()
    })
}

/// A C++ type name in Rust style: acronyms become words (`CTEMaterialize` → `CteMaterialize`).
pub fn rust_type_name(name: &str) -> String {
    words(name).iter().map(|w| capitalize(w)).collect()
}

/// A C++ `SCREAMING_CASE` enum member as a Rust variant (`COLUMN_REF` → `ColumnRef`).
pub fn camel_case(name: &str) -> String {
    name.split('_')
        .filter(|w| !w.is_empty())
        .map(capitalize)
        .collect()
}

/// A type name as a snake-case identifier (`ExpressionClass` → `expression_class`).
pub fn snake_case(name: &str) -> String {
    words(name)
        .iter()
        .map(|w| w.to_ascii_lowercase())
        .collect::<Vec<_>>()
        .join("_")
}

/// Whether `name` is a Rust keyword and cannot be used as a field name.
pub fn is_keyword(name: &str) -> bool {
    matches!(
        name,
        "as" | "async"
            | "await"
            | "break"
            | "const"
            | "continue"
            | "crate"
            | "dyn"
            | "else"
            | "enum"
            | "extern"
            | "false"
            | "fn"
            | "for"
            | "gen"
            | "if"
            | "impl"
            | "in"
            | "let"
            | "loop"
            | "match"
            | "mod"
            | "move"
            | "mut"
            | "pub"
            | "ref"
            | "return"
            | "self"
            | "static"
            | "struct"
            | "super"
            | "trait"
            | "true"
            | "type"
            | "unsafe"
            | "use"
            | "where"
            | "while"
            | "yield"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_names() {
        assert_eq!(rust_type_name("RecursiveCTENode"), "RecursiveCteNode");
        assert_eq!(rust_type_name("CTEMaterialize"), "CteMaterialize");
        assert_eq!(rust_type_name("ExpressionClass"), "ExpressionClass");
        assert_eq!(rust_type_name("SQLStatement"), "SqlStatement");
        assert_eq!(camel_case("COLUMN_REF"), "ColumnRef");
        assert_eq!(camel_case("TIMESTAMP_TZ"), "TimestampTz");
        assert_eq!(snake_case("TableReferenceType"), "table_reference_type");
        assert_eq!(snake_case("CTEMaterialize"), "cte_materialize");
    }
}
