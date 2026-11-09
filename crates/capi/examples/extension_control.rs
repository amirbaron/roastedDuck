//! Standalone public-C-ABI extension regression driver; payloads are signed externally.
use roastedduck::{Config, Database, Error, ErrorKind, Result, Value};

fn main() -> Result<()> {
    duckdb::register_extension_loader();
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .ok_or_else(|| Error::invalid_input("expected extension path"))?;
    let mode = args.next().unwrap_or_default();
    if mode == "matrix-install" {
        return install_matrix(
            &path,
            &args
                .next()
                .ok_or_else(|| Error::invalid_input("expected matrix output directory"))?,
        );
    }
    let install = mode.contains("install");
    let reject = mode.starts_with("reject");
    let mut config = Config::default();
    config
        .options
        .push(("allow_unsigned_extensions".to_owned(), Value::boolean(!reject)));
    if install {
        let directory = args
            .next()
            .ok_or_else(|| Error::invalid_input("expected installation directory"))?;
        config
            .options
            .push(("extension_directory".to_owned(), Value::varchar(directory)));
    }
    let load_sql = format!("LOAD '{}'", path.replace('\'', "''"));
    let sql = if install {
        format!("INSTALL '{}'", path.replace('\'', "''"))
    } else {
        load_sql.clone()
    };
    for _ in 0..2 {
        let db = Database::open("", &config)?;
        let mut connection = db.connect()?;
        if reject {
            let error = connection
                .query(&sql)
                .err()
                .ok_or_else(|| Error::internal("unsigned extension was accepted"))?;
            assert_eq!(error.kind(), ErrorKind::Io);
            assert!(error.raw_message().contains(if install {
                "doesn't have a valid signature"
            } else {
                "signature is either missing or invalid"
            }));
            continue;
        }
        connection.query(&sql)?;
        if install {
            let catalog = connection.query("SELECT loaded,installed,install_mode,extension_version FROM duckdb_extensions() WHERE extension_name='c_only_probe'")?;
            assert_eq!(catalog.row_count(), 1);
            assert_eq!(catalog.value(0, 0), Value::boolean(false));
            assert_eq!(catalog.value(1, 0), Value::boolean(true));
            assert_eq!(catalog.value(2, 0), Value::varchar("CUSTOM_PATH"));
            assert_eq!(catalog.value(3, 0), Value::varchar("0.0.1"));
            assert!(connection.query("SELECT c_only_add7(5)").is_err());
            connection.query("LOAD c_only_probe")?;
        }
        let values = connection
            .query("SELECT c_only_add7(5),c_only_add7(0),c_only_add7(-7),c_only_add7(NULL::BIGINT)")?;
        assert_eq!(values.row_count(), 1);
        assert_eq!(values.value(0, 0), Value::bigint(12));
        assert_eq!(values.value(1, 0), Value::bigint(7));
        assert_eq!(values.value(2, 0), Value::bigint(0));
        assert!(values.value(3, 0).is_null());
        assert_eq!(
            connection
                .query("SELECT sum(c_only_add7(i)) FROM range(20) t(i)")?
                .value(0, 0)
                .as_i128(),
            Some(330)
        );
        connection.query(&sql)?;
        let catalog =
            connection.query("SELECT loaded FROM duckdb_extensions() WHERE extension_name='c_only_probe'")?;
        assert_eq!(catalog.row_count(), 1);
        assert_eq!(catalog.value(0, 0), Value::boolean(true));
        drop(connection);
        let mut reopened = db.connect()?;
        assert_eq!(
            reopened.query("SELECT c_only_add7(5)")?.value(0, 0),
            Value::bigint(12)
        );
    }
    println!("C-only extension control PASS (two independent databases)");
    Ok(())
}

fn install_matrix(fixtures: &str, output: &str) -> Result<()> {
    use std::path::Path;
    let fixtures = Path::new(fixtures);
    let a = fixtures.join("raw-v1/c_only_probe.duckdb_extension");
    let b = fixtures.join("raw-v2/c_only_probe.duckdb_extension");
    let bad = fixtures.join("too-small/c_only_probe.duckdb_extension");
    let quote = |path: &Path| path.to_string_lossy().replace('\'', "''");
    let catalog = "SELECT loaded,installed,extension_version,install_mode,installed_from FROM duckdb_extensions() WHERE extension_name='c_only_probe'";
    for mask in [0o000, 0o022, 0o077] {
        let directory = Path::new(output).join(format!("raw-force-{mask:o}"));
        let mut config = Config::default();
        config
            .options
            .push(("allow_unsigned_extensions".to_owned(), Value::boolean(true)));
        config.options.push((
            "extension_directory".to_owned(),
            Value::varchar(directory.to_string_lossy()),
        ));
        // SAFETY: standalone serial diagnostic only, restores original mask before return.
        let original = unsafe { libc::umask(mask) };
        let result = (|| -> Result<()> {
            let db = Database::open("", &config)?;
            let mut con = db.connect()?;
            con.query(format!("INSTALL '{}'", quote(&a)))?;
            let target = directory.join(format!(
                "v1.5.6/{}/c_only_probe.duckdb_extension",
                roastedduck::external_extensions::platform()
            ));
            let info = std::path::PathBuf::from(format!("{}.info", target.display()));
            let initial = std::fs::read(&target).map_err(|e| Error::new(ErrorKind::Io, e.to_string()))?;
            let initial_info = std::fs::read(&info).map_err(|e| Error::new(ErrorKind::Io, e.to_string()))?;
            con.query(format!("INSTALL '{}'", quote(&b)))?;
            assert_eq!(
                std::fs::read(&target).map_err(|e| Error::new(ErrorKind::Io, e.to_string()))?,
                initial
            );
            assert_eq!(
                std::fs::read(&info).map_err(|e| Error::new(ErrorKind::Io, e.to_string()))?,
                initial_info
            );
            con.query(format!("FORCE INSTALL '{}'", quote(&b)))?;
            assert_eq!(con.query(catalog)?.value(2, 0), Value::varchar("0.0.2"));
            let replaced = std::fs::read(&target).map_err(|e| Error::new(ErrorKind::Io, e.to_string()))?;
            let replaced_info = std::fs::read(&info).map_err(|e| Error::new(ErrorKind::Io, e.to_string()))?;
            assert_eq!(
                replaced,
                std::fs::read(&b).map_err(|e| Error::new(ErrorKind::Io, e.to_string()))?
            );
            assert!(con.query(format!("FORCE INSTALL '{}'", quote(&bad))).is_err());
            assert_eq!(
                std::fs::read(&target).map_err(|e| Error::new(ErrorKind::Io, e.to_string()))?,
                replaced
            );
            assert_eq!(
                std::fs::read(&info).map_err(|e| Error::new(ErrorKind::Io, e.to_string()))?,
                replaced_info
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = |metadata: std::fs::Metadata| u64::from(metadata.permissions().mode() & 0o777);
                let expected = 0o666_u64 & !u64::from(mask);
                assert_eq!(
                    mode(std::fs::metadata(&target).map_err(|e| Error::new(ErrorKind::Io, e.to_string()))?),
                    expected
                );
                assert_eq!(
                    mode(std::fs::metadata(&info).map_err(|e| Error::new(ErrorKind::Io, e.to_string()))?),
                    expected
                );
            }
            con.query("LOAD c_only_probe")?;
            assert_eq!(con.query("SELECT c_only_add7(5)")?.value(0, 0), Value::bigint(12));
            println!("raw-force mask={mask:o} PASS");
            Ok(())
        })();
        // SAFETY: restore the process mask after the serial case, including errors.
        unsafe {
            libc::umask(original);
        }
        result?;
    }
    for (kind, source) in [
        ("gzip", fixtures.join("gzip-v1/c_only_probe.duckdb_extension.gz")),
        (
            "gzip-fallback",
            fixtures.join("raw-v1/c_only_probe.duckdb_extension.gz"),
        ),
    ] {
        let directory = Path::new(output).join(kind);
        let mut config = Config::default();
        config
            .options
            .push(("allow_unsigned_extensions".to_owned(), Value::boolean(true)));
        config.options.push((
            "extension_directory".to_owned(),
            Value::varchar(directory.to_string_lossy()),
        ));
        let db = Database::open("", &config)?;
        let mut con = db.connect()?;
        con.query(format!("INSTALL '{}'", quote(&source)))?;
        assert_eq!(con.query(catalog)?.value(2, 0), Value::varchar("0.0.1"));
        assert_eq!(
            con.query(catalog)?.value(4, 0),
            Value::varchar(if kind == "gzip" {
                source.to_string_lossy()
            } else {
                a.to_string_lossy()
            })
        );
        con.query("LOAD c_only_probe")?;
        assert_eq!(con.query("SELECT c_only_add7(5)")?.value(0, 0), Value::bigint(12));
        println!("{kind} PASS");
    }
    Ok(())
}
