//! Registered-filesystem INSTALL regression; authenticate this driver and fixtures externally.
use std::sync::Arc;

use rustdb::{Config, Database, Error, ErrorKind, Result, Value};
use rustdb_function::registered_files::{FileHandle, FileSystem, Registry};

#[derive(Debug)]
struct Source {
    raw: Arc<[u8]>,
    gzip: Arc<[u8]>,
}

#[derive(Debug)]
struct Handle {
    data: Arc<[u8]>,
    fail: bool,
}

impl FileHandle for Handle {
    fn read_at(&self, offset: u64, buffer: &mut [u8]) -> Result<usize> {
        if self.fail {
            return Err(Error::new(ErrorKind::Io, "registered extension read failed"));
        }
        let start = usize::try_from(offset).map_err(|_| Error::invalid_input("invalid offset"))?;
        let remaining = self.data.get(start..).unwrap_or_default();
        let count = buffer.len().min(remaining.len());
        buffer[..count].copy_from_slice(&remaining[..count]);
        Ok(count)
    }
    fn write_at(&self, _: u64, _: &[u8]) -> Result<usize> {
        Err(Error::not_implemented("read-only fixture"))
    }
    fn size(&self) -> Result<u64> {
        Ok(self.data.len() as u64)
    }
    fn modified(&self) -> Result<i64> {
        Ok(0)
    }
    fn flush(&self) -> Result<()> {
        Ok(())
    }
}

impl FileSystem for Source {
    fn name(&self) -> &'static str {
        "RegisteredExtensionSource"
    }
    fn handles(&self, path: &[u8]) -> bool {
        path.starts_with(b"https://") || path.starts_with(b"regext://")
    }
    fn open(&self, path: &[u8], write: bool) -> Result<Arc<dyn FileHandle>> {
        assert!(!write);
        Ok(Arc::new(Handle {
            data: if path.ends_with(b".gz") {
                Arc::clone(&self.gzip)
            } else {
                Arc::clone(&self.raw)
            },
            fail: path.starts_with(b"https://error/"),
        }))
    }
    fn glob(&self, path: &[u8]) -> Result<Vec<rustdb_common::ByteString>> {
        Ok(vec![path.into()])
    }
    fn is_file(&self, path: &[u8]) -> Result<bool> {
        Ok(!(path.starts_with(b"https://missing/")
            || path.starts_with(b"https://fallback/") && path.ends_with(b".gz")))
    }
    fn is_dir(&self, _: &[u8]) -> Result<bool> {
        Ok(false)
    }
    fn mkdir(&self, _: &[u8]) -> Result<()> {
        Err(Error::not_implemented("read-only fixture"))
    }
    fn remove(&self, _: &[u8]) -> Result<()> {
        Err(Error::not_implemented("read-only fixture"))
    }
    fn rename(&self, _: &[u8], _: &[u8]) -> Result<()> {
        Err(Error::not_implemented("read-only fixture"))
    }
}

fn main() -> Result<()> {
    duckdb::register_extension_loader();
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 2 {
        return Err(Error::invalid_input("expected fixture root and output root"));
    }
    let fixture = std::path::Path::new(&args[0]);
    let out = std::path::Path::new(&args[1]);
    let read = |path| std::fs::read(path).map_err(|error| Error::new(ErrorKind::Io, error.to_string()));
    let source = Arc::new(Source {
        raw: read(fixture.join("raw-v1/c_only_probe.duckdb_extension"))?.into(),
        gzip: read(fixture.join("gzip-v1/c_only_probe.duckdb_extension.gz"))?.into(),
    });
    for kind in [
        "raw", "gzip", "fallback", "missing", "error", "disabled", "unknown",
    ] {
        let directory = out.join(kind);
        let mut config = Config::default();
        config
            .options
            .push(("allow_unsigned_extensions".into(), Value::boolean(true)));
        config.options.push((
            "extension_directory".into(),
            Value::varchar(directory.to_string_lossy().as_ref()),
        ));
        config
            .options
            .push(("autoload_known_extensions".into(), Value::boolean(false)));
        config
            .options
            .push(("autoinstall_known_extensions".into(), Value::boolean(false)));
        let db = Database::open("", &config)?;
        let mut con = db.connect()?;
        let cache = con
            .function_client()
            .object_cache()
            .ok_or_else(|| Error::internal("missing registry"))?;
        Registry::initialize(&cache).register(Arc::clone(&source) as Arc<dyn FileSystem>)?;
        if kind == "disabled" {
            con.query("SET disabled_filesystems='RegisteredExtensionSource'")?;
        }
        let suffix = if matches!(kind, "gzip" | "fallback") {
            ".gz"
        } else {
            ""
        };
        let scheme = if kind == "unknown" { "regext" } else { "https" };
        let url = format!("{scheme}://{kind}/c_only_probe.duckdb_extension{suffix}");
        let result = con.query(format!("INSTALL '{url}'"));
        if matches!(kind, "missing" | "error" | "disabled" | "unknown") {
            let error = result
                .err()
                .ok_or_else(|| Error::internal("expected source error"))?;
            println!("{kind}|ERROR|{}", error.raw_message());
        } else {
            result?;
            let platform = rustdb::external_extensions::platform();
            let installed = directory
                .join("v1.5.6")
                .join(platform)
                .join("c_only_probe.duckdb_extension");
            assert_eq!(read(installed)?, source.raw.as_ref());
            con.query("LOAD c_only_probe")?;
            assert_eq!(con.query("SELECT c_only_add7(5)")?.value(0, 0), Value::bigint(12));
            println!("{kind}|PASS|12");
        }
    }
    Ok(())
}
