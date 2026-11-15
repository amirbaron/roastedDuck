//! PostgreSQL-owned catalog metadata and table scans. No native table data is created.
use std::any::Any;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError, Weak};

use roastedduck_buffer::BufferManager;
use roastedduck_catalog::backend::{BackendScan, CatalogBackend, TableBackend};
use roastedduck_catalog::{
    Catalog, CatalogEntry, CatalogName, CatalogType, EntryKind, SchemaEntry, TableColumn, TableEntry,
    Transaction, TypeEntry,
};
use roastedduck_common::{ByteString, Error, Result};
use roastedduck_function::{FunctionClient, FunctionData, TableFunction};
use roastedduck_postgres::{
    BinaryParameter, Driver,
    codec::{Codec, Options},
    config::Parameters,
};
use roastedduck_types::{DataChunk, LogicalType};

use super::identifier;
use super::transaction::{Cursor, Decoder, Participant, PgTransaction};

type CacheKey = (CatalogType, String, String, u8);

#[derive(Debug)]
pub(crate) struct PgCatalog {
    own: Weak<Self>,
    native: Weak<Catalog>,
    driver: Driver,
    parameters: Parameters,
    buffer_manager: BufferManager,
    read_only: bool,
    isolation: String,
    schemas: Vec<String>,
    default_schema: ByteString,
    cache: Mutex<HashMap<CacheKey, Arc<CatalogEntry>>>,
}
impl PgCatalog {
    fn mark_write(&self, transaction: &Transaction) -> Result<()> {
        let native = self.native()?;
        if self.read_only {
            return Err(Error::binder("Cannot modify a read-only Postgres database"));
        }
        transaction.modify_database(native.name(), None, native.locks(), false, false)
    }
    pub fn new(
        native: &Arc<Catalog>,
        parameters: Parameters,
        buffer_manager: BufferManager,
        read_only: bool,
        isolation: String,
        schemas: Vec<String>,
    ) -> Result<Arc<Self>> {
        let driver = Driver::new()?;
        // Construction validates the endpoint before the attachment is published.
        driver.connect(&parameters)?.batch("SELECT version()")?;
        let default_schema = schemas.first().map_or("public", String::as_str).into();
        Ok(Arc::new_cyclic(|own| Self {
            own: own.clone(),
            native: Arc::downgrade(native),
            driver,
            parameters,
            buffer_manager,
            read_only,
            isolation,
            schemas,
            default_schema,
            cache: Mutex::default(),
        }))
    }
    fn native(&self) -> Result<Arc<Catalog>> {
        self.native
            .upgrade()
            .ok_or_else(|| Error::internal("Postgres catalog has closed"))
    }
    pub fn transaction(
        &self,
        client: &Arc<FunctionClient>,
        transaction: &Transaction,
    ) -> Result<Arc<PgTransaction>> {
        let catalog = self.native()?;
        let participant = transaction.participant(&catalog.name(), || {
            let session = self.driver.connect(&self.parameters)?;
            Ok(Arc::new(Participant(PgTransaction::new(
                session,
                client.clone(),
                self.buffer_manager.clone(),
                &self.isolation,
                self.read_only,
            ))))
        })?;
        participant
            .as_any()
            .downcast_ref::<Participant>()
            .map(|p| p.0.clone())
            .ok_or_else(|| Error::internal("Postgres catalog has another transaction participant"))
    }
    pub fn current(&self, client: &Arc<FunctionClient>) -> Result<Arc<PgTransaction>> {
        let transaction = client
            .active_transaction()
            .ok_or_else(|| Error::internal("Postgres operation without active transaction"))?;
        let transaction = transaction
            .downcast_ref::<Transaction>()
            .ok_or_else(|| Error::internal("Invalid Postgres transaction state"))?;
        self.transaction(client, transaction)
    }
    pub fn clear_cache(&self) {
        self.cache.lock().unwrap_or_else(PoisonError::into_inner).clear();
    }

    fn schema(
        &self,
        client: &Arc<FunctionClient>,
        transaction: &Transaction,
        name: &str,
    ) -> Result<Option<Arc<CatalogEntry>>> {
        if !self.schemas.is_empty() && !self.schemas.iter().any(|s| s == name) {
            return Ok(None);
        }
        let pg = self.transaction(client, transaction)?;
        let rows = pg.metadata("SELECT nspname::text FROM pg_catalog.pg_namespace WHERE lower(nspname::text)=lower($1::text) ORDER BY nspname::text=$1::text DESC,nspname", &[text_parameter(name)])?;
        let Some(row) = rows.first() else {
            return Ok(None);
        };
        let name = row[0].to_varchar()?;
        let native = self.native()?;
        let catalog_name =
            CatalogName::with_oid_allocator(ByteString::from(native.name().as_ref()), native.oid_allocator());
        Ok(Some(Arc::new(native.new_entry(
            name.as_str(),
            EntryKind::Schema(SchemaEntry::new(&catalog_name, &name)),
        ))))
    }

    fn table(
        &self,
        client: &Arc<FunctionClient>,
        transaction: &Transaction,
        schema: &str,
        name: &str,
        options: Options,
    ) -> Result<Option<Arc<CatalogEntry>>> {
        let Some(schema_entry) =
            self.lookup(client, transaction, CatalogType::Schema, b"", schema.as_bytes())?
        else {
            return Ok(None);
        };
        let schema = utf8(schema_entry.name())?.to_owned();
        let pg = self.transaction(client, transaction)?;
        let rows = pg.metadata("SELECT c.oid::int8,c.relname::text,c.relkind::text,obj_description(c.oid,'pg_class') FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname=$1::text AND lower(c.relname::text)=lower($2::text) AND c.relkind IN ('r','v','m','f','p') ORDER BY c.relname::text=$2::text DESC,c.relname", &[text_parameter(&schema),text_parameter(name)])?;
        let Some(row) = rows.first() else {
            return Ok(None);
        };
        let oid = row[0]
            .as_i64()
            .ok_or_else(|| Error::internal("Invalid Postgres table OID"))?;
        let name = row[1].to_varchar()?;
        let relation = format!("{}.{}", identifier(&schema), identifier(&name));
        let columns = pg.describe(&format!("SELECT * FROM {relation}"), true)?;
        let attributes = pg.metadata("SELECT a.attname::text,a.attndims::int4,a.attnotnull,col_description(a.attrelid,a.attnum) FROM pg_catalog.pg_attribute a WHERE a.attrelid=$1::int8::oid AND a.attnum>0 AND NOT a.attisdropped ORDER BY a.attnum", &[BinaryParameter(Some(oid.to_be_bytes().to_vec()))])?;
        if attributes.len() != columns.len() {
            return Err(Error::internal("Postgres table metadata changed during binding"));
        }
        let codecs = columns
            .iter()
            .zip(&attributes)
            .map(|(column, attribute)| {
                Codec::new(
                    &column.data_type,
                    column.type_modifier,
                    attribute[1].as_i32().unwrap_or(0).max(0) as usize,
                    options,
                )
            })
            .collect::<Result<Vec<_>>>()?;
        let names = columns.iter().map(|c| c.name.clone()).collect::<Vec<_>>();
        let mut table = TableEntry::default();
        for (index, ((column, attribute), codec)) in columns.iter().zip(&attributes).zip(&codecs).enumerate()
        {
            table.columns.push(TableColumn {
                name: column.name.as_str().into(),
                logical_type: codec.logical_type.clone(),
                default_value: None,
                generated: None,
                comment: attribute[3].clone(),
                compression_type: roastedduck_ast::CompressionType::CompressionAuto,
            });
            if attribute[2].as_bool() == Some(true) {
                table.not_null_columns.push(index);
            }
        }
        table.backend = Some(Arc::new(PgTable {
            catalog: self.own.clone(),
            relation,
            table_name: name.clone(),
            names,
            codecs,
            emit_ctid: !matches!(row[2].as_str(), Some("v" | "f")),
        }));
        let mut entry = self.native()?.new_entry(name, EntryKind::Table(table));
        entry.comment = row[3].clone();
        Ok(Some(Arc::new(entry)))
    }
    fn user_type(
        &self,
        client: &Arc<FunctionClient>,
        transaction: &Transaction,
        schema: &str,
        name: &str,
        options: Options,
    ) -> Result<Option<Arc<CatalogEntry>>> {
        let pg = self.transaction(client, transaction)?;
        let rows = pg.metadata("SELECT t.typname::text FROM pg_catalog.pg_type t JOIN pg_catalog.pg_namespace n ON n.oid=t.typnamespace WHERE n.nspname=$1::text AND t.typname=$2::text AND t.typtype IN ('e','c')", &[text_parameter(schema), text_parameter(name)])?;
        if rows.is_empty() {
            return Ok(None);
        }
        let columns = pg.describe(
            &format!("SELECT NULL::{}.{}", identifier(schema), identifier(name)),
            true,
        )?;
        let column = columns
            .first()
            .ok_or_else(|| Error::internal("Postgres type without a column"))?;
        let codec = Codec::new(&column.data_type, -1, 0, options)?;
        Ok(Some(Arc::new(self.native()?.new_entry(
            name,
            EntryKind::Type(TypeEntry {
                user_type: codec.logical_type.with_alias(name),
            }),
        ))))
    }
}
impl CatalogBackend for PgCatalog {
    fn drop_entry(
        &self,
        client: &Arc<FunctionClient>,
        transaction: &Transaction,
        info: &roastedduck_ast::DropInfo,
    ) -> Result<()> {
        use roastedduck_ast::CatalogType as C;
        let keyword = match info.catalog_type {
            C::SchemaEntry => "SCHEMA",
            C::TableEntry => "TABLE",
            C::ViewEntry => "VIEW",
            C::TypeEntry => "TYPE",
            C::SequenceEntry => "SEQUENCE",
            C::IndexEntry => "INDEX",
            _ => {
                return Err(Error::not_implemented(
                    "This DROP operation is not implemented for Postgres",
                ));
            }
        };
        self.mark_write(transaction)?;
        let object = if info.catalog_type == C::SchemaEntry {
            identifier(utf8(&info.name)?)
        } else {
            format!(
                "{}.{}",
                identifier(utf8(&info.schema)?),
                identifier(utf8(&info.name)?)
            )
        };
        let exists = if info.if_not_found == roastedduck_ast::OnEntryNotFound::ReturnNull {
            "IF EXISTS "
        } else {
            ""
        };
        let cascade = if info.cascade { " CASCADE" } else { "" };
        self.transaction(client, transaction)?
            .execute(&format!("DROP {keyword} {exists}{object}{cascade}"), true)?;
        self.clear_cache();
        Ok(())
    }
    fn create(
        &self,
        client: &Arc<FunctionClient>,
        transaction: &Transaction,
        info: &roastedduck_ast::CreateInfo,
    ) -> Result<Option<Arc<CatalogEntry>>> {
        use roastedduck_ast::{CreateInfoKind as K, OnCreateConflict as C};
        let mut info = info.clone();
        info.catalog.clear();
        let (kind, name) = match &mut info.kind {
            K::Schema => (CatalogType::Schema, info.schema.clone()),
            K::Table(table) => {
                table.query = None;
                for column in &mut table.columns.columns {
                    let ty = roastedduck_postgres::copy::postgres_type(&column.logical_type);
                    let sql = roastedduck_postgres::copy::type_sql(&ty)?;
                    column.logical_type = ty.with_alias(sql);
                }
                (CatalogType::Table, table.table.clone())
            }
            K::Type(ty) => (CatalogType::Type, ty.name.clone()),
            _ => {
                return Err(Error::not_implemented(
                    "This CREATE operation is not yet implemented for Postgres",
                ));
            }
        };
        let schema = if kind == CatalogType::Schema {
            b"".as_slice()
        } else {
            info.schema.as_bytes()
        };
        let existing = self.lookup(client, transaction, kind, schema, &name)?;
        if existing.is_some() && info.on_conflict == C::IgnoreOnConflict {
            return Ok(None);
        }
        self.mark_write(transaction)?;
        let pg = self.transaction(client, transaction)?;
        if existing.is_some() && info.on_conflict == C::ReplaceOnConflict {
            let keyword = match kind {
                CatalogType::Schema => "SCHEMA",
                CatalogType::Table => "TABLE",
                _ => "TYPE",
            };
            let object = if kind == CatalogType::Schema {
                identifier(utf8(&name)?)
            } else {
                format!("{}.{}", identifier(utf8(&info.schema)?), identifier(utf8(&name)?))
            };
            pg.execute(&format!("DROP {keyword} {object}"), true)?;
        }
        info.on_conflict = C::ErrorOnConflict;
        let sql = if let K::Type(ty) = &info.kind
            && let Some(fields) = ty.logical_type.struct_children()
        {
            let fields = fields
                .iter()
                .map(|(name, ty)| {
                    Ok(format!(
                        "{} {}",
                        identifier(utf8(name)?),
                        roastedduck_postgres::copy::type_sql(&roastedduck_postgres::copy::postgres_type(ty))?
                    ))
                })
                .collect::<Result<Vec<_>>>()?
                .join(",");
            format!(
                "CREATE TYPE {}.{} AS ({fields})",
                identifier(utf8(&info.schema)?),
                identifier(utf8(&name)?)
            )
        } else {
            info.to_sql()
                .ok_or_else(|| Error::not_implemented("Postgres CREATE has no SQL representation"))?
                .into_string()
                .map_err(|_| Error::invalid_input("Invalid UTF-8 in Postgres DDL"))?
        };
        pg.execute(&sql, true)?;
        self.clear_cache();
        let schema = if kind == CatalogType::Schema {
            b"".as_slice()
        } else {
            info.schema.as_bytes()
        };
        self.lookup(client, transaction, kind, schema, &name)
    }
    fn catalog_type(&self) -> &'static str {
        "postgres"
    }
    fn default_schema(&self) -> &[u8] {
        self.default_schema.as_bytes()
    }
    fn lookup(
        &self,
        client: &Arc<FunctionClient>,
        transaction: &Transaction,
        kind: CatalogType,
        schema: &[u8],
        name: &[u8],
    ) -> Result<Option<Arc<CatalogEntry>>> {
        if !matches!(
            kind,
            CatalogType::Schema | CatalogType::Table | CatalogType::View | CatalogType::Type
        ) {
            return Ok(None);
        }
        let schema =
            std::str::from_utf8(schema).map_err(|_| Error::invalid_input("Invalid Postgres schema name"))?;
        let name =
            std::str::from_utf8(name).map_err(|_| Error::invalid_input("Invalid Postgres entry name"))?;
        let options = type_options(client);
        let flags = u8::from(options.array_as_varchar)
            | u8::from(options.numeric_as_varchar) << 1
            | u8::from(options.numeric_nan_as_null) << 2;
        let key = (kind, schema.to_owned(), name.to_owned(), flags);
        if let Some(entry) = self
            .cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&key)
        {
            return Ok(Some(entry.clone()));
        }
        let entry = match kind {
            CatalogType::Schema => self.schema(client, transaction, name)?,
            CatalogType::Table | CatalogType::View => {
                self.table(client, transaction, schema, name, options)?
            }
            CatalogType::Type => self.user_type(client, transaction, schema, name, options)?,
            _ => None,
        };
        if let Some(entry) = &entry {
            self.cache
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(key, entry.clone());
        }
        Ok(entry)
    }
}

fn text_parameter(value: &str) -> BinaryParameter {
    BinaryParameter(Some(value.as_bytes().to_vec()))
}
pub(super) fn type_options(client: &FunctionClient) -> Options {
    let setting = |name: &str, default| {
        client
            .settings()
            .and_then(|s| s.current_setting(name).ok())
            .and_then(|v| v.as_bool())
            .unwrap_or(default)
    };
    Options {
        array_as_varchar: setting("pg_array_as_varchar", false),
        numeric_as_varchar: setting("pg_numeric_as_varchar", false),
        numeric_nan_as_null: setting("pg_numeric_nan_as_null", true),
        resolve_custom_types: true,
    }
}

#[derive(Clone, Debug)]
struct PgTable {
    table_name: String,
    catalog: Weak<PgCatalog>,
    relation: String,
    names: Vec<String>,
    codecs: Vec<Codec>,
    emit_ctid: bool,
}
impl TableBackend for PgTable {
    fn validate_insert(&self, returning: bool) -> Result<()> {
        if returning {
            Err(Error::binder(
                "RETURNING clause not yet supported for insertion into Postgres table",
            ))
        } else {
            Ok(())
        }
    }
    fn insert(
        &self,
        client: &Arc<FunctionClient>,
        transaction: &Transaction,
        columns: &[usize],
        chunks: &mut dyn Iterator<Item = Result<DataChunk>>,
    ) -> Result<u64> {
        let catalog = self
            .catalog
            .upgrade()
            .ok_or_else(|| Error::internal("Postgres catalog closed"))?;
        catalog.mark_write(transaction)?;
        let text = client
            .settings()
            .and_then(|s| s.current_setting("pg_use_binary_copy").ok())
            .and_then(|v| v.as_bool())
            == Some(false)
            || self.codecs.iter().any(Codec::requires_text_copy);
        let names = columns
            .iter()
            .map(|column| {
                self.names
                    .get(*column)
                    .map(|s| identifier(s))
                    .ok_or_else(|| Error::internal("Invalid Postgres insert column"))
            })
            .collect::<Result<Vec<_>>>()?;
        let column_list = if names.is_empty() {
            String::new()
        } else {
            format!(" ({})", names.join(","))
        };
        let format = if text { "TEXT, NULL '\u{0008}'" } else { "BINARY" };
        let sql = format!("COPY {}{column_list} FROM STDIN (FORMAT {format})", self.relation);
        catalog
            .transaction(client, transaction)?
            .copy_input(&sql, text, chunks)
    }
    fn scan(&self, client: &Arc<FunctionClient>, _: &Transaction) -> Result<BackendScan> {
        let mut function =
            TableFunction::new("postgres_scan", vec![]).with_scan(|data, column_ids, state, output| {
                let data = data
                    .and_then(|d| d.as_any().downcast_ref::<ScanData>())
                    .ok_or_else(|| Error::internal("Missing Postgres table scan data"))?;
                if state.local.is_none() {
                    let pg = data.catalog.current(&data.client)?;
                    let mut projections = Vec::new();
                    let mut codecs = Vec::new();
                    for id in column_ids {
                        if *id == roastedduck_planner::operator::COLUMN_IDENTIFIER_ROW_ID {
                            projections.push(
                                if data.table.emit_ctid {
                                    "ctid"
                                } else {
                                    "NULL::bigint"
                                }
                                .to_owned(),
                            );
                            codecs.push(if data.text {
                                Decoder::TextCtid
                            } else {
                                Decoder::Ctid
                            });
                        } else {
                            let index = *id as usize;
                            let codec = data
                                .table
                                .codecs
                                .get(index)
                                .ok_or_else(|| Error::internal("Invalid Postgres projected column"))?;
                            projections.push(codec.projection(&identifier(&data.table.names[index]))?);
                            codecs.push(if data.text {
                                Decoder::Text(codec.logical_type.clone())
                            } else {
                                Decoder::Value(codec.clone())
                            });
                        }
                    }
                    if projections.is_empty() {
                        projections.push("1".into());
                    }
                    let sql = format!("SELECT {} FROM {}", projections.join(","), data.table.relation);
                    let cursor = pg.cursor(&sql, &[], codecs, true, data.text)?;
                    state.local = Some(Box::new(cursor));
                }
                let cursor = state
                    .local
                    .as_mut()
                    .and_then(|s| s.downcast_mut::<Arc<Mutex<Cursor>>>())
                    .ok_or_else(|| Error::internal("Invalid Postgres cursor"))?;
                if let Some(chunk) = cursor.lock().unwrap_or_else(PoisonError::into_inner).next()? {
                    state.position += chunk.len() as u64;
                    *output = chunk;
                    Ok(true)
                } else {
                    output.set_len(0);
                    Ok(false)
                }
            });
        function.projection_pushdown = true;
        function.projected_output = true;
        function.static_explain = Some(Arc::new(|data| {
            data.and_then(|d| d.as_any().downcast_ref::<ScanData>())
                .map_or_else(Vec::new, |d| {
                    vec![("Table".into(), d.table.table_name.as_str().into())]
                })
        }));
        Ok(BackendScan {
            function,
            bind_data: Some(Arc::new(ScanData {
                table: self.clone(),
                text: super::use_text_protocol(client),
                catalog: self
                    .catalog
                    .upgrade()
                    .ok_or_else(|| Error::internal("Postgres catalog closed"))?,
                client: client.clone(),
            })),
            virtual_columns: vec![(
                roastedduck_planner::operator::COLUMN_IDENTIFIER_ROW_ID,
                "rowid".into(),
                LogicalType::BIGINT,
            )],
        })
    }
}
#[derive(Debug)]
struct ScanData {
    text: bool,
    table: PgTable,
    catalog: Arc<PgCatalog>,
    client: Arc<FunctionClient>,
}
impl FunctionData for ScanData {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn utf8(value: &ByteString) -> Result<&str> {
    value
        .to_str()
        .map_err(|_| Error::invalid_input("Invalid UTF-8 in Postgres identifier"))
}
