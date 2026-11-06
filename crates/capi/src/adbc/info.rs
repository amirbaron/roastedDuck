//! `GetInfo`'s prescribed dense-union schema and values.
use super::*;
use batch::schema;
use manual::{array, list, primitive, struct_array, varchar};
fn leaf(format: &[u8], name: &[u8]) -> ArrowSchema {
    schema(format, Some(name), 2, Vec::new())
}
fn info_schema() -> ArrowSchema {
    let map_values = schema(b"+l", Some(b"value"), 2, vec![leaf(b"i", b"item")]);
    let map_entries = schema(
        b"+s",
        Some(b"entries"),
        0,
        vec![schema(b"i", Some(b"key"), 0, Vec::new()), map_values],
    );
    let value = schema(
        b"+ud:0,1,2,3,4,5",
        Some(b"info_value"),
        2,
        vec![
            leaf(b"u", b"string_value"),
            leaf(b"b", b"bool_value"),
            leaf(b"l", b"int64_value"),
            leaf(b"i", b"int32_bitmask"),
            schema(b"+l", Some(b"string_list"), 2, vec![leaf(b"u", b"item")]),
            schema(b"+m", Some(b"int32_to_int32_list_map"), 2, vec![map_entries]),
        ],
    );
    schema(
        b"+s",
        None,
        2,
        vec![schema(b"I", Some(b"info_name"), 0, Vec::new()), value],
    )
}
pub(super) unsafe extern "C" fn get_info(
    connection: *mut AdbcConnection,
    codes: *const u32,
    count: usize,
    out: *mut ArrowArrayStream,
    error: *mut AdbcError,
) -> u8 {
    // SAFETY: requested codes form a count-entry array; output is a pure writable Arrow stream slot.
    unsafe {
        if connection.is_null() {
            return fail(error, INVALID_ARGUMENT, b"Missing connection object");
        }
        let Some(state) = super::connection(connection) else {
            return fail(error, INVALID_ARGUMENT, b"Connection is invalid");
        };
        if out.is_null() {
            return fail(error, INVALID_ARGUMENT, b"Output parameter was not provided");
        }
        if state.handle().is_null() {
            return fail(error, INVALID_STATE, b"Connection is not initialized");
        }
        let requested = if codes.is_null() {
            &[0, 1, 100, 101, 102, 103][..]
        } else {
            std::slice::from_raw_parts(codes, count)
        };
        let version = opt_bytes(duckdb_library_version()).unwrap_or_default();
        let mut names = Vec::new();
        let mut ids = Vec::new();
        let mut offsets = Vec::new();
        let mut strings: Vec<&[u8]> = Vec::new();
        let mut numbers = Vec::new();
        for code in requested {
            let text: Option<&[u8]> = match *code {
                0 => Some(b"duckdb".as_slice()),
                1 | 101 => Some(version),
                100 => Some(b"ADBC DuckDB Driver"),
                102 => Some(b"(unknown)"),
                103 => None,
                _ => continue,
            };
            names.extend_from_slice(&code.to_ne_bytes());
            if let Some(text) = text {
                ids.push(0);
                offsets.extend_from_slice(&(strings.len() as i32).to_ne_bytes());
                strings.push(text);
            } else {
                ids.push(2);
                offsets.extend_from_slice(&(numbers.len() as i32).to_ne_bytes());
                numbers.push(1_001_000_i64);
            }
        }
        let empty_map = list(
            &[0],
            struct_array(0, vec![primitive(0, None), list(&[0], primitive(0, None))]),
        );
        let union = array(
            ids.len(),
            0,
            vec![Some(ids.clone()), Some(offsets)],
            vec![
                varchar(&strings),
                primitive(0, None),
                primitive(
                    numbers.len(),
                    (!numbers.is_empty()).then(|| numbers.iter().flat_map(|v| v.to_ne_bytes()).collect()),
                ),
                primitive(0, None),
                list(&[0], varchar(&[])),
                empty_map,
            ],
        );
        let mut array = struct_array(ids.len(), vec![primitive(ids.len(), Some(names)), union]);
        let mut schema = info_schema();
        *out = ArrowArrayStream::empty();
        batch::from_batch(&raw mut array, &raw mut schema, out, error)
    }
}
