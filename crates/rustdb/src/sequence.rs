//! `nextval` and `currval` (DuckDB's `function/scalar/sequence/nextval.cpp`): the
//! sequence is looked up when the call is bound, through the binder's client context.

use std::any::Any;
use std::sync::Arc;

use rustdb_catalog::{CatalogType, EntryKind, SequenceEntry};
use rustdb_common::{Error, Result};
use rustdb_function::{BindInput, FunctionData, FunctionState, ScalarFunction, ScalarFunctionSet, Stability};
use rustdb_planner::ClientContext;
use rustdb_types::{DataChunk, LogicalType, Vector};

/// The sequence a call uses (`NextvalBindData`).
#[derive(Debug)]
struct SequenceBindData {
    name: String,
    sequence: SequenceEntry,
}

impl FunctionData for SequenceBindData {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// `QualifiedName::ParseComponents`: dot-separated parts, double quotes protect dots.
fn parse_components(input: &str) -> Result<Vec<String>> {
    let mut result = Vec::new();
    let mut entry = String::new();
    let mut chars = input.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => loop {
                match chars.next() {
                    Some('"') => break,
                    Some(c) => entry.push(c),
                    None => {
                        return Err(Error::parser(format!(
                            "Unterminated quote in qualified name! (input: {input})"
                        )));
                    }
                }
            },
            '.' => result.push(std::mem::take(&mut entry)),
            c => entry.push(c),
        }
    }
    if !entry.is_empty() {
        result.push(entry);
    }
    Ok(result)
}

/// `QualifiedName::Parse`: (catalog, schema, name).
fn parse_qualified_name(input: &str) -> Result<(String, String, String)> {
    let mut parts = parse_components(input)?;
    Ok(match parts.len() {
        0 => (String::new(), String::new(), String::new()),
        1 => (String::new(), String::new(), parts.remove(0)),
        2 => {
            let name = parts.remove(1);
            (String::new(), parts.remove(0), name)
        }
        3 => {
            let name = parts.remove(2);
            let schema = parts.remove(1);
            (parts.remove(0), schema, name)
        }
        _ => {
            return Err(Error::parser(format!(
                "Expected catalog.entry, schema.entry or entry: too many entries found (input: {input})"
            )));
        }
    })
}

/// `NextValBind`: the sequence named by the constant argument.
fn bind_sequence(input: &BindInput<'_>) -> Result<Option<Arc<dyn FunctionData>>> {
    let Some(Some(name)) = input.constants.first() else {
        if input.argument_types.first().map(LogicalType::id) == Some(rustdb_types::LogicalTypeId::Unknown) {
            return Err(rustdb_planner::expression_binder::parameter_not_resolved());
        }
        return Err(Error::not_implemented(
            "currval/nextval requires a constant sequence - non-constant sequences are no longer supported",
        ));
    };
    if name.is_null() {
        return Ok(None);
    }
    let context = input
        .client
        .and_then(|client| client.downcast_ref::<ClientContext>())
        .ok_or_else(|| Error::internal("nextval bound without a client context"))?;
    let (mut catalog, mut schema, name) = parse_qualified_name(&name.to_varchar()?)?;
    if !schema.is_empty() && catalog.is_empty() && context.catalog(&schema).is_some() {
        catalog = std::mem::take(&mut schema);
    }
    let found = context
        .get_entry(&catalog, &schema, CatalogType::Sequence, &name, false)?
        .ok_or_else(|| Error::internal("sequence lookup"))?;
    let EntryKind::Sequence(sequence) = &found.entry.kind else {
        return Err(Error::internal("sequence lookup found another entry"));
    };
    Ok(Some(Arc::new(SequenceBindData {
        name: found.entry.name().to_owned(),
        sequence: sequence.clone(),
    })))
}

/// `NextValFunction`: one sequence value per row.
fn sequence_kernel(
    state: &FunctionState,
    count: usize,
    result: &mut Vector,
    next: fn(&SequenceBindData) -> Result<i64>,
) -> Result<()> {
    let Some(data) = state
        .bind_data
        .as_ref()
        .and_then(|data| data.as_any().downcast_ref::<SequenceBindData>())
    else {
        *result = Vector::constant_null(LogicalType::BIGINT);
        return Ok(());
    };
    let mut values = Vector::flat(LogicalType::BIGINT, count.max(1));
    {
        let slots = values.values_mut::<i64>();
        for slot in slots.iter_mut().take(count) {
            *slot = next(data)?;
        }
    }
    *result = values;
    Ok(())
}

/// `nextval` and `currval`.
pub(crate) fn sequence_functions() -> Vec<ScalarFunctionSet> {
    let make = |name: &'static str, next: fn(&SequenceBindData) -> Result<i64>| {
        let mut function = ScalarFunction::new(
            name,
            vec![LogicalType::VARCHAR],
            LogicalType::BIGINT,
            move |args: &DataChunk, state: &mut FunctionState, result: &mut Vector| {
                sequence_kernel(state, args.len(), result, next)
            },
        )
        .with_bind(|_, input| bind_sequence(input));
        function.stability = Stability::Volatile;
        function.null_handling = rustdb_function::NullHandling::Special;
        let mut set = ScalarFunctionSet::new(name);
        set.add(function);
        set
    };
    vec![
        make("nextval", |data| data.sequence.next_value(&data.name)),
        make("currval", |data| data.sequence.current_value()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qualified_names_split_on_unquoted_dots() {
        assert_eq!(
            parse_qualified_name("a.\"b.c\".d").unwrap(),
            ("a".to_owned(), "b.c".to_owned(), "d".to_owned())
        );
        assert_eq!(
            parse_qualified_name("s.seq").unwrap(),
            (String::new(), "s".to_owned(), "seq".to_owned())
        );
        assert!(parse_qualified_name("a.b.c.d").is_err());
    }
}
