//! PostgreSQL-owned catalog metadata and table scans. No native table data is created.
use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError, Weak};

use roastedduck_buffer::BufferManager;
use roastedduck_catalog::backend::{BackendScan, CatalogBackend, TableBackend, TableMutation};
use roastedduck_catalog::{
    Catalog, CatalogEntry, CatalogName, CatalogType, EntryKind, SchemaEntry, TableColumn, TableEntry,
    Transaction, TypeEntry,
};
use roastedduck_common::{ByteString, Error, Result};
use roastedduck_function::FunctionClient;
use roastedduck_postgres::{
    BinaryParameter,
    codec::{Codec, Options},
};
use roastedduck_types::{DataChunk, LogicalType};

use super::identifier;
use super::transaction::{Participant, PgTransaction};

mod ddl;
mod filter;
mod introspection;
pub(crate) mod scan;
mod staleness;

type CacheKey = (CatalogType, String, String, u8);

#[derive(Debug)]
pub(crate) struct PgCatalog {
    own: Weak<Self>,
    native: Weak<Catalog>,
    pub(crate) pool: Arc<roastedduck_postgres::pool::Pool>,
    buffer_manager: BufferManager,
    read_only: bool,
    version: u32,
    isolation: String,
    schemas: Vec<String>,
    default_schema: ByteString,
    cache: Mutex<HashMap<CacheKey, Arc<CatalogEntry>>>,
    listings: Mutex<HashMap<(CatalogType, String), Vec<String>>>,
    signatures: Mutex<staleness::Signatures>,
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
        factory: Arc<dyn Fn() -> Result<roastedduck_postgres::Session> + Send + Sync>,
        buffer_manager: BufferManager,
        read_only: bool,
        isolation: String,
        schemas: Vec<String>,
        pool_config: roastedduck_postgres::pool::Config,
    ) -> Result<Arc<Self>> {
        // Construction validates the endpoint before the attachment is published.
        let pool = roastedduck_postgres::pool::Pool::new(pool_config, factory)?;
        let version = {
            let lease = pool.acquire(None)?;
            let mut rows = lease.session()?.stream("SHOW server_version_num", &[])?;
            let values = rows.next_batch(2)?;
            values
                .first()
                .and_then(|r| r.first())
                .and_then(Option::as_deref)
                .and_then(|b| std::str::from_utf8(b).ok())
                .and_then(|s| s.parse().ok())
                .unwrap_or(0)
        };
        let default_schema = schemas.first().map_or("public", String::as_str).into();
        Ok(Arc::new_cyclic(|own| Self {
            own: own.clone(),
            native: Arc::downgrade(native),
            pool,
            buffer_manager,
            read_only,
            version,
            isolation,
            schemas,
            default_schema,
            cache: Mutex::default(),
            listings: Mutex::default(),
            signatures: Mutex::default(),
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
            let session = self.pool.acquire(client.interrupt_flag().as_deref())?;
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
        {
            let mut signatures = self.signatures.lock().unwrap_or_else(PoisonError::into_inner);
            signatures.generation = signatures.generation.wrapping_add(1);
            signatures.values.clear();
        }
        self.cache.lock().unwrap_or_else(PoisonError::into_inner).clear();
        self.listings
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }

    fn evict_entry(&self, kind: CatalogType, schema: &str, name: &str) {
        let same_kind = |other| {
            other == kind
                || matches!(
                    (kind, other),
                    (CatalogType::Table, CatalogType::View) | (CatalogType::View, CatalogType::Table)
                )
        };
        self.cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|(k, s, n, _), _| {
                !(same_kind(*k) && s == schema && n == name || kind == CatalogType::Schema && s == name)
            });
        let mut listings = self.listings.lock().unwrap_or_else(PoisonError::into_inner);
        listings.retain(|(_, s), _| !(kind == CatalogType::Schema && s == name));
        for ((k, s), names) in listings.iter_mut() {
            if same_kind(*k) && s == schema {
                names.retain(|n| n != name);
            }
        }
    }

    fn remember_entry(&self, key: CacheKey, entry: Arc<CatalogEntry>) -> Result<()> {
        let (kind, schema, _, _) = &key;
        let name = utf8(entry.name())?.to_owned();
        let mut listings = self.listings.lock().unwrap_or_else(PoisonError::into_inner);
        for ((k, s), names) in listings.iter_mut() {
            if (*k == *kind
                || matches!(
                    (*kind, *k),
                    (CatalogType::Table, CatalogType::View) | (CatalogType::View, CatalogType::Table)
                ))
                && s == schema
                && !names.contains(&name)
            {
                names.push(name.clone());
            }
        }
        drop(listings);
        self.cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key, entry);
        Ok(())
    }

    fn listing(
        &self,
        client: &Arc<FunctionClient>,
        transaction: &Transaction,
        kind: CatalogType,
        schema: &str,
    ) -> Result<Vec<String>> {
        let signature = if matches!(kind, CatalogType::Table | CatalogType::View) {
            self.check_staleness(client, schema)?
        } else {
            None
        };
        let key = (kind, schema.to_owned());
        let cached = self
            .listings
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&key)
            .cloned();
        let names = if let Some(names) = cached {
            names
        } else {
            let pg = self.transaction(client, transaction)?;
            let rows = if staleness::information_schema(client) {
                match kind {
                    CatalogType::Schema => pg.metadata("SELECT schema_name::text FROM information_schema.schemata WHERE schema_name NOT IN ('information_schema','pg_catalog','pg_toast') ORDER BY schema_name", &[])?,
                    CatalogType::Table | CatalogType::View => pg.metadata("SELECT DISTINCT table_name::text FROM information_schema.columns WHERE table_schema=$1::text AND table_schema NOT IN ('information_schema','pg_catalog','pg_toast') ORDER BY 1", &[text_parameter(schema)])?,
                    _ => Vec::new(),
                }
            } else {
                match kind {
                CatalogType::Schema => pg.metadata("SELECT nspname::text FROM pg_catalog.pg_namespace ORDER BY oid", &[])?,
                CatalogType::Table | CatalogType::View => pg.metadata("SELECT c.relname::text FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname=$1::text AND c.relkind IN ('r','v','m','f','p') ORDER BY c.oid", &[text_parameter(schema)])?,
                CatalogType::Index => pg.metadata("SELECT indexname::text FROM pg_catalog.pg_indexes WHERE schemaname=$1::text ORDER BY indexname", &[text_parameter(schema)])?,
                CatalogType::Type => pg.metadata("SELECT t.typname::text FROM pg_catalog.pg_type t JOIN pg_catalog.pg_namespace n ON n.oid=t.typnamespace WHERE n.nspname=$1::text AND t.typtype IN ('e','c') ORDER BY t.oid", &[text_parameter(schema)])?,
                _ => return Ok(Vec::new()),
            }
            };
            let names = rows
                .iter()
                .map(|r| r[0].to_varchar())
                .collect::<Result<Vec<_>>>()?;
            self.listings
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(key, names.clone());
            names
        };
        self.stage_loaded_signature(client, transaction, schema, signature)?;
        Ok(names)
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
        let sql = if staleness::information_schema(client) {
            "SELECT schema_name::text FROM information_schema.schemata WHERE schema_name NOT IN ('information_schema','pg_catalog','pg_toast') AND lower(schema_name)=lower($1::text) ORDER BY schema_name=$1::text DESC,schema_name"
        } else {
            "SELECT nspname::text FROM pg_catalog.pg_namespace WHERE lower(nspname::text)=lower($1::text) ORDER BY nspname::text=$1::text DESC,nspname"
        };
        let rows = pg.metadata(sql, &[text_parameter(name)])?;
        let Some(row) = rows.first() else {
            return Ok(None);
        };
        let name = row[0].to_varchar()?;
        // Upstream captures the schema's table/type/index name sets together.
        // Later CREATE TABLE does not retroactively add its implicit row type.
        for kind in [CatalogType::Table, CatalogType::Type, CatalogType::Index] {
            self.listing(client, transaction, kind, &name)?;
        }
        let native = self.native()?;
        let catalog_name =
            CatalogName::with_oid_allocator(ByteString::from(native.name().as_ref()), native.oid_allocator());
        let mut entry = native.new_entry(
            name.as_str(),
            EntryKind::Schema(SchemaEntry::new(&catalog_name, &name)),
        );
        entry.internal = name == "information_schema" || name.starts_with("pg_");
        Ok(Some(Arc::new(entry)))
    }

    fn table(
        &self,
        client: &Arc<FunctionClient>,
        transaction: &Transaction,
        schema: &str,
        name: &str,
        options: Options,
        created: Option<&roastedduck_ast::ColumnList>,
    ) -> Result<Option<Arc<CatalogEntry>>> {
        let Some(schema_entry) =
            self.lookup(client, transaction, CatalogType::Schema, b"", schema.as_bytes())?
        else {
            return Ok(None);
        };
        let schema = utf8(schema_entry.name())?.to_owned();
        if staleness::information_schema(client) {
            return self.information_schema_table(client, transaction, &schema, name, options, created);
        }
        let pg = self.transaction(client, transaction)?;
        let rows = pg.metadata("SELECT c.oid::int8,c.relname::text,c.relkind::text,obj_description(c.oid,'pg_class'),c.relpages::int8 FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname=$1::text AND lower(c.relname::text)=lower($2::text) AND c.relkind IN ('r','v','m','f','p') ORDER BY c.relname::text=$2::text DESC,c.relname", &[text_parameter(&schema),text_parameter(name)])?;
        let Some(row) = rows.first() else {
            return Ok(None);
        };
        let oid = row[0]
            .as_i64()
            .ok_or_else(|| Error::internal("Invalid Postgres table OID"))?;
        let name = row[1].to_varchar()?;
        let relation = format!("{}.{}", identifier(&schema), identifier(&name));
        let attributes = pg.metadata("SELECT a.attname::text,a.attndims::int4,a.attnotnull,col_description(a.attrelid,a.attnum),a.attnum::int4,a.atttypid::int8,a.atttypmod::int4 FROM pg_catalog.pg_attribute a WHERE a.attrelid=$1::int8::oid AND a.attnum>0 AND NOT a.attisdropped ORDER BY a.attnum", &[BinaryParameter(Some(oid.to_be_bytes().to_vec()))])?;
        // Catalog discovery must not parse a SELECT over the user relation: that
        // holds an AccessShareLock and would block external DDL until commit.
        if attributes.is_empty() && created.is_none() {
            return Ok(None);
        }
        let columns = attributes
            .iter()
            .map(|row| {
                let oid = row[5]
                    .as_i64()
                    .and_then(|v| u32::try_from(v).ok())
                    .ok_or_else(|| Error::internal("Invalid Postgres type OID"))?;
                pg.catalog_column(row[0].to_varchar()?, oid, row[6].as_i32().unwrap_or(-1))
            })
            .collect::<Result<Vec<_>>>()?;
        let options = if created.is_some() {
            Options {
                array_as_varchar: false,
                ..options
            }
        } else {
            options
        };
        let mut codecs = columns
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
        if let Some(created) = created {
            for (codec, column) in codecs.iter_mut().zip(&created.columns) {
                let declared = roastedduck_postgres::copy::postgres_type(&column.logical_type);
                codec.logical_type = retain_aliases(&codec.logical_type, &declared);
                codec.logical_type = remove_created_alias(codec.logical_type.clone())?;
            }
        }
        let names = columns.iter().map(|c| c.name.clone()).collect::<Vec<_>>();
        // SQL identifiers are case-insensitive in DuckDB. Keep the original
        // PostgreSQL names for wire queries and expose its deduplicated names.
        let mut exposed_names = names
            .iter()
            .map(|s| ByteString::from(s.as_str()))
            .collect::<Vec<_>>();
        roastedduck_planner::deduplicate_columns(&mut exposed_names);
        let mut table = TableEntry::default();
        for (index, ((_column, attribute), codec)) in columns.iter().zip(&attributes).zip(&codecs).enumerate()
        {
            table.columns.push(TableColumn {
                name: exposed_names[index].clone(),
                logical_type: codec.logical_type.clone(),
                default_value: created
                    .and_then(|c| c.columns.get(index))
                    .and_then(|c| c.expression.as_deref())
                    .cloned(),
                generated: None,
                comment: attribute[3].clone(),
                compression_type: roastedduck_ast::CompressionType::CompressionAuto,
            });
            if attribute[2].as_bool() == Some(true) {
                table.not_null_columns.push(index);
            }
        }
        let constraints=pg.metadata("SELECT contype::text,conkey FROM pg_catalog.pg_constraint WHERE conrelid=$1::int8::oid AND contype IN ('p','u') ORDER BY oid", &[BinaryParameter(Some(oid.to_be_bytes().to_vec()))])?;
        for constraint in constraints {
            let mut names = Vec::new();
            for key in constraint[1].children().unwrap_or_default() {
                let attnum = key
                    .as_i128()
                    .ok_or_else(|| Error::internal("Invalid Postgres constraint column"))?;
                let index = attributes
                    .iter()
                    .position(|a| a[4].as_i128() == Some(attnum))
                    .ok_or_else(|| Error::internal("Postgres constraint references a missing column"))?;
                names.push(table.columns[index].name.clone());
            }
            table.constraints.push(roastedduck_ast::Constraint::Unique(
                roastedduck_ast::UniqueConstraint {
                    is_primary_key: constraint[0].as_str() == Some("p"),
                    index: roastedduck_common::LogicalIndex(u64::MAX),
                    columns: names,
                },
            ));
        }
        table.backend = Some(Arc::new(PgTable {
            catalog: self.own.clone(),
            relation,
            table_name: name.clone(),
            pages: if created.is_some() {
                0
            } else {
                row[4].as_i64().unwrap_or(0)
            },
            names,
            codecs,
        }));
        let mut entry = self.native()?.new_entry(name, EntryKind::Table(table));
        entry.comment = row[3].clone();
        entry.internal = schema_entry.internal;
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
        let rows = pg.metadata("SELECT t.typname::text FROM pg_catalog.pg_type t JOIN pg_catalog.pg_namespace n ON n.oid=t.typnamespace WHERE n.nspname=$1::text AND lower(t.typname::text)=lower($2::text) AND t.typtype IN ('e','c') ORDER BY t.typname::text=$2::text DESC,t.typname", &[text_parameter(schema), text_parameter(name)])?;
        let Some(row) = rows.first() else {
            return Ok(None);
        };
        let name = row[0].to_varchar()?;
        let columns = pg.describe(
            &format!("SELECT NULL::{}.{}", identifier(schema), identifier(&name)),
            true,
        )?;
        let column = columns
            .first()
            .ok_or_else(|| Error::internal("Postgres type without a column"))?;
        let codec = Codec::new(&column.data_type, -1, 0, options)?;
        let mut entry = self.native()?.new_entry(
            name.as_str(),
            EntryKind::Type(TypeEntry {
                user_type: codec.logical_type.with_alias(name.as_str()),
            }),
        );
        entry.internal = schema == "information_schema" || schema.starts_with("pg_");
        Ok(Some(Arc::new(entry)))
    }
}
impl CatalogBackend for PgCatalog {
    fn scan(
        &self,
        client: &Arc<FunctionClient>,
        transaction: &Transaction,
        kind: CatalogType,
        schema: &[u8],
    ) -> Result<Vec<Arc<CatalogEntry>>> {
        let schema = std::str::from_utf8(schema)
            .map_err(|_| Error::invalid_input("Invalid UTF-8 in Postgres schema"))?;
        let names = self.listing(client, transaction, kind, schema)?;
        let mut entries = Vec::with_capacity(names.len());
        for name in names {
            if let Some(entry) = self.lookup_impl(
                client,
                transaction,
                kind,
                schema.as_bytes(),
                name.as_bytes(),
                false,
            )? {
                entries.push(entry);
            }
        }
        Ok(entries)
    }
    fn alter(
        &self,
        client: &Arc<FunctionClient>,
        transaction: &Transaction,
        info: &roastedduck_ast::AlterInfo,
    ) -> Result<()> {
        use roastedduck_ast::{AlterInfoKind, AlterTableInfo as A, ParsedExpressionKind};
        let AlterInfoKind::AlterTable(alter) = &info.kind else {
            return Err(Error::binder("Only altering tables is supported for now"));
        };
        let entry = self.lookup(client, transaction, CatalogType::Table, &info.schema, &info.name)?;
        let column_name = |name: &ByteString| -> Result<String> {
            let actual = entry
                .as_ref()
                .and_then(|e| match &e.kind {
                    EntryKind::Table(t) => t.column(name).map(|(_, c)| c.name.clone()),
                    _ => None,
                })
                .unwrap_or_else(|| name.clone());
            Ok(utf8(&actual)?.to_owned())
        };
        let name = entry.as_ref().map_or(&info.name, |e| e.name());
        let prefix = format!(
            "ALTER TABLE {}.{}",
            identifier(utf8(&info.schema)?),
            identifier(utf8(name)?)
        );
        let sql = match alter {
            A::RenameTable(rename) => {
                format!("{prefix} RENAME TO {}", identifier(utf8(&rename.new_table_name)?))
            }
            A::RenameColumn(rename) => format!(
                "{prefix} RENAME COLUMN {} TO {}",
                identifier(&column_name(&rename.old_name)?),
                identifier(utf8(&rename.new_name)?)
            ),
            A::RemoveColumn(remove) => format!(
                "{prefix} DROP COLUMN {}{}",
                if remove.if_column_exists { "IF EXISTS " } else { "" },
                identifier(&column_name(&remove.removed_column)?)
            ),
            A::AddColumn(add) => {
                let mut sql = format!(
                    "{prefix} ADD COLUMN {}{} {}",
                    if add.if_column_not_exists {
                        "IF NOT EXISTS "
                    } else {
                        ""
                    },
                    identifier(utf8(&add.new_column.name)?),
                    add.new_column.logical_type
                );
                if let Some(expression) = &add.new_column.expression {
                    let ParsedExpressionKind::Constant(value) = &expression.kind else {
                        return Err(Error::binder(
                            "Unsupported ALTER TABLE DEFAULT expression type - only constant DEFAULT expressions are supported",
                        ));
                    };
                    sql.push_str(" DEFAULT ");
                    sql.push_str(utf8(&value.value.try_to_sql_string()?)?);
                }
                sql
            }
            _ => {
                return Err(Error::binder(
                    "Unsupported ALTER TABLE type - Postgres tables only support RENAME TABLE, RENAME COLUMN, ADD COLUMN and DROP COLUMN",
                ));
            }
        };
        self.mark_write(transaction)?;
        self.transaction(client, transaction)?.execute(&sql, true)?;
        self.clear_cache();
        self.refresh_signature(client, transaction, utf8(&info.schema)?)?;
        Ok(())
    }
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
        let schema = if info.schema.is_empty() {
            &self.default_schema
        } else {
            &info.schema
        };
        let object = if info.catalog_type == C::SchemaEntry {
            identifier(utf8(&info.name)?)
        } else {
            format!("{}.{}", identifier(utf8(schema)?), identifier(utf8(&info.name)?))
        };
        let exists = if info.if_not_found == roastedduck_ast::OnEntryNotFound::ReturnNull {
            "IF EXISTS "
        } else {
            ""
        };
        let cascade = if info.cascade { " CASCADE" } else { "" };
        self.transaction(client, transaction)?
            .execute(&format!("DROP {keyword} {exists}{object}{cascade}"), true)?;
        let kind = CatalogType::from_ast(info.catalog_type)
            .ok_or_else(|| Error::internal("Invalid Postgres DROP kind"))?;
        self.evict_entry(
            kind,
            if kind == CatalogType::Schema {
                ""
            } else {
                utf8(schema)?
            },
            utf8(&info.name)?,
        );
        if matches!(kind, CatalogType::Table | CatalogType::View) {
            self.refresh_signature(client, transaction, utf8(schema)?)?;
        }
        Ok(())
    }
    fn create(
        &self,
        client: &Arc<FunctionClient>,
        transaction: &Transaction,
        info: &roastedduck_ast::CreateInfo,
    ) -> Result<Option<Arc<CatalogEntry>>> {
        use roastedduck_ast::{CreateInfoKind as K, OnCreateConflict as C};
        if matches!(info.kind, K::Index(_)) {
            return self.create_index(client, transaction, info).map(Some);
        }
        let mut info = info.clone();
        let created_columns = match &info.kind {
            K::Table(table) => Some(table.columns.clone()),
            _ => None,
        };
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
            K::View(view) => (CatalogType::View, view.view_name.clone()),
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
                CatalogType::View => "VIEW",
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
        let sql = if let Some(sql) = ddl::create_sql(&info)? {
            sql
        } else if let K::Type(ty) = &info.kind
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
        self.evict_entry(
            kind,
            if kind == CatalogType::Schema {
                ""
            } else {
                utf8(&info.schema)?
            },
            utf8(&name)?,
        );
        let schema = if kind == CatalogType::Schema {
            b"".as_slice()
        } else {
            info.schema.as_bytes()
        };
        if matches!(kind, CatalogType::Table | CatalogType::View) {
            self.refresh_signature(client, transaction, utf8(&info.schema)?)?;
        }
        if let Some(created) = created_columns {
            let entry = self.table(
                client,
                transaction,
                utf8(&info.schema)?,
                utf8(&name)?,
                type_options(client),
                Some(&created),
            )?;
            if let Some(entry) = &entry {
                self.remember_entry(
                    (
                        CatalogType::Table,
                        utf8(&info.schema)?.to_owned(),
                        utf8(entry.name())?.to_owned(),
                        type_flags(client),
                    ),
                    entry.clone(),
                )?;
            }
            return Ok(entry);
        }
        if staleness::information_schema(client)
            && let K::Type(ty) = &info.kind
        {
            let mut entry = self.native()?.new_entry(
                name.clone(),
                EntryKind::Type(TypeEntry {
                    user_type: ty.logical_type.clone().with_alias(name.clone()),
                }),
            );
            entry.internal = info.schema.starts_with(b"pg_") || info.schema == "information_schema";
            let entry = Arc::new(entry);
            self.remember_entry(
                (
                    CatalogType::Type,
                    utf8(&info.schema)?.to_owned(),
                    utf8(&name)?.to_owned(),
                    type_flags(client),
                ),
                entry.clone(),
            )?;
            return Ok(Some(entry));
        }
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
        self.lookup_impl(client, transaction, kind, schema, name, true)
    }
}
impl PgCatalog {
    fn lookup_impl(
        &self,
        client: &Arc<FunctionClient>,
        transaction: &Transaction,
        kind: CatalogType,
        schema: &[u8],
        name: &[u8],
        check_stale: bool,
    ) -> Result<Option<Arc<CatalogEntry>>> {
        // PostgreSQL views and tables share the same upstream catalog set.
        let kind = if kind == CatalogType::View {
            CatalogType::Table
        } else {
            kind
        };
        if !matches!(
            kind,
            CatalogType::Schema
                | CatalogType::Table
                | CatalogType::View
                | CatalogType::Type
                | CatalogType::Index
        ) {
            return Ok(None);
        }
        let schema =
            std::str::from_utf8(schema).map_err(|_| Error::invalid_input("Invalid Postgres schema name"))?;
        let name =
            std::str::from_utf8(name).map_err(|_| Error::invalid_input("Invalid Postgres entry name"))?;
        let schema_entry = if kind == CatalogType::Schema {
            None
        } else {
            let Some(entry) =
                self.lookup(client, transaction, CatalogType::Schema, b"", schema.as_bytes())?
            else {
                return Ok(None);
            };
            Some(entry)
        };
        let schema = schema_entry
            .as_ref()
            .map_or(Ok(schema), |entry| utf8(entry.name()))?;
        let signature = if kind == CatalogType::Table && check_stale {
            self.check_staleness(client, schema)?
        } else {
            None
        };
        let options = type_options(client);
        let flags = type_flags(client);
        let flags = if kind == CatalogType::Index { 0 } else { flags };
        let key = (kind, schema.to_owned(), name.to_owned(), flags);
        let cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(entry) = cache.get(&key) {
            return Ok(Some(entry.clone()));
        }
        drop(cache);
        let entry = match kind {
            CatalogType::Schema => self.schema(client, transaction, name)?,
            CatalogType::Table | CatalogType::View => {
                self.table(client, transaction, schema, name, options, None)?
            }
            CatalogType::Type if !staleness::information_schema(client) => {
                self.user_type(client, transaction, schema, name, options)?
            }
            CatalogType::Index if !staleness::information_schema(client) => {
                self.index(client, transaction, schema, name)?
            }
            _ => None,
        };
        if let Some(entry) = &entry {
            // Resolve the server's exact spelling before reusing a folded lookup.
            // PostgreSQL can contain both "object" and "OBJECT" simultaneously.
            let key = (kind, schema.to_owned(), utf8(entry.name())?.to_owned(), flags);
            if let Some(cached) = self
                .cache
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(&key)
            {
                return Ok(Some(cached.clone()));
            }
            self.remember_entry(key, entry.clone())?;
        }
        self.stage_loaded_signature(client, transaction, schema, signature)?;
        Ok(entry)
    }
}

// Retain CREATE-time aliases without changing the decoder's physical layout.
// Reloaded catalog types deliberately use PostgreSQL's normal type mapping.
fn retain_aliases(actual: &LogicalType, declared: &LogicalType) -> LogicalType {
    if actual.id() != declared.id() {
        return actual.clone();
    }
    let mapped = if let (Some(actual), Some(declared)) = (actual.child_type(), declared.child_type()) {
        LogicalType::list(retain_aliases(actual, declared))
    } else if let (Some(actual_fields), Some(declared_fields)) =
        (actual.struct_children(), declared.struct_children())
    {
        if actual_fields.len() == declared_fields.len() {
            LogicalType::struct_type(
                actual_fields
                    .iter()
                    .zip(declared_fields)
                    .map(|((name, actual), (_, declared))| (name.clone(), retain_aliases(actual, declared)))
                    .collect(),
            )
        } else {
            actual.clone()
        }
    } else {
        actual.clone()
    };
    declared.alias().map_or_else(
        || mapped.clone(),
        |alias| mapped.clone().with_alias(alias.clone()),
    )
}

fn text_parameter(value: &str) -> BinaryParameter {
    BinaryParameter(Some(value.as_bytes().to_vec()))
}
fn type_flags(client: &FunctionClient) -> u8 {
    let options = type_options(client);
    u8::from(options.array_as_varchar)
        | u8::from(options.numeric_as_varchar) << 1
        | u8::from(options.numeric_nan_as_null) << 2
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
    pages: i64,
    catalog: Weak<PgCatalog>,
    relation: String,
    names: Vec<String>,
    codecs: Vec<Codec>,
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
    fn validate_update(&self, returning: bool, _defaults: bool) -> Result<()> {
        if returning {
            return Err(Error::binder(
                "RETURNING clause not yet supported for updates of a Postgres table",
            ));
        }
        Ok(())
    }
    fn validate_delete(&self, returning: bool) -> Result<()> {
        if returning {
            Err(Error::binder(
                "RETURNING clause not yet supported for deletion of a Postgres table",
            ))
        } else {
            Ok(())
        }
    }
    fn validate_merge(&self, returning: bool) -> Result<()> {
        if returning {
            Err(Error::not_implemented(
                "RETURNING is not implemented for Postgres yet",
            ))
        } else {
            Ok(())
        }
    }
    fn begin_insert(
        &self,
        client: &Arc<FunctionClient>,
        transaction: &Transaction,
        columns: &[usize],
    ) -> Result<Box<dyn TableMutation>> {
        let catalog = self
            .catalog
            .upgrade()
            .ok_or_else(|| Error::internal("Postgres catalog closed"))?;
        catalog.mark_write(transaction)?;
        let (sql, text) = self.insert_sql(client, columns)?;
        Ok(super::mutation::insert(
            catalog.transaction(client, transaction)?,
            sql,
            text,
        ))
    }
    fn begin_update(
        &self,
        client: &Arc<FunctionClient>,
        transaction: &Transaction,
        columns: &[usize],
        defaults: bool,
    ) -> Result<Box<dyn TableMutation>> {
        let catalog = self
            .catalog
            .upgrade()
            .ok_or_else(|| Error::internal("Postgres catalog closed"))?;
        catalog.mark_write(transaction)?;
        let names = columns
            .iter()
            .map(|i| {
                self.names
                    .get(*i)
                    .cloned()
                    .ok_or_else(|| Error::internal("Invalid Postgres updated column"))
            })
            .collect::<Result<Vec<_>>>()?;
        let types = columns
            .iter()
            .map(|i| self.codecs[*i].logical_type.clone())
            .collect::<Vec<_>>();
        super::mutation::update(
            catalog.transaction(client, transaction)?,
            &self.relation,
            &names,
            &types,
            defaults,
        )
    }
    fn begin_delete(
        &self,
        client: &Arc<FunctionClient>,
        transaction: &Transaction,
        row_id: usize,
    ) -> Result<Box<dyn TableMutation>> {
        let catalog = self
            .catalog
            .upgrade()
            .ok_or_else(|| Error::internal("Postgres catalog closed"))?;
        catalog.mark_write(transaction)?;
        Ok(super::mutation::delete(
            catalog.transaction(client, transaction)?,
            self.relation.clone(),
            row_id,
        ))
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
        let (sql, text) = self.insert_sql(client, columns)?;
        catalog
            .transaction(client, transaction)?
            .copy_input(&sql, text, chunks)
    }
    fn scan(&self, client: &Arc<FunctionClient>, transaction: &Transaction) -> Result<BackendScan> {
        scan::bind(self, client, transaction)
    }
}

fn utf8(value: &ByteString) -> Result<&str> {
    value
        .to_str()
        .map_err(|_| Error::invalid_input("Invalid UTF-8 in Postgres identifier"))
}

impl PgTable {
    fn insert_sql(&self, client: &Arc<FunctionClient>, columns: &[usize]) -> Result<(String, bool)> {
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
        Ok((sql, text))
    }
}

fn remove_created_alias(ty: LogicalType) -> Result<LogicalType> {
    if let Some(alias) = ty.alias()
        && !alias.eq_ignore_ascii_case(b"json")
    {
        if !matches!(
            ty.id(),
            roastedduck_types::LogicalTypeId::Struct | roastedduck_types::LogicalTypeId::Enum
        ) {
            return Err(Error::internal("Unsupported logical type for RemoveAlias"));
        }
        return Ok(ty.with_alias(""));
    }
    Ok(ty)
}
