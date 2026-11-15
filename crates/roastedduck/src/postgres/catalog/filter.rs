//! SQL predicates emitted by the pinned PostgreSQL extension's table filters.
use roastedduck_common::{Error, Result};
use roastedduck_storage::table_filter::TableFilter;
use roastedduck_types::{LogicalTypeId, Value, compare::CompareOp};

fn literal(value: &Value) -> Result<String> {
    if value.logical_type().id() == LogicalTypeId::Blob {
        let mut out = String::from("'\\x");
        for byte in value.as_bytes().unwrap_or_default() {
            const HEX: &[u8; 16] = b"0123456789ABCDEF";
            out.push(char::from(HEX[usize::from(byte >> 4)]));
            out.push(char::from(HEX[usize::from(byte & 15)]));
        }
        out.push_str("'::BYTEA");
        Ok(out)
    } else {
        Ok(crate::postgres::literal(&value.to_varchar()?))
    }
}
pub(super) fn supported(filter: &TableFilter) -> bool {
    match filter {
        TableFilter::Expression(_) => false,
        TableFilter::And(children) | TableFilter::Or(children) => children.iter().all(supported),
        TableFilter::StructExtract { child_filter, .. } => supported(child_filter),
        TableFilter::Optional(child) => child.as_deref().is_none_or(supported),
        _ => true,
    }
}
pub(super) fn expression(column: &str, filter: &TableFilter, virtual_column: bool) -> Result<String> {
    Ok(match filter {
        TableFilter::IsNull => format!("{column} IS NULL"),
        TableFilter::IsNotNull => format!("{column} IS NOT NULL"),
        TableFilter::ConstantComparison { op, constant } => {
            if virtual_column {
                return Ok("FALSE".into());
            }
            let op = match op {
                CompareOp::Equal => "=",
                CompareOp::NotEqual => "<>",
                CompareOp::LessThan => "<",
                CompareOp::LessThanOrEqual => "<=",
                CompareOp::GreaterThan => ">",
                CompareOp::GreaterThanOrEqual => ">=",
            };
            let collation = if constant.logical_type().id() == LogicalTypeId::Varchar {
                " COLLATE \"C\""
            } else {
                ""
            };
            format!("{column} {op} {}{collation}", literal(constant)?)
        }
        TableFilter::And(children) | TableFilter::Or(children) => {
            let op = if matches!(filter, TableFilter::And(_)) {
                " AND "
            } else {
                " OR "
            };
            let entries = children
                .iter()
                .map(|f| expression(column, f, virtual_column))
                .collect::<Result<Vec<_>>>()?
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>();
            if entries.is_empty() {
                String::new()
            } else {
                format!("({})", entries.join(op))
            }
        }
        TableFilter::StructExtract {
            child_name,
            child_filter,
            ..
        } => {
            let name = std::str::from_utf8(child_name)
                .map_err(|_| Error::invalid_input("Invalid Postgres field name"))?;
            expression(
                &format!("({column}).{}", crate::postgres::identifier(name)),
                child_filter,
                virtual_column,
            )?
        }
        TableFilter::Optional(child) => match child {
            Some(child) => expression(column, child, virtual_column)?,
            None => String::new(),
        },
        TableFilter::In(values) => format!(
            "{column} IN ({})",
            values.iter().map(literal).collect::<Result<Vec<_>>>()?.join(", ")
        ),
        // Join bloom/selectivity hints are optional and may be installed after
        // binding. They never replace the join predicate, so unsupported hints
        // can be omitted just like the upstream dynamic filter.
        TableFilter::SelectivityOptional(child) => {
            if supported(&child.child) {
                expression(column, &child.child, virtual_column)?
            } else {
                String::new()
            }
        }
        TableFilter::Dynamic(_) => String::new(),
        TableFilter::Expression(_) => return Err(Error::internal("Unsupported Postgres table filter")),
    })
}
