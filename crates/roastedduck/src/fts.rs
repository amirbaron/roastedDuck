//! roastedDuck's compatible implementation of duckdb-fts 6814ec9 (DuckDB 1.5.6).
//! The index consists of ordinary SQL tables/macros, not a storage index.

use roastedduck_catalog::{CatalogType, EntryKind};
use roastedduck_common::{ByteString, Error, Result, byte_format};
use roastedduck_function::{
    FunctionState, PragmaFunction, PragmaFunctionSet, ScalarFunction, ScalarFunctionSet,
};
use roastedduck_functions::scalar::executor::{Str, StrOut, binary};
use roastedduck_planner::{BoundPragmaInfo, ClientContext};
use roastedduck_types::{DataChunk, LogicalType, Value, Vector};

pub(crate) fn is_pragma(name: &[u8]) -> bool {
    name.eq_ignore_ascii_case(b"create_fts_index") || name.eq_ignore_ascii_case(b"drop_fts_index")
}

pub(crate) fn pragma_functions() -> Vec<PragmaFunctionSet> {
    let mut create = PragmaFunction::call("create_fts_index", vec![LogicalType::VARCHAR; 2]).with_query();
    create.varargs = Some(LogicalType::VARCHAR);
    create.named_parameters = vec![
        ("stemmer".into(), LogicalType::VARCHAR),
        ("stopwords".into(), LogicalType::VARCHAR),
        ("ignore".into(), LogicalType::VARCHAR),
        ("strip_accents".into(), LogicalType::BOOLEAN),
        ("lower".into(), LogicalType::BOOLEAN),
        ("overwrite".into(), LogicalType::BOOLEAN),
    ];
    vec![
        PragmaFunctionSet::single(create),
        PragmaFunctionSet::single(
            PragmaFunction::call("drop_fts_index", vec![LogicalType::VARCHAR]).with_query(),
        ),
    ]
}

fn stem(args: &DataChunk, _: &mut FunctionState, result: &mut Vector) -> Result<()> {
    binary::<Str, Str, StrOut>(args, result, |input, algorithm, out| {
        out.extend_from_slice(&roastedduck_snowball::stem(input, algorithm)?);
        Ok(true)
    })
}

pub(crate) fn stem_function() -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new("stem");
    set.functions.push(ScalarFunction::new(
        "stem",
        vec![LogicalType::VARCHAR; 2],
        LogicalType::VARCHAR,
        stem,
    ));
    set
}

struct QualifiedName {
    catalog: ByteString,
    schema: ByteString,
    name: ByteString,
}

impl QualifiedName {
    fn parse(context: &ClientContext, value: &Value) -> Result<Self> {
        if value.is_null() {
            return Err(Error::internal("Calling StringValue::Get on a NULL value"));
        }
        let (catalog, schema, name) = crate::sequence::parse_qualified_name(&value.to_varchar()?)?;
        let catalog = ByteString::from(catalog);
        let schema = if schema.is_empty() {
            // An omitted catalog uses the current search path, rather than main.
            if catalog.is_empty() {
                context.default_schema().into()
            } else {
                context.default_schema_for(&catalog)
            }
        } else {
            schema.into()
        };
        Ok(Self {
            catalog,
            schema,
            name: name.into(),
        })
    }

    fn fts_schema(&self) -> ByteString {
        byte_format!("fts_{}_{}", self.schema, self.name)
    }

    fn qualified(&self, name: &[u8]) -> ByteString {
        if self.catalog.is_empty() {
            name.into()
        } else {
            byte_format!("{}.{name}", self.catalog)
        }
    }

    fn table(&self, context: &ClientContext) -> Result<roastedduck_planner::context::EntryLookup> {
        context
            .get_entry(&self.catalog, &self.schema, CatalogType::Table, &self.name, false)?
            .ok_or_else(|| Error::internal("FTS table lookup returned no entry"))
    }
}

fn string_value(value: &Value) -> Result<ByteString> {
    if value.is_null() {
        return Err(Error::internal("Calling StringValue::Get on a NULL value"));
    }
    value.to_varchar_bytes()
}

fn replace(input: &[u8], needle: &[u8], value: &[u8]) -> ByteString {
    let mut out = Vec::new();
    let mut remaining = input;
    while let Some(pos) = remaining
        .windows(needle.len())
        .position(|window| window == needle)
    {
        out.extend_from_slice(&remaining[..pos]);
        out.extend_from_slice(value);
        remaining = &remaining[pos + needle.len()..];
    }
    out.extend_from_slice(remaining);
    out.into()
}

pub(crate) fn pragma_query(
    context: &ClientContext,
    name: &[u8],
    bound: &BoundPragmaInfo,
) -> Result<ByteString> {
    let qname = QualifiedName::parse(context, &bound.parameters[0])?;
    let fts_schema = qname.fts_schema();
    // Pinned FTS passes the catalog-qualified spelling back to schema lookup.
    // Preserve its qualified-name quirk (duplicate create rebuilds; drop errors).
    let lookup_schema = qname.qualified(&fts_schema);
    let exists = context
        .get_entry(&qname.catalog, b"", CatalogType::Schema, &lookup_schema, true)?
        .is_some();
    if name.eq_ignore_ascii_case(b"drop_fts_index") {
        if !exists {
            return Err(Error::catalog(byte_format!(
                "a FTS index does not exist on table '{}.{}'. Create one with 'PRAGMA create_fts_index()'.",
                qname.schema,
                qname.name
            )));
        }
        return Ok(byte_format!(
            "DROP SCHEMA {} CASCADE;",
            qname.qualified(&fts_schema)
        ));
    }
    let found = qname.table(context)?;
    let EntryKind::Table(table) = &found.entry.kind else {
        return Err(Error::catalog("FTS requires a table"));
    };
    let option = |name: &[u8]| {
        bound
            .named_parameters
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value)
    };
    let string = |name: &[u8], default: &[u8]| -> Result<ByteString> {
        option(name).map_or_else(|| Ok(default.into()), string_value)
    };
    let boolean =
        |name: &[u8], default: bool| option(name).map_or(default, |value| value.as_bool().unwrap_or(false));
    let stemmer = string(b"stemmer", b"porter")?;
    let stopwords = string(b"stopwords", b"english")?;
    if stopwords.as_bytes() != b"english" && stopwords.as_bytes() != b"none" {
        QualifiedName::parse(context, &Value::try_varchar(&stopwords)?)?.table(context)?;
    }
    let ignore = string(b"ignore", br#"[0-9!@#$%^&*()_+={}\[\]:;<>,.?~\\/\|''"`-]+"#)?;
    if exists && !boolean(b"overwrite", false) {
        return Err(Error::catalog(byte_format!(
            "a FTS index already exists on table '{}.{}'. Supply 'overwrite=1' to overwrite, or drop the existing index with 'PRAGMA drop_fts_index()' before creating a new one.",
            qname.schema,
            qname.name
        )));
    }
    let id = string_value(&bound.parameters[1])?;
    let mut columns = Vec::new();
    for value in &bound.parameters[2..] {
        let column = string_value(value)?;
        if column.as_bytes() == b"*" {
            columns = table
                .columns
                .iter()
                .filter(|column| column.logical_type == LogicalType::VARCHAR)
                .map(|column| column.name.clone())
                .collect();
            break;
        }
        if table.column(&column).is_none() {
            return Err(Error::catalog(byte_format!(
                "Table '{}.{}' does not have a column named '{column}'!",
                qname.schema,
                qname.name
            )));
        }
        columns.push(column);
    }
    if columns.is_empty() {
        return Err(Error::invalid_input(
            "at least one column must be supplied for indexing!",
        ));
    }
    let mut sql = ByteString::from(
        "DROP SCHEMA IF EXISTS %fts_schema% CASCADE; CREATE SCHEMA %fts_schema%; CREATE TABLE %fts_schema%.stopwords (sw VARCHAR);",
    );
    if stopwords.as_bytes() == b"english" {
        sql.push_bytes(include_bytes!("fts/stopwords.sql"));
    } else if stopwords.as_bytes() != b"none" {
        sql.push_bytes(&byte_format!(
            "INSERT INTO %fts_schema%.stopwords SELECT * FROM {stopwords};"
        ));
    }
    let mut tokenize = ByteString::from("s::VARCHAR");
    if boolean(b"strip_accents", true) {
        tokenize = byte_format!("strip_accents({tokenize})");
    }
    if boolean(b"lower", true) {
        tokenize = byte_format!("lower({tokenize})");
    }
    tokenize = byte_format!("string_split_regex(regexp_replace({tokenize}, $${ignore}$$, ' ', 'g'), '\\s+')");
    sql.push_bytes(&byte_format!(
        "CREATE MACRO %fts_schema%.tokenize(s) AS {tokenize};"
    ));
    sql.push_bytes(include_bytes!("fts/index.sql"));
    let fields: Vec<_> = columns
        .iter()
        .enumerate()
        .map(|(index, column)| byte_format!("({index}, '{column}')"))
        .collect();
    let queries: Vec<_> = columns.iter().map(|column| byte_format!("SELECT unnest(%fts_schema%.tokenize(fts_ii.\"{column}\")) AS w, rowid AS docid, (SELECT fieldid FROM %fts_schema%.fields WHERE field = '{column}') AS fieldid FROM %input_table% AS fts_ii")).collect();
    for (key, value) in [
        (b"%field_values%".as_slice(), ByteString::join(&fields, b", ")),
        (
            b"%union_fields_query%",
            ByteString::join(&queries, b" UNION ALL "),
        ),
        (b"%fts_schema%", qname.qualified(&fts_schema)),
        (
            b"%input_table%",
            qname.qualified(&byte_format!("{}.{}", qname.schema, qname.name)),
        ),
        (b"%input_id%", id),
        (b"%stemmer%", stemmer),
    ] {
        sql = replace(&sql, key, &value);
    }
    Ok(sql)
}
