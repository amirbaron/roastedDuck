//! Keeping a table's dependencies in step with its definition when it is altered
//! (DuckDB re-binds the altered table, which records its dependencies anew).

use rustdb_ast::visit::{VisitorMut, walk_expression};
use rustdb_ast::{
    CatalogEntryInfo, CatalogType as AstCatalogType, LogicalDependency, ParsedExpression,
    ParsedExpressionKind,
};
use rustdb_common::ByteString;
use rustdb_types::{LogicalType, LogicalTypeId};

use crate::TableEntry;

/// The sequences (`nextval('name')` / `currval('name')` with a constant name) and the
/// functions an expression calls.
#[derive(Default)]
struct References {
    sequences: Vec<(ByteString, ByteString)>,
    functions: Vec<ByteString>,
}

fn unquote(mut name: &[u8]) -> ByteString {
    while name.first() == Some(&b'"') {
        name = &name[1..];
    }
    while name.last() == Some(&b'"') {
        name = &name[..name.len() - 1];
    }
    name.into()
}

impl VisitorMut for References {
    fn visit_expression(&mut self, expression: &mut ParsedExpression) {
        if let ParsedExpressionKind::Function(function) = &expression.kind {
            let name = function.function_name.to_ascii_lowercase();
            if (name == "nextval" || name == "currval")
                && let Some(ParsedExpressionKind::Constant(constant)) =
                    function.children.first().map(|c| &c.kind)
                && let Some(text) = constant.value.as_bytes()
            {
                let mut parts: Vec<&[u8]> = text.split(|byte| *byte == b'.').collect();
                let sequence = unquote(parts.pop().unwrap_or_default());
                let schema = unquote(parts.pop().unwrap_or_default());
                self.sequences.push((schema, sequence));
            }
            self.functions.push(name);
        }
        walk_expression(self, expression);
    }
}

/// Whether `ty` (or a type nested in it) is the user type `name`.
fn uses_type(ty: &LogicalType, name: &[u8]) -> bool {
    if ty.alias().is_some_and(|alias| alias.eq_ignore_ascii_case(name)) {
        return true;
    }
    match ty.id() {
        LogicalTypeId::List => ty.child_type().is_some_and(|child| uses_type(child, name)),
        LogicalTypeId::Array => ty.array_child().is_some_and(|(child, _)| uses_type(child, name)),
        LogicalTypeId::Map => ty
            .map_key_value()
            .is_some_and(|(key, value)| uses_type(key, name) || uses_type(value, name)),
        LogicalTypeId::Struct => ty
            .struct_children()
            .is_some_and(|children| children.iter().any(|(_, child)| uses_type(child, name))),
        LogicalTypeId::Union => ty
            .union_members()
            .is_some_and(|members| members.iter().any(|(_, member)| uses_type(member, name))),
        _ => false,
    }
}

/// The dependencies of an altered table: those its new definition still refers to, plus
/// sequences its new defaults name.
pub fn altered_table_dependencies(
    table: &TableEntry,
    previous: &[LogicalDependency],
    default_schema: impl AsRef<[u8]>,
) -> Vec<LogicalDependency> {
    let default_schema = default_schema.as_ref();
    let mut references = References::default();
    for column in &table.columns {
        for expression in [&column.default_value, &column.generated].into_iter().flatten() {
            let mut expression = expression.clone();
            references.visit_expression(&mut expression);
        }
    }
    let mut result: Vec<LogicalDependency> = previous
        .iter()
        .filter(|dependency| {
            let name = &dependency.entry.name;
            match dependency.entry.catalog_type {
                AstCatalogType::SequenceEntry => references
                    .sequences
                    .iter()
                    .any(|(_, sequence)| sequence.eq_ignore_ascii_case(name)),
                AstCatalogType::MacroEntry
                | AstCatalogType::TableMacroEntry
                | AstCatalogType::ScalarFunctionEntry => references
                    .functions
                    .iter()
                    .any(|function| function.eq_ignore_ascii_case(name)),
                AstCatalogType::TypeEntry => table
                    .columns
                    .iter()
                    .any(|column| uses_type(&column.logical_type, name.as_bytes())),
                _ => true,
            }
        })
        .cloned()
        .collect();
    for (schema, sequence) in references.sequences {
        let known = result.iter().any(|dependency| {
            dependency.entry.catalog_type == AstCatalogType::SequenceEntry
                && dependency.entry.name.eq_ignore_ascii_case(&sequence)
        });
        if !known {
            result.push(LogicalDependency {
                entry: CatalogEntryInfo {
                    catalog_type: AstCatalogType::SequenceEntry,
                    schema: if schema.is_empty() {
                        ByteString::from(default_schema)
                    } else {
                        schema
                    },
                    name: sequence,
                },
                catalog: ByteString::new(),
            });
        }
    }
    result
}
