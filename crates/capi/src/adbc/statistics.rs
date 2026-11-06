//! Table row-count estimates in the dense-union layout required by ADBC 1.1.
use super::*;
use batch::schema;
use manual::{array, list, null_varchar, primitive, struct_array, varchar};
struct Catalog {
    name: ByteString,
    schemas: Vec<Schema>,
}
struct Schema {
    name: ByteString,
    tables: Vec<(ByteString, u64)>,
}
fn field(format: &[u8], name: &[u8], nullable: bool, children: Vec<ArrowSchema>) -> ArrowSchema {
    schema(format, Some(name), if nullable { 2 } else { 0 }, children)
}
fn result_schema() -> ArrowSchema {
    let value = field(
        b"+ud:0,1,2,3",
        b"statistic_value",
        false,
        vec![
            field(b"l", b"int64", true, Vec::new()),
            field(b"L", b"uint64", true, Vec::new()),
            field(b"g", b"float64", true, Vec::new()),
            field(b"z", b"binary", true, Vec::new()),
        ],
    );
    let statistic = field(
        b"+s",
        b"item",
        true,
        vec![
            field(b"u", b"table_name", false, Vec::new()),
            field(b"u", b"column_name", true, Vec::new()),
            field(b"s", b"statistic_key", false, Vec::new()),
            value,
            field(b"b", b"statistic_is_approximate", false, Vec::new()),
        ],
    );
    let db_schema = field(
        b"+s",
        b"item",
        true,
        vec![
            field(b"u", b"db_schema_name", true, Vec::new()),
            field(b"+l", b"db_schema_statistics", false, vec![statistic]),
        ],
    );
    schema(
        b"+s",
        None,
        2,
        vec![
            field(b"u", b"catalog_name", true, Vec::new()),
            field(b"+l", b"catalog_db_schemas", false, vec![db_schema]),
        ],
    )
}
fn result_array(catalogs: &[Catalog]) -> ArrowArray {
    let mut catalog_names = Vec::new();
    let mut schema_names = Vec::new();
    let mut table_names = Vec::new();
    let mut counts = Vec::new();
    let mut schema_offsets = vec![0i32];
    let mut table_offsets = vec![0i32];
    for catalog in catalogs {
        catalog_names.push(catalog.name.as_bytes());
        for schema in &catalog.schemas {
            schema_names.push(schema.name.as_bytes());
            for (table, count) in &schema.tables {
                table_names.push(table.as_bytes());
                counts.push(*count as f64);
            }
            table_offsets.push(table_names.len() as i32);
        }
        schema_offsets.push(schema_names.len() as i32);
    }
    let n = table_names.len();
    let union = array(
        n,
        0,
        vec![
            Some(vec![2; n]),
            Some((0..n).flat_map(|v| (v as i32).to_ne_bytes()).collect()),
        ],
        vec![
            primitive(0, None),
            primitive(0, None),
            primitive(n, Some(counts.iter().flat_map(|v| v.to_ne_bytes()).collect())),
            array(
                0,
                0,
                vec![None, Some(0i32.to_ne_bytes().to_vec()), None],
                Vec::new(),
            ),
        ],
    );
    let stats = struct_array(
        n,
        vec![
            varchar(&table_names),
            null_varchar(n),
            primitive(n, Some((0..n).flat_map(|_| 6i16.to_ne_bytes()).collect())),
            union,
            primitive(n, Some(vec![255; n.div_ceil(8)])),
        ],
    );
    let schemas = struct_array(
        schema_names.len(),
        vec![varchar(&schema_names), list(&table_offsets, stats)],
    );
    struct_array(
        catalogs.len(),
        vec![varchar(&catalog_names), list(&schema_offsets, schemas)],
    )
}
pub(super) unsafe extern "C" fn get_statistics(
    connection: *mut AdbcConnection,
    catalog: *const c_char,
    schema_name: *const c_char,
    table: *const c_char,
    _approximate: c_char,
    out: *mut ArrowArrayStream,
    error: *mut AdbcError,
) -> u8 {
    // SAFETY: query/result/string handles are owned locally; catalog groups retain copies before each result is destroyed.
    unsafe {
        let Some(state) = super::connection(connection) else {
            return fail(error, INVALID_ARGUMENT, b"Connection is invalid");
        };
        if out.is_null() {
            return fail(error, INVALID_ARGUMENT, b"Output parameter was not provided");
        }
        if state.handle().is_null() {
            return fail(error, INVALID_STATE, b"Connection is not initialized");
        }
        state.materialize();
        let filters = [
            metadata::filter(opt_bytes(catalog)),
            metadata::filter(opt_bytes(schema_name)),
            metadata::filter(opt_bytes(table)),
        ];
        let sql = metadata::format_template(
            include_bytes!("sql/statistics.sql"),
            &[&filters[0], &filters[1], &filters[2]],
        );
        let Some(sql) = sql else {
            return fail(error, INTERNAL, b"Inconsistent statistics SQL template");
        };
        let sql = to_cstring(sql);
        let mut result = empty_result();
        if duckdb_query(state.handle(), sql.as_ptr(), &raw mut result) != DuckdbState::DuckDBSuccess {
            let code = fail(
                error,
                INTERNAL,
                opt_bytes(duckdb_result_error(&raw mut result)).unwrap_or_default(),
            );
            duckdb_destroy_result(&raw mut result);
            return code;
        }
        let mut catalogs: Vec<Catalog> = Vec::new();
        for row in 0..duckdb_row_count(&raw mut result) {
            if duckdb_value_is_null(&raw mut result, 3, row) {
                continue;
            }
            let count = duckdb_value_int64(&raw mut result, 3, row);
            if count < 0 {
                continue;
            }
            let mut names = Vec::new();
            for column in 0..3 {
                let value = duckdb_value_varchar(&raw mut result, column, row);
                names.push(ByteString::from(opt_bytes(value).unwrap_or_default()));
                duckdb_free(value.cast());
            }
            if catalogs.last().is_none_or(|c| c.name != names[0]) {
                catalogs.push(Catalog {
                    name: names[0].clone(),
                    schemas: Vec::new(),
                });
            }
            let last = catalogs.len() - 1;
            let group = &mut catalogs[last];
            if group.schemas.last().is_none_or(|s| s.name != names[1]) {
                group.schemas.push(Schema {
                    name: names[1].clone(),
                    tables: Vec::new(),
                });
            }
            let last = group.schemas.len() - 1;
            group.schemas[last].tables.push((names[2].clone(), count as u64));
        }
        duckdb_destroy_result(&raw mut result);
        let mut schema = result_schema();
        let mut array = result_array(&catalogs);
        *out = ArrowArrayStream::empty();
        batch::from_batch(&raw mut array, &raw mut schema, out, error)
    }
}
pub(super) unsafe extern "C" fn get_names(
    connection: *mut AdbcConnection,
    out: *mut ArrowArrayStream,
    error: *mut AdbcError,
) -> u8 {
    // SAFETY: output is a pure writable slot; the driver has no private statistic names.
    unsafe {
        if super::connection(connection).is_none() {
            return fail(error, INVALID_ARGUMENT, b"Connection is invalid");
        }
        if out.is_null() {
            return fail(error, INVALID_ARGUMENT, b"Output parameter was not provided");
        }
        let mut schema = schema(
            b"+s",
            None,
            2,
            vec![
                field(b"u", b"statistic_name", false, Vec::new()),
                field(b"s", b"statistic_key", false, Vec::new()),
            ],
        );
        let mut array = struct_array(0, vec![varchar(&[]), primitive(0, None)]);
        *out = ArrowArrayStream::empty();
        batch::from_batch(&raw mut array, &raw mut schema, out, error)
    }
}
