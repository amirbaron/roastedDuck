//! PostgreSQL index definitions are catalog metadata; the server owns their storage.
use super::{PgCatalog, identifier, text_parameter, utf8};
use roastedduck_ast::{CreateIndexInfo, CreateInfo, CreateInfoKind, IndexConstraintType};
use roastedduck_catalog::IndexEntry;
use roastedduck_catalog::backend::CatalogBackend;
use roastedduck_catalog::{CatalogEntry, CatalogType, EntryKind, Transaction};
use roastedduck_common::{ByteString, CaseInsensitiveMap, Error, Result};
use roastedduck_function::FunctionClient;
use std::fmt::Write as _;
use std::sync::Arc;

struct Unqualify;
impl roastedduck_ast::visit::VisitorMut for Unqualify {
    fn visit_expression(&mut self, expression: &mut roastedduck_ast::ParsedExpression) {
        if let roastedduck_ast::ParsedExpressionKind::ColumnRef(column) = &mut expression.kind {
            if let Some(name) = column.column_names.last().cloned() {
                column.column_names = vec![name];
            }
            return;
        }
        roastedduck_ast::visit::walk_expression(self, expression);
    }
}

impl PgCatalog {
    pub(super) fn index(
        &self,
        client: &Arc<FunctionClient>,
        transaction: &Transaction,
        schema: &str,
        name: &str,
    ) -> Result<Option<Arc<CatalogEntry>>> {
        let Some(schema_entry) =
            self.lookup(client, transaction, CatalogType::Schema, b"", schema.as_bytes())?
        else {
            return Ok(None);
        };
        let schema = utf8(schema_entry.name())?;
        let pg = self.transaction(client, transaction)?;
        let rows = pg.metadata("SELECT tablename::text,indexname::text FROM pg_catalog.pg_indexes WHERE schemaname=$1::text AND lower(indexname)=lower($2::text) ORDER BY indexname=$2::text DESC,indexname", &[text_parameter(schema), text_parameter(name)])?;
        let Some(row) = rows.first() else {
            return Ok(None);
        };
        let table = row[0].to_varchar()?;
        let name = row[1].to_varchar()?;
        // Pinned upstream reloads names only: no reconstructed key expressions or
        // uniqueness flags, even when PostgreSQL knows them.
        let info = CreateIndexInfo {
            name: name.as_str().into(),
            table: table.into(),
            constraint_type: IndexConstraintType::None,
            parsed_expressions: vec![],
            scan_types: vec![],
            names: vec![],
            column_ids: vec![],
            options: CaseInsensitiveMap::default(),
            index_type_name: ByteString::default(),
        };
        let mut entry = self.native()?.new_entry(
            name,
            EntryKind::Index(IndexEntry {
                info,
                schema: schema.into(),
                storage: None,
            }),
        );
        entry.internal = schema_entry.internal;
        Ok(Some(Arc::new(entry)))
    }

    pub(super) fn create_index(
        &self,
        client: &Arc<FunctionClient>,
        transaction: &Transaction,
        info: &CreateInfo,
    ) -> Result<Arc<CatalogEntry>> {
        use roastedduck_ast::visit::VisitorMut;
        let CreateInfoKind::Index(index) = &info.kind else {
            return Err(Error::internal(
                "Postgres index creation without an index definition",
            ));
        };
        if info.on_conflict == roastedduck_ast::OnCreateConflict::IgnoreOnConflict
            && let Some(existing) =
                self.lookup(client, transaction, CatalogType::Index, &info.schema, &index.name)?
        {
            return Ok(existing);
        }
        let table = self
            .lookup(
                client,
                transaction,
                CatalogType::Table,
                &info.schema,
                &index.table,
            )?
            .ok_or_else(|| Error::catalog("Postgres index table does not exist"))?;
        let mut index = index.as_ref().clone();
        for expression in &mut index.parsed_expressions {
            Unqualify.visit_expression(expression);
        }
        table.name().clone_into(&mut index.table);
        let expressions = index
            .parsed_expressions
            .iter()
            .map(|e| {
                e.to_sql_bytes()
                    .into_string()
                    .map_err(|_| Error::invalid_input("Invalid UTF-8 in Postgres index expression"))
            })
            .collect::<Result<Vec<_>>>()?
            .join(", ");
        let sql = format!(
            "CREATE {}INDEX {} ON {}.{} ({expressions})",
            if index.constraint_type == IndexConstraintType::Unique {
                "UNIQUE "
            } else {
                ""
            },
            identifier(utf8(&index.name)?),
            identifier(utf8(&info.schema)?),
            identifier(utf8(table.name())?)
        );
        self.mark_write(transaction)?;
        self.transaction(client, transaction)?.execute(&sql, true)?;
        let mut entry = self.native()?.new_entry(
            index.name.clone(),
            EntryKind::Index(IndexEntry {
                info: index,
                schema: info.schema.clone(),
                storage: None,
            }),
        );
        entry.internal = table.internal;
        let entry = Arc::new(entry);
        let schema = utf8(&info.schema)?.to_owned();
        self.remember_entry(
            (CatalogType::Index, schema, utf8(entry.name())?.to_owned(), 0),
            entry.clone(),
        )?;
        Ok(entry)
    }
}

/// PostgreSQL identifiers must retain their spelling even when DuckDB's SQL
/// renderer would omit quotes. Expressions keep the pinned upstream rendering.
pub(super) fn create_sql(info: &CreateInfo) -> Result<Option<String>> {
    use roastedduck_ast::{Constraint, ForeignKeyType, TableColumnType};
    let schema = identifier(utf8(&info.schema)?);
    Ok(Some(match &info.kind {
        CreateInfoKind::Schema => format!("CREATE SCHEMA {schema}"),
        CreateInfoKind::View(view) => {
            let aliases = view
                .aliases
                .iter()
                .map(|name| utf8(name).map(identifier))
                .collect::<Result<Vec<_>>>()?;
            let aliases = if aliases.is_empty() {
                String::new()
            } else {
                format!("({}) ", aliases.join(", "))
            };
            format!(
                "CREATE VIEW {schema}.{} {aliases}AS {}",
                identifier(utf8(&view.view_name)?),
                view.query
            )
        }
        CreateInfoKind::Type(ty) if ty.logical_type.enum_values().is_some() => {
            let values = ty
                .logical_type
                .enum_values()
                .unwrap_or_default()
                .iter()
                .map(|v| utf8(v).map(super::super::literal))
                .collect::<Result<Vec<_>>>()?;
            format!(
                "CREATE TYPE {schema}.{} AS ENUM ({})",
                identifier(utf8(&ty.name)?),
                values.join(", ")
            )
        }
        CreateInfoKind::Table(table) => {
            let mut not_null = std::collections::BTreeSet::new();
            let mut primary = std::collections::BTreeSet::new();
            let mut unique = std::collections::BTreeSet::new();
            let mut multi_primary = Vec::new();
            let mut extra = Vec::new();
            for constraint in &table.constraints {
                match constraint {
                    Constraint::NotNull(c) => {
                        not_null.insert(c.index.0);
                    }
                    Constraint::Unique(c) if c.index.0 != u64::MAX => {
                        if c.is_primary_key {
                            primary.insert(c.index.0);
                        } else {
                            unique.insert(c.index.0);
                        }
                    }
                    Constraint::Unique(c) => {
                        if c.is_primary_key {
                            multi_primary.extend(c.columns.iter().cloned());
                        }
                        let columns = c
                            .columns
                            .iter()
                            .map(|name| utf8(name).map(identifier))
                            .collect::<Result<Vec<_>>>()?;
                        extra.push(format!(
                            "{}({})",
                            if c.is_primary_key { "PRIMARY KEY" } else { "UNIQUE" },
                            columns.join(", ")
                        ));
                    }
                    Constraint::ForeignKey(c)
                        if !matches!(
                            c.fk_type,
                            ForeignKeyType::FkTypeForeignKeyTable | ForeignKeyType::FkTypeSelfReferenceTable
                        ) => {}
                    _ => extra.push(utf8(&constraint.to_sql())?.to_owned()),
                }
            }
            let mut columns = Vec::new();
            for (index, column) in table.columns.columns.iter().enumerate() {
                let index = index as u64;
                let mut sql = format!(
                    "{} {}",
                    identifier(utf8(&column.name)?),
                    roastedduck_postgres::copy::type_sql(&column.logical_type)?
                );
                if not_null.contains(&index)
                    && !primary.contains(&index)
                    && !multi_primary.contains(&column.name)
                {
                    sql.push_str(" NOT NULL");
                }
                if primary.contains(&index) {
                    sql.push_str(" PRIMARY KEY");
                }
                if unique.contains(&index) {
                    sql.push_str(" UNIQUE");
                }
                if let Some(expression) = &column.expression {
                    if column.category == TableColumnType::Generated {
                        let _ = write!(sql, " GENERATED ALWAYS AS({expression})");
                    } else {
                        let _ = write!(sql, " DEFAULT({expression})");
                    }
                }
                columns.push(sql);
            }
            columns.extend(extra);
            format!(
                "CREATE TABLE {schema}.{}({});",
                identifier(utf8(&table.table)?),
                columns.join(", ")
            )
        }
        _ => return Ok(None),
    }))
}
