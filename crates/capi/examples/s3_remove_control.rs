//! Authenticated externally; SDK-only filesystem removal controls through SQL.
use roastedduck::{Config, Database, Error, LogicalType, Result, Value};
use roastedduck_function::{ScalarFunction, ScalarFunctionSet, Stability};
use std::fmt::Write as _;

fn quote(value: &str) -> String {
    let mut output = String::from("\"");
    for c in value.chars() {
        match c {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            c if c < ' ' => {
                let _ = write!(output, "\\u{:04x}", u32::from(c));
            }
            c => output.push(c),
        }
    }
    output.push('"');
    output
}

fn main() -> Result<()> {
    duckdb::register_extension_loader();
    let args: Vec<_> = std::env::args().skip(1).collect();
    let [endpoint, package, out] = args.as_slice() else {
        return Err(Error::invalid_input("expected endpoint, package and output"));
    };
    let mut config = Config::default();
    for name in [
        "allow_unsigned_extensions",
        "allow_community_extensions",
        "autoinstall_known_extensions",
        "autoload_known_extensions",
    ] {
        config.options.push((name.into(), Value::boolean(false)));
    }
    config.options.push((
        "secret_directory".into(),
        Value::varchar(
            std::path::Path::new(out)
                .with_extension("secrets")
                .to_string_lossy()
                .as_ref(),
        ),
    ));
    let db = Database::open("", &config)?;
    let mut con = db.connect()?;
    let mut function = ScalarFunction::new(
        "probe_remove",
        vec![LogicalType::VARCHAR],
        LogicalType::BOOLEAN,
        |input, _, output| {
            *output = roastedduck_types::Vector::flat(LogicalType::BOOLEAN, input.len().max(1));
            for row in 0..input.len() {
                let path = input.column(0).get_value(row).to_varchar_bytes()?;
                let fs = roastedduck_function::registered_files::lookup(&path)
                    .ok_or_else(|| Error::internal("S3 filesystem not registered"))?;
                fs.remove(&path)?;
                output.set_value(row, &Value::boolean(true))?;
            }
            Ok(())
        },
    );
    function.stability = Stability::Volatile;
    let mut set = ScalarFunctionSet::new("probe_remove");
    set.add(function);
    con.register_scalar_function_set(set)?;
    let sqls = [
        ("load", format!("LOAD '{}'", package.replace('\'', "''")), true),
        (
            "secret",
            format!(
                "CREATE SECRET credentials (TYPE s3, KEY_ID 'fixture-access', SECRET 'fixture-secret', REGION 'fixture-region', ENDPOINT '{endpoint}', URL_STYLE 'path', USE_SSL false)"
            ),
            true,
        ),
        ("part-size", "SET s3_uploader_max_filesize='50MB'".into(), true),
        (
            "create",
            "COPY (SELECT 1 i) TO 's3://bucket/delete-single.csv' (FORMAT CSV)".into(),
            true,
        ),
        (
            "remove",
            "SELECT probe_remove('s3://bucket/delete-single.csv')".into(),
            true,
        ),
        (
            "missing",
            "SELECT probe_remove('s3://bucket/delete-single.csv')".into(),
            false,
        ),
        (
            "create-denied",
            "COPY (SELECT 2 i) TO 's3://bucket/deny-single.csv' (FORMAT CSV)".into(),
            true,
        ),
        (
            "denied",
            "SELECT probe_remove('s3://bucket/deny-single.csv')".into(),
            false,
        ),
        ("disable", "SET disabled_filesystems='S3FileSystem'".into(), true),
        (
            "disabled",
            "SELECT probe_remove('s3://bucket/deny-single.csv')".into(),
            false,
        ),
        ("database-valid", "SELECT 42".into(), true),
    ];
    let mut records = Vec::new();
    for (label, sql, success) in sqls {
        let record = match con.query(&sql) {
            Ok(result) => {
                if !success {
                    return Err(Error::internal(format!("unexpected SDK success: {label}")));
                }
                let mut rows = Vec::new();
                for row in 0..result.row_count() {
                    let mut values = Vec::new();
                    for col in 0..result.column_count() {
                        values.push(quote(&result.value(col, row).to_varchar()?));
                    }
                    rows.push(format!("[{}]", values.join(",")));
                }
                format!(
                    "{{\"label\":{},\"sql\":{},\"status\":0,\"error\":null,\"rows\":[{}]}}",
                    quote(label),
                    quote(&sql),
                    rows.join(",")
                )
            }
            Err(error) => {
                if success {
                    return Err(error);
                }
                format!(
                    "{{\"label\":{},\"sql\":{},\"status\":1,\"error\":{},\"rows\":[]}}",
                    quote(label),
                    quote(&sql),
                    quote(&error.to_string())
                )
            }
        };
        records.push(record);
    }
    std::fs::write(out, format!("[{}]\n", records.join(",")))
        .map_err(|e| Error::invalid_input(e.to_string()))?;
    Ok(())
}
