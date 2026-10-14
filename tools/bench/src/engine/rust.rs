//! rustdb, linked directly.

use std::path::Path;

use rustdb::{Config, Connection, Database, ErrorKind, ExtensionLoad, LogicalType, LogicalTypeId, Value};

use super::{DbConfig, Engine, EngineError, ExtensionStatus, QueryResult};
use crate::verify::{ResultColumn, ResultTable, ValueKind};

/// rustdb with one database and connection.
#[derive(Debug, Default)]
pub struct RustEngine {
    state: Option<(Database, Connection)>,
}

impl RustEngine {
    pub fn new() -> Self {
        Self::default()
    }

    fn connection(&mut self) -> Result<&mut Connection, EngineError> {
        self.state
            .as_mut()
            .map(|(_, connection)| connection)
            .ok_or_else(|| EngineError::failed("no open database"))
    }
}

fn convert_error(error: &rustdb::Error) -> EngineError {
    if error.kind() == ErrorKind::NotImplemented {
        EngineError::unsupported(error.to_string())
    } else {
        EngineError::failed(error.to_string())
    }
}

impl Engine for RustEngine {
    type Result = rustdb::MaterializedResult;

    fn version(&self) -> String {
        format!("rustdb {}", env!("CARGO_PKG_VERSION"))
    }

    fn open(&mut self, path: Option<&Path>, config: &DbConfig) -> Result<(), EngineError> {
        self.close();
        let mut db_config = Config::default();
        for (name, value) in config.options() {
            let value = match name {
                "threads" => Value::bigint(i64::from(config.threads)),
                _ => Value::varchar(value),
            };
            db_config = db_config.with_option(name, value);
        }
        let path = path.map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
        let database = Database::open(&path, &db_config).map_err(|e| convert_error(&e))?;
        let connection = database.connect().map_err(|e| convert_error(&e))?;
        self.state = Some((database, connection));
        Ok(())
    }

    fn close(&mut self) {
        self.state = None;
    }

    fn query(&mut self, sql: &str) -> Result<Self::Result, EngineError> {
        self.connection()?.query(sql).map_err(|e| convert_error(&e))
    }

    fn load_extension(&mut self, name: &str) -> Result<ExtensionStatus, EngineError> {
        let (database, _) = self
            .state
            .as_ref()
            .ok_or_else(|| EngineError::failed("no open database"))?;
        Ok(match database.load_extension(name) {
            ExtensionLoad::Loaded => ExtensionStatus::Loaded,
            ExtensionLoad::NotAvailable => ExtensionStatus::NotAvailable,
            ExtensionLoad::Unknown => ExtensionStatus::Unknown,
        })
    }
}

impl QueryResult for rustdb::MaterializedResult {
    fn row_count(&self) -> usize {
        rustdb::MaterializedResult::row_count(self)
    }

    fn column_count(&self) -> usize {
        rustdb::MaterializedResult::column_count(self)
    }

    fn to_table(&self) -> Result<ResultTable, EngineError> {
        let columns = self
            .names()
            .iter()
            .zip(self.types())
            .map(|(name, ty)| ResultColumn {
                name: name.clone(),
                type_name: ty.to_string(),
                kind: value_kind(ty),
            })
            .collect();
        let rows = (0..self.row_count())
            .map(|row| {
                (0..self.column_count())
                    .map(|column| {
                        let value = self.value(column, row);
                        if value.is_null() {
                            Ok(None)
                        } else {
                            value.to_varchar().map(Some).map_err(|e| convert_error(&e))
                        }
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ResultTable { columns, rows })
    }
}

fn value_kind(ty: &LogicalType) -> ValueKind {
    use LogicalTypeId as L;
    match ty.id() {
        L::Boolean => ValueKind::Boolean,
        L::TinyInt
        | L::SmallInt
        | L::Integer
        | L::BigInt
        | L::HugeInt
        | L::UTinyInt
        | L::USmallInt
        | L::UInteger
        | L::UBigInt
        | L::UHugeInt => ValueKind::Integer,
        L::Float => ValueKind::Float,
        L::Double => ValueKind::Double,
        L::Decimal => ValueKind::Decimal {
            scale: ty.decimal_properties().map_or(0, |(_, scale)| scale),
        },
        L::Varchar | L::Char => ValueKind::Varchar,
        _ => ValueKind::Other,
    }
}
