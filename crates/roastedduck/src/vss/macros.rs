//! Pinned VSS table macros and their catalog registration.
//! See `mod.rs` and `LICENSE` for upstream provenance and attribution.

use roastedduck_ast::{
    MacroFunction, MacroFunctionKind, ParsedExpression, SqlStatementKind, TableMacroFunction,
};
use roastedduck_catalog::{Catalog, EntryKind, MacroEntry};
use roastedduck_common::{CaseInsensitiveMap, Error, Result};
use roastedduck_types::{LogicalType, Value};

const JOIN_SQL: &str = include_str!("vss_join.sql");
const MATCH_SQL: &str = include_str!("vss_match.sql");

fn table_macro(sql: &str, parameters: &[&str]) -> Result<MacroFunction> {
    let mut statements = roastedduck_parser::parse(sql)?;
    if statements.len() != 1 {
        return Err(Error::internal("VSS table macro must contain one SELECT"));
    }
    let Some(SqlStatementKind::Select(select)) = statements.pop().map(|statement| statement.kind) else {
        return Err(Error::internal("VSS table macro is not a SELECT"));
    };
    let mut parameters: Vec<_> = parameters
        .iter()
        .map(|name| ParsedExpression::column_ref([*name]))
        .collect();
    parameters.push(ParsedExpression::column_ref(["metric"]));
    let mut default_parameters = CaseInsensitiveMap::default();
    default_parameters.insert("metric", ParsedExpression::constant(Value::varchar("l2sq")));
    Ok(MacroFunction {
        types: vec![LogicalType::UNKNOWN; parameters.len()],
        parameters,
        default_parameters,
        kind: MacroFunctionKind::Table(TableMacroFunction {
            query_node: select.node,
        }),
    })
}

/// Registers the pinned `vss_join` and `vss_match` SQL bodies in `system.main`.
pub(crate) fn register_macros(system: &Catalog) -> Result<()> {
    for (name, sql, parameters) in [
        (
            "vss_join",
            JOIN_SQL,
            &["left_table", "right_table", "left_col", "right_col", "k"][..],
        ),
        (
            "vss_match",
            MATCH_SQL,
            &["right_table", "left_col", "right_col", "k"][..],
        ),
    ] {
        let mut entry = system.new_entry(
            name,
            EntryKind::TableMacro(MacroEntry {
                macros: vec![table_macro(sql, parameters)?],
            }),
        );
        entry.internal = true;
        entry.temporary = true;
        system.create_entry_committed("main", entry)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Config, Database};

    #[test]
    fn vector_macros_execute_with_and_without_optimizer_without_an_index() -> Result<()> {
        let database = Database::open(
            "",
            &Config {
                load_extensions: false,
                ..Config::default()
            },
        )?;
        register_macros(&database.instance().system_catalog)?;
        let mut connection = database.connect()?;
        connection.query("CREATE TABLE lhs(q FLOAT[2])")?;
        connection.query("INSERT INTO lhs VALUES ([1,0])")?;
        connection.query("CREATE TABLE rhs(id INTEGER, vec FLOAT[2])")?;
        connection.query("INSERT INTO rhs VALUES (1,[1,0]), (2,[0,1]), (3,[-1,0])")?;
        for setting in ["disable_optimizer", "enable_optimizer"] {
            connection.query(format!("PRAGMA {setting}"))?;
            for (metric, order) in [
                ("l2sq", "ASC"),
                ("l2", "ASC"),
                ("cosine", "DESC"),
                ("cos", "DESC"),
                ("ip", "DESC"),
            ] {
                let joined = connection.query(format!(
                    "SELECT right_tbl.id FROM vss_join(lhs, rhs, q, vec, 2, metric := '{metric}') ORDER BY score {order}"
                ))?;
                assert_eq!(joined.row_count(), 2);
                assert_eq!(joined.value(0, 0), Value::integer(1));
                assert_eq!(joined.value(0, 1), Value::integer(2));
                let matched = connection.query(format!(
                    "SELECT matches[1].row.id, matches[2].row.id FROM lhs, vss_match(rhs, q, vec, 2, metric := '{metric}')"
                ))?;
                assert_eq!(matched.row_count(), 1);
                assert_eq!(matched.value(0, 0), Value::integer(1));
                assert_eq!(matched.value(1, 0), Value::integer(2));
            }
            // The l2sq macro's score is Euclidean distance, not squared distance.
            let distance = connection.query(
                "SELECT score > 1.4 AND score < 1.5 AS correct_distance \
                 FROM vss_join(lhs, rhs, q, vec, 2) WHERE right_tbl.id = 2",
            )?;
            assert_eq!(distance.value(0, 0), Value::boolean(true));
        }
        Ok(())
    }
}
