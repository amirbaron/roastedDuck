//! `EXPORT DATABASE` (DuckDB's `PhysicalExport`): after its input copied the tables,
//! writes `schema.sql` (the catalog's `CREATE` statements) and `load.sql` (a
//! `COPY ... FROM` per table).

use rustdb_common::{ByteString, byte_write};
use std::io::Write as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use rustdb_ast::keyword::write_optionally_quoted_bytes as write_optionally_quoted;

use rustdb_catalog::{Catalog, Transaction};
use rustdb_common::{Error, ErrorKind, Result};
use rustdb_planner::operator::LogicalExport;
use rustdb_types::{DataChunk, LogicalType, Value};

use crate::operator::{ErasedSink, ErasedSource, PhysicalOperator, PhysicalRef, SinkState};
use crate::{ExecutionContext, Sink, SinkResult, Source, SourceResult};

/// Writes the export's SQL files (DuckDB's `PhysicalExport`).
#[derive(Debug)]
pub struct PhysicalExport {
    types: Vec<LogicalType>,
    children: Vec<PhysicalRef>,
    export: LogicalExport,
    catalog: Arc<Catalog>,
    transaction: Arc<Transaction>,
}

impl PhysicalExport {
    pub fn new(
        child: Option<PhysicalRef>,
        export: LogicalExport,
        catalog: Arc<Catalog>,
        transaction: Arc<Transaction>,
    ) -> Self {
        Self {
            types: vec![LogicalType::BOOLEAN],
            children: child.into_iter().collect(),
            export,
            catalog,
            transaction,
        }
    }

    /// `WriteCatalogEntries`.
    fn schema_sql(&self) -> ByteString {
        let mut result = ByteString::new();
        for info in rustdb_catalog::checkpoint::export_create_infos(&self.catalog, &self.transaction) {
            if let Some(sql) = info.to_sql() {
                result.push_bytes(&sql);
                result.push_bytes(";\n");
            }
        }
        result.push(b'\n');
        result
    }

    /// `WriteCopyStatement` for every exported table.
    fn load_sql(&self) -> Result<ByteString> {
        let export = &self.export;
        let mut result = ByteString::new();
        for table in &export.tables {
            result.push_bytes("COPY ");
            if table.schema_name != "main" && !table.schema_name.is_empty() {
                result.push_bytes(write_optionally_quoted(&table.schema_name));
                result.push(b'.');
            }
            let file_path = table.file_path.replace_byte(b'\\', b"/");
            let _ = byte_write!(
                result,
                "{} FROM {} (FORMAT '{}'",
                write_optionally_quoted(&table.table_name),
                rustdb_ast::keyword::sql_string_bytes(&file_path),
                export.format
            );
            let mut options = export.options.clone();
            if export.format == "csv" {
                options.retain(|(name, _)| !name.eq_ignore_ascii_case("force_not_null"));
                if !table.not_null_columns.is_empty() {
                    options.push((
                        "force_not_null".into(),
                        table
                            .not_null_columns
                            .iter()
                            .map(|name| Value::try_varchar(name))
                            .collect::<Result<_>>()?,
                    ));
                }
            }
            for (name, values) in &options {
                let values = if values.is_empty() {
                    vec![Value::boolean(true)]
                } else {
                    values.clone()
                };
                let _ = byte_write!(result, ", {name} ");
                if let [value] = values.as_slice() {
                    result.push_bytes(value.try_to_sql_string()?);
                } else {
                    let rendered: Vec<ByteString> = values
                        .iter()
                        .map(Value::try_to_sql_string)
                        .collect::<Result<_>>()?;
                    let _ = byte_write!(result, "({})", ByteString::join(&rendered, b", "));
                }
            }
            result.push_bytes(");\n");
        }
        Ok(result)
    }
}

fn write_file(path: &[u8], contents: &[u8]) -> Result<()> {
    rustdb_function::file_system::verify_can_write(path)?;
    if rustdb_function::registered_files::lookup(path).is_none() {
        return std::fs::write(rustdb_function::file_system::native_path(path)?, contents).map_err(|error| {
            Error::new_bytes(
                ErrorKind::Io,
                rustdb_common::byte_format!("Cannot open file \"{path}\": {error}"),
            )
        });
    }
    let mut output = rustdb_function::registered_files::Output::create(path)?;
    output
        .write_all(contents)
        .and_then(|()| output.flush())
        .map_err(|error| {
            if let Some(original) = error.get_ref().and_then(|error| error.downcast_ref::<Error>()) {
                return original.clone();
            }
            Error::new_bytes(
                ErrorKind::Io,
                rustdb_common::byte_format!("Cannot open file \"{path}\": {error}"),
            )
        })
}

impl PhysicalOperator for PhysicalExport {
    fn name(&self) -> &'static str {
        "EXPORT"
    }
    fn types(&self) -> &[LogicalType] {
        &self.types
    }
    fn children(&self) -> &[PhysicalRef] {
        &self.children
    }
    fn as_sink(&self) -> Option<&dyn ErasedSink> {
        if self.children.is_empty() {
            None
        } else {
            Some(self)
        }
    }
    fn as_source(&self) -> Option<&dyn ErasedSource> {
        Some(self)
    }
}

impl Sink for PhysicalExport {
    type Global = ();
    type Local = ();

    fn global_sink_state(&self, _: &ExecutionContext) -> Result<()> {
        Ok(())
    }

    fn local_sink_state(&self, _: &ExecutionContext, (): &()) -> Result<()> {
        Ok(())
    }

    fn sink(&self, _: &ExecutionContext, _: &DataChunk, (): &(), (): &mut ()) -> Result<SinkResult> {
        Ok(SinkResult::NeedMoreInput)
    }

    fn combine(&self, _: &ExecutionContext, (): &(), (): ()) -> Result<()> {
        Ok(())
    }
}

impl Source for PhysicalExport {
    type Global = AtomicBool;
    type Local = ();

    fn global_source_state(&self, _: &ExecutionContext, _: Option<&SinkState>) -> Result<AtomicBool> {
        Ok(AtomicBool::new(false))
    }

    fn local_source_state(&self, _: &ExecutionContext, _: &AtomicBool) -> Result<()> {
        Ok(())
    }

    fn get_data(
        &self,
        _: &ExecutionContext,
        chunk: &mut DataChunk,
        finished: &AtomicBool,
        (): &mut (),
    ) -> Result<SourceResult> {
        if !finished.swap(true, Ordering::Relaxed) {
            let bytes = self.export.file_path.as_bytes();
            let end = bytes.iter().rposition(|byte| *byte != b'/').map_or(0, |i| i + 1);
            let directory = &bytes[..end];
            write_file(
                &rustdb_common::byte_format!("{directory}/schema.sql"),
                &self.schema_sql(),
            )?;
            write_file(
                &rustdb_common::byte_format!("{directory}/load.sql"),
                &self.load_sql()?,
            )?;
        }
        chunk.set_columns(Vec::new(), 0);
        Ok(SourceResult::Finished)
    }
}
