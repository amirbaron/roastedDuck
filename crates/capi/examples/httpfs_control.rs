//! Signed externally; loopback TLS provider control through real SQL readers.
use rustdb::{Config, Database, Error, ErrorKind, Result, Value};

fn main() -> Result<()> {
    duckdb::register_extension_loader();
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 5 {
        return Err(Error::invalid_input(
            "expected origin, CA file, mode, fixture root, output root",
        ));
    }
    let (origin, ca, mode, fixture, out) = (&args[0], &args[1], &args[2], &args[3], &args[4]);
    let mut config = Config::default();
    config.options.push((
        "allow_unsigned_extensions".into(),
        Value::boolean(mode == "install"),
    ));
    config
        .options
        .push(("extension_directory".into(), Value::varchar(out)));
    config.options.push((
        "secret_directory".into(),
        Value::varchar(
            std::path::Path::new(out)
                .join("secrets")
                .to_string_lossy()
                .as_ref(),
        ),
    ));
    let database = Database::open("", &config)?;
    database.register_http_file_system()?;
    let mut connection = database.connect()?;
    connection.query("SET http_retries=0")?;
    if !matches!(
        mode.as_str(),
        "untrusted" | "disabled-verification" | "httplib-default" | "httplib-strict"
    ) {
        connection.query(format!("SET ca_cert_file='{}'", ca.replace('\'', "''")))?;
    }
    if mode == "disabled-verification" {
        connection.query("SET enable_curl_server_cert_verification=false")?;
    }
    if mode.starts_with("httplib-") {
        connection.query("SET httpfs_client_implementation='httplib'")?;
    }
    if mode == "httplib-strict" {
        connection.query("SET enable_server_cert_verification=true")?;
    }
    if mode == "install" {
        connection.query(format!(
            "INSTALL '{origin}/{mode}/c_only_probe.duckdb_extension.gz'"
        ))?;
        let installed = std::path::Path::new(out)
            .join("v1.5.6")
            .join(rustdb::external_extensions::platform())
            .join("c_only_probe.duckdb_extension");
        let read = |p| std::fs::read(p).map_err(|e| Error::new(ErrorKind::Io, e.to_string()));
        assert_eq!(
            read(installed)?,
            read(std::path::Path::new(fixture).join("raw-v1/c_only_probe.duckdb_extension"))?
        );
        connection.query("LOAD c_only_probe")?;
        assert_eq!(
            connection.query("SELECT c_only_add7(5)")?.value(0, 0),
            Value::bigint(12)
        );
        println!("{mode}|PASS|12");
        return Ok(());
    }
    let name = match mode.as_str() {
        "parquet" | "etag-parquet" => "data.parquet",
        "redirect" => "redirect.csv",
        "missing" => "missing.csv",
        _ => "data.csv",
    };
    let url = format!("{origin}/{mode}/{name}");
    let parquet = matches!(mode.as_str(), "parquet" | "etag-parquet");
    let sql = if parquet {
        format!("SELECT sum(i),count(s) FROM read_parquet('{url}')")
    } else {
        format!("SELECT sum(i),string_agg(s,',' ORDER BY i) FROM read_csv_auto('{url}')")
    };
    let result = connection.query(sql);
    if matches!(mode.as_str(), "untrusted" | "httplib-strict" | "missing") {
        let error = result
            .err()
            .ok_or_else(|| Error::internal("expected TLS/source error"))?;
        println!("{mode}|ERROR|{error}");
    } else {
        let rows = result?;
        assert_eq!(rows.row_count(), 1);
        let a = rows.value(0, 0).to_varchar()?;
        let b = rows.value(1, 0).to_varchar()?;
        assert_eq!(
            (a.as_str(), b.as_str()),
            if parquet {
                ("4950", "100")
            } else {
                ("3", "one,two")
            }
        );
        println!("{mode}|PASS|{a}|{b}");
    }
    if mode == "etag" {
        let second = connection.query(format!(
            "SELECT sum(i),string_agg(s,',' ORDER BY i) FROM read_csv_auto('{url}')"
        ))?;
        assert_eq!(second.value(0, 0).to_varchar()?, "7");
        println!(
            "etag-second|{}|{}",
            second.value(0, 0).to_varchar()?,
            second.value(1, 0).to_varchar()?
        );
    }
    if mode == "etag-parquet" {
        connection.query(format!(
            "SELECT octet_length(content) FROM read_blob('{origin}/switch/etag-parquet')"
        ))?;
        let second = connection.query(format!(
            "SELECT sum(i),count(s) FROM read_parquet('{url}') WHERE i>=100"
        ))?;
        assert_eq!(second.value(0, 0).to_varchar()?, "14950");
        assert_eq!(second.value(1, 0).to_varchar()?, "100");
        println!(
            "etag-parquet-second|{}|{}",
            second.value(0, 0).to_varchar()?,
            second.value(1, 0).to_varchar()?
        );
    }
    assert_eq!(connection.query("SELECT 42")?.value(0, 0), Value::integer(42));
    Ok(())
}
