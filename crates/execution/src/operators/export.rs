//! `EXPORT DATABASE` (DuckDB's `PhysicalExport`): after its input copied the tables,
//! writes `schema.sql` (the catalog's `CREATE` statements) and `load.sql` (a
//! `COPY ... FROM` per table).

use std::fmt::Write as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use rustdb_ast::keyword::write_optionally_quoted;
use rustdb_ast::value_to_sql_string;
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
    fn schema_sql(&self) -> String {
        let mut result = String::new();
        for info in rustdb_catalog::checkpoint::export_create_infos(&self.catalog, &self.transaction) {
            if let Some(sql) = info.to_sql() {
                result += &sql;
                result += ";\n";
            }
        }
        result.push('\n');
        result
    }

    /// `WriteCopyStatement` for every exported table.
    fn load_sql(&self) -> String {
        let export = &self.export;
        let mut result = String::new();
        for table in &export.tables {
            result += "COPY ";
            if table.schema_name != "main" && !table.schema_name.is_empty() {
                result += &write_optionally_quoted(&table.schema_name);
                result.push('.');
            }
            let file_path = table.file_path.replace('\\', "/");
            let _ = write!(
                result,
                "{} FROM {} (FORMAT '{}'",
                write_optionally_quoted(&table.table_name),
                value_to_sql_string(&Value::varchar(&file_path)),
                export.format
            );
            let mut options = export.options.clone();
            if export.format == "csv" {
                options.retain(|(name, _)| !name.eq_ignore_ascii_case("force_not_null"));
                if !table.not_null_columns.is_empty() {
                    options.push((
                        "force_not_null".to_owned(),
                        table.not_null_columns.iter().map(Value::varchar).collect(),
                    ));
                }
            }
            for (name, values) in &options {
                let values = if values.is_empty() {
                    vec![Value::boolean(true)]
                } else {
                    values.clone()
                };
                let _ = write!(result, ", {name} ");
                if let [value] = values.as_slice() {
                    result += &value_to_sql_string(value);
                } else {
                    let rendered: Vec<String> = values.iter().map(value_to_sql_string).collect();
                    let _ = write!(result, "({})", rendered.join(", "));
                }
            }
            result += ");\n";
        }
        result
    }
}

fn write_file(path: &str, contents: &str) -> Result<()> {
    std::fs::write(path, contents)
        .map_err(|error| Error::new(ErrorKind::Io, format!("Cannot open file \"{path}\": {error}")))
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
            let directory = self.export.file_path.trim_end_matches('/');
            write_file(&format!("{directory}/schema.sql"), &self.schema_sql())?;
            write_file(&format!("{directory}/load.sql"), &self.load_sql())?;
        }
        chunk.set_columns(Vec::new(), 0);
        Ok(SourceResult::Finished)
    }
}
