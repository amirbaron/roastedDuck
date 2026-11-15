//! SQL-standard metadata path. No `pg_class`/`pg_attribute`/type discovery is used here.
use super::{PgCatalog, PgTable, identifier};
use roastedduck_catalog::{CatalogEntry, EntryKind, TableColumn, TableEntry, Transaction};
use roastedduck_common::{ByteString, Result};
use roastedduck_function::FunctionClient;
use roastedduck_postgres::codec::{Codec, Options};
use roastedduck_types::{LogicalType, Value};
use std::sync::Arc;

impl PgCatalog {
    pub(super) fn information_schema_table(
        &self,
        client: &Arc<FunctionClient>,
        transaction: &Transaction,
        schema: &str,
        name: &str,
        options: Options,
        created: Option<&roastedduck_ast::ColumnList>,
    ) -> Result<Option<Arc<CatalogEntry>>> {
        let rows = self.transaction(client, transaction)?.metadata(
            "SELECT table_name::text,column_name::text,data_type::text,is_nullable::text,ordinal_position::integer FROM information_schema.columns WHERE table_schema=$1::text AND lower(table_name)=lower($2::text) AND table_schema NOT IN ('information_schema','pg_catalog','pg_toast') ORDER BY table_name=$2::text DESC,table_name,ordinal_position",
            &[super::text_parameter(schema),super::text_parameter(name)])?;
        let Some(first) = rows.first() else {
            return Ok(None);
        };
        let name = first[0].to_varchar()?;
        let mut names = Vec::new();
        let mut codecs = Vec::new();
        let mut table = TableEntry::default();
        for (index, row) in rows
            .iter()
            .take_while(|r| r[0].as_str() == Some(&name))
            .enumerate()
        {
            let column_name = row[1].to_varchar()?;
            let codec = if let Some(declared) = created.and_then(|c| c.columns.get(index)) {
                let ty = roastedduck_postgres::copy::postgres_type(&declared.logical_type);
                Codec::for_created(&super::remove_created_alias(ty)?, options)?
            } else {
                Codec::from_information_schema(&row[2].to_varchar()?, options)?
            };
            table.columns.push(TableColumn {
                name: column_name.as_str().into(),
                logical_type: codec.logical_type.clone(),
                default_value: created
                    .and_then(|c| c.columns.get(index))
                    .and_then(|c| c.expression.as_deref())
                    .cloned(),
                generated: None,
                comment: Value::null(LogicalType::VARCHAR),
                compression_type: roastedduck_ast::CompressionType::CompressionAuto,
            });
            if row[3].as_str() == Some("NO") {
                table.not_null_columns.push(index);
            }
            names.push(column_name);
            codecs.push(codec);
        }
        let mut exposed: Vec<ByteString> = names.iter().map(|n| n.as_str().into()).collect();
        roastedduck_planner::deduplicate_columns(&mut exposed);
        for (column, name) in table.columns.iter_mut().zip(exposed) {
            column.name = name;
        }
        table.backend = Some(Arc::new(PgTable {
            catalog: self.own.clone(),
            relation: format!("{}.{}", identifier(schema), identifier(&name)),
            table_name: name.clone(),
            pages: 0,
            names,
            codecs,
        }));
        let mut entry = self.native()?.new_entry(name, EntryKind::Table(table));
        entry.internal = schema.starts_with("pg_") || schema == "information_schema";
        Ok(Some(Arc::new(entry)))
    }
}
