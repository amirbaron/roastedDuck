//! DuckDB's `C_STRUCT` extension initialization through the existing public C API.
use std::collections::BTreeMap;
use std::ffi::{CStr, CString, c_char, c_void};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use libloading::Library;
use rsa::{Pkcs1v15Sign, PublicKey, pkcs8::DecodePublicKey};
use rustdb::{Database, Error, ErrorKind, Result};
use rustdb_common::{ByteString, byte_format};
use rustdb_function::cast::ValueCast;
use sha2::{Digest, Sha256};

use crate::extension_api::EXTENSION_API_V1;
use crate::open::{DatabaseWrapper, DuckdbDatabase};

#[derive(Debug)]
struct Metadata {
    magic: Vec<u8>,
    platform: Vec<u8>,
    api_version: Vec<u8>,
    version: ByteString,
    abi: Vec<u8>,
}

fn io_error(error: &std::io::Error) -> Error {
    Error::new(ErrorKind::Io, error.to_string())
}
fn setting(db: &Database, name: &str) -> bool {
    db.global_setting(name)
        .or_else(|| {
            db.config()
                .options
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(name))
                .map(|(_, value)| value.clone())
        })
        .and_then(|value| value.default_cast_as(&rustdb::LogicalType::BOOLEAN).ok())
        .and_then(|value| value.as_bool())
        .unwrap_or(name == "allow_community_extensions")
}
fn semver(version: &[u8]) -> Option<(u64, u64, u64)> {
    let text = std::str::from_utf8(version).ok()?.strip_prefix('v')?;
    let parts: Vec<_> = text.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    Some((
        parts[0].parse().ok()?,
        parts[1].parse().ok()?,
        parts[2].parse().ok()?,
    ))
}
fn supported(version: &[u8]) -> bool {
    semver(version).is_some_and(|(major, minor, patch)| major == 1 && (minor, patch) <= (5, 6))
}
fn pretty(bytes: &[u8]) -> String {
    let mut result = String::new();
    for &b in bytes {
        if b.is_ascii_alphanumeric() || b"_- .".contains(&b) {
            result.push(char::from(b));
        } else {
            result.push_str("\\x");
            result.push(char::from(b"0123456789abcdef"[usize::from(b >> 4)]));
            result.push(char::from(b"0123456789abcdef"[usize::from(b & 15)]));
        }
    }
    result
}
fn metadata(file: &mut File, filename: &[u8]) -> Result<Metadata> {
    if file.metadata().map_err(|error| io_error(&error))?.len() < 512 {
        return Err(Error::invalid_input(byte_format!(
            "File '{filename}' is not a DuckDB extension. Valid DuckDB extensions must be at least 512 bytes"
        )));
    }
    file.seek(SeekFrom::End(-512)).map_err(|error| io_error(&error))?;
    let mut footer = [0; 512];
    file.read_exact(&mut footer).map_err(|error| io_error(&error))?;
    let field = |index: usize| {
        let value = &footer[index * 32..(index + 1) * 32];
        value[..value.iter().rposition(|b| *b != 0).map_or(0, |p| p + 1)].to_vec()
    };
    Ok(Metadata {
        magic: field(7),
        platform: field(6),
        api_version: field(5),
        version: field(4).into(),
        abi: field(3),
    })
}
fn mismatch(meta: &Metadata) -> Result<ByteString> {
    if meta.magic != b"4" {
        return Ok(
            "The file is not a DuckDB extension. The metadata at the end of the file is invalid".into(),
        );
    }
    let mut result = ByteString::new();
    match meta.abi.as_slice() {
        b"C_STRUCT" => match semver(&meta.api_version) {
            None => {
                result = byte_format!(
                    "The file was built for DuckDB C API version '{}', which failed to parse as a recognized version string",
                    meta.api_version
                );
            }
            Some((major, _, _)) if major != 1 => {
                result = byte_format!(
                    "The file was built for DuckDB C API version '{}', but we can only load extensions built for DuckDB C API 'v1.x.y'.",
                    meta.api_version
                );
            }
            Some(_) if !supported(&meta.api_version) => {
                result = byte_format!(
                    "The file was built for DuckDB C API version '{}', but we can only load extensions built for DuckDB C API 'v1.5.6' and lower.",
                    meta.api_version
                );
            }
            _ => {}
        },
        b"C_STRUCT_UNSTABLE" | b"CPP" | b"" => {
            if meta.api_version != b"v1.5.6" {
                result = byte_format!(
                    "The file was built specifically for DuckDB version '{}' and can only be loaded with that version of DuckDB. (this version of DuckDB is 'v1.5.6')",
                    pretty(&meta.api_version)
                );
            }
        }
        _ => {
            return Err(Error::internal(byte_format!(
                "Unknown ABI type for extension: '{}'",
                meta.abi
            )));
        }
    }
    let platform = rustdb::external_extensions::platform();
    if meta.platform != platform.as_bytes() {
        result.push_bytes(if result.is_empty() { "T" } else { " Also, t" });
        result.push_bytes(byte_format!("he file was built for the platform '{}', but we can only load extensions built for platform '{platform}'.", pretty(&meta.platform)));
    }
    Ok(result)
}

/// Hash independent 1MiB chunks, then their raw SHA256 digests, as C++ does.
fn signature_valid(file: &mut File, community: bool) -> Result<bool> {
    let keys = crate::extension_keys::CORE_KEYS
        .iter()
        .chain(crate::extension_keys::COMMUNITY_KEYS.iter().filter(|_| community))
        .copied();
    signature_valid_with_keys(file, keys)
}

fn signature_valid_with_keys(file: &mut File, keys: impl Iterator<Item = &'static str>) -> Result<bool> {
    let length = file.metadata().map_err(|error| io_error(&error))?.len() - 256;
    file.seek(SeekFrom::Start(0)).map_err(|error| io_error(&error))?;
    let mut remaining = length;
    let mut outer = Sha256::new();
    let mut buffer = [0; 8192];
    while remaining > 0 {
        let mut chunk = remaining.min(1024 * 1024);
        let mut inner = Sha256::new();
        while chunk > 0 {
            let amount =
                usize::try_from(chunk.min(8192)).map_err(|_| Error::internal("extension chunk size"))?;
            file.read_exact(&mut buffer[..amount])
                .map_err(|error| io_error(&error))?;
            inner.update(&buffer[..amount]);
            chunk -= amount as u64;
            remaining -= amount as u64;
        }
        outer.update(inner.finalize());
    }
    let digest = outer.finalize();
    let mut signature = [0; 256];
    file.read_exact(&mut signature)
        .map_err(|error| io_error(&error))?;
    Ok(keys
        .filter_map(|pem| PublicKey::from_public_key_pem(pem).ok())
        .any(|key| {
            key.verify(Pkcs1v15Sign::new::<rsa::sha2::Sha256>(), &digest, &signature)
                .is_ok()
        }))
}

#[derive(Debug)]
struct LoadContext {
    _database: Box<DatabaseWrapper>,
    handle: DuckdbDatabase,
    error: Mutex<Option<(ErrorKind, ByteString)>>,
    unstable: bool,
}
#[repr(C)]
struct Access {
    set_error: unsafe extern "C" fn(*mut c_void, *const c_char),
    get_database: unsafe extern "C" fn(*mut c_void) -> *mut DuckdbDatabase,
    get_api: unsafe extern "C" fn(*mut c_void, *const c_char) -> *const c_void,
}
type Init = unsafe extern "C" fn(*mut c_void, *mut Access) -> bool;

unsafe extern "C" fn set_error(info: *mut c_void, error: *const c_char) {
    let _ = crate::helper::catch(|| {
        // SAFETY: info is the live initialization context, supplied by this adapter.
        if let Some(context) = unsafe { info.cast::<LoadContext>().as_ref() } {
            let error = if error.is_null() {
                (
                    ErrorKind::UnknownType,
                    ByteString::from(
                        "Extension has indicated an error occurred during initialization, but did not set an error message.",
                    ),
                )
            } else {
                // SAFETY: the extension supplies a C string for the duration of this call.
                (
                    ErrorKind::Invalid,
                    ByteString::from(unsafe { CStr::from_ptr(error) }.to_bytes()),
                )
            };
            *context.error.lock().unwrap_or_else(PoisonError::into_inner) = Some(error);
        }
        Ok(())
    });
}
unsafe extern "C" fn get_database(info: *mut c_void) -> *mut DuckdbDatabase {
    // SAFETY: info is the context supplied to the initializer; its borrowed handle
    // remains stable until initialization returns. The extension must not close it.
    unsafe { info.cast::<LoadContext>().as_ref() }.map_or(std::ptr::null_mut(), |context| {
        (&raw const context.handle).cast_mut()
    })
}
unsafe extern "C" fn get_api(info: *mut c_void, version: *const c_char) -> *const c_void {
    let result = crate::helper::catch(|| {
        // SAFETY: the initializer only receives our live context pointer.
        let Some(context) = (unsafe { info.cast::<LoadContext>().as_ref() }) else {
            return Ok(std::ptr::null());
        };
        // SAFETY: a non-null version is a C string for this callback.
        let version = if version.is_null() {
            &b""[..]
        } else {
            unsafe { CStr::from_ptr(version) }.to_bytes()
        };
        if !context.unstable && !supported(version) {
            *context.error.lock().unwrap_or_else(PoisonError::into_inner) = Some((
                ErrorKind::UnknownType,
                byte_format!(
                    "Unsupported C CAPI version detected during extension initialization: {version}"
                ),
            ));
            return Ok(std::ptr::null());
        }
        Ok((&raw const EXTENSION_API_V1).cast::<c_void>())
    });
    result.unwrap_or(std::ptr::null())
}

// C++ never dlcloses successful extension code. Keep one module lease per native
// path so callbacks/results cannot outlive their machine code, even after an init
// error partially registered a function. This holds no query/table data.
static LIBRARIES: OnceLock<Mutex<BTreeMap<PathBuf, Arc<Library>>>> = OnceLock::new();
fn open_library(path: &Path) -> Result<Arc<Library>> {
    let libraries = LIBRARIES.get_or_init(Mutex::default);
    if let Some(library) = libraries
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(path)
        .cloned()
    {
        return Ok(library);
    }
    // SAFETY: loading native code is explicitly authorized by LOAD and validated
    // metadata/signature policy; the code has the same trust as a C++ extension.
    #[cfg(unix)]
    let opened = unsafe {
        libloading::os::unix::Library::open(
            Some(path),
            libloading::os::unix::RTLD_NOW | libloading::os::unix::RTLD_LOCAL,
        )
    }
    .map(Library::from);
    #[cfg(not(unix))]
    let opened = unsafe { Library::new(path) };
    let library = Arc::new(opened.map_err(|error| Error::new(ErrorKind::Io, error.to_string()))?);
    Ok(Arc::clone(
        libraries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(path.into())
            .or_insert(library),
    ))
}

pub(crate) fn load(database: &Database, filename: &[u8], requested: &[u8], direct: bool) -> Result<()> {
    let path = rustdb_common::native_file_path(filename)?;
    let basename = path
        .file_stem()
        .ok_or_else(|| Error::invalid_input("Extension path has no name"))?;
    let name = basename.as_encoded_bytes();
    let state = database.external_extensions();
    let lock = state.initialization_lock(name);
    let _guard = lock.lock().unwrap_or_else(PoisonError::into_inner);
    if state.contains(name) {
        return Ok(());
    }
    let mut file = File::open(&path).map_err(|error| io_error(&error))?;
    let meta = metadata(&mut file, filename)?;
    // The pinned httpfs package identifies the contract implemented in Rust.
    // Its C++ machine code is never opened or executed by this adapter.
    let ported_httpfs = name == b"httpfs"
        && meta.magic == b"4"
        && meta.abi == b"CPP"
        && meta.version == "4bc690d"
        && meta.api_version == b"v1.5.6"
        && meta.platform == rustdb::external_extensions::platform().as_bytes();
    // Loading C++ engine-internal binaries is a separate unsupported ABI.
    if !ported_httpfs && meta.magic == b"4" && matches!(meta.abi.as_slice(), b"CPP" | b"") {
        return Err(Error::not_implemented(byte_format!(
            "loading the extension file \"{filename}\""
        )));
    }
    let error = mismatch(&meta)?;
    let unsigned = setting(database, "allow_unsigned_extensions");
    if (!unsigned || !setting(database, "allow_extensions_metadata_mismatch")) && !error.is_empty() {
        return Err(Error::invalid_input(byte_format!(
            "Failed to load '{requested}', {error}"
        )));
    }
    if !unsigned && !signature_valid(&mut file, setting(database, "allow_community_extensions"))? {
        return Err(Error::new_bytes(
            ErrorKind::Io,
            byte_format!(
                "Extension \"{filename}\" could not be loaded because its signature is either missing or invalid and unsigned extensions are disabled by configuration (allow_unsigned_extensions)"
            ),
        ));
    }
    if ported_httpfs {
        if !direct {
            rustdb::external_extensions::validate_installed_metadata(filename, name, &meta.version)?;
        }
        database.register_http_file_system()?;
        state.finish_load(rustdb::external_extensions::ExternalExtensionRecord {
            name: name.into(),
            version: meta.version,
        });
        return Ok(());
    }
    if !matches!(meta.abi.as_slice(), b"C_STRUCT" | b"C_STRUCT_UNSTABLE") {
        return Err(Error::internal("Unknown ABI type for extension"));
    }
    let library = open_library(&path)?;
    if !direct {
        rustdb::external_extensions::validate_installed_metadata(filename, name, &meta.version)?;
    }
    let symbol = CString::new(byte_format!("{name}_init_c_api").as_bytes())
        .map_err(|_| Error::invalid_input("NUL in extension entrypoint"))?;
    // SAFETY: the selected C_STRUCT entrypoint has the public header's signature.
    let initialize = unsafe { library.get::<Init>(symbol.as_bytes_with_nul()) }.map_err(|error| {
        Error::new(
            ErrorKind::Io,
            format!(
                "File \"{}\" did not contain function \"{}\": {error}",
                path.display(),
                symbol.to_string_lossy()
            ),
        )
    })?;
    let mut wrapper = Box::new(DatabaseWrapper {
        database: Arc::new(database.clone()),
    });
    let handle = &raw mut *wrapper;
    let mut context = Box::new(LoadContext {
        _database: wrapper,
        handle,
        error: Mutex::new(None),
        unstable: meta.abi == b"C_STRUCT_UNSTABLE",
    });
    let mut access = Access {
        set_error,
        get_database,
        get_api,
    };
    // SAFETY: all access callbacks and their borrowed database remain valid for the
    // synchronous initializer; the module lease is retained for registered callbacks.
    let initialized = unsafe { initialize((&raw mut *context).cast(), &raw mut access) };
    let error = context
        .error
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take();
    if let Some((kind, error)) = error {
        return Err(Error::new_bytes(
            kind,
            byte_format!("An error was thrown during initialization of the extension '{requested}': {error}"),
        ));
    }
    if !initialized {
        return Err(Error::new_bytes(
            ErrorKind::Fatal,
            byte_format!(
                "Extension '{filename}' failed to initialize but did not return an error. This indicates an error in the extension: C API extensions should return a boolean `true` to indicate successful initialization. This means that the Extension may be partially initialized resulting in an inconsistent state of DuckDB."
            ),
        ));
    }
    state.finish_load(rustdb::external_extensions::ExternalExtensionRecord {
        name: name.into(),
        version: meta.version,
    });
    Ok(())
}

/// Validate install bytes without executing any initializer or opening its code.
pub(crate) fn validate_install(database: &Database, filename: &[u8], name: &[u8]) -> Result<ByteString> {
    let mut file =
        File::open(rustdb_common::native_file_path(filename)?).map_err(|error| io_error(&error))?;
    if file.metadata().map_err(|error| io_error(&error))?.len() < 512 {
        return Err(Error::new_bytes(
            ErrorKind::Io,
            byte_format!("Failed to install '{name}', file too small to be a valid DuckDB extension!"),
        ));
    }
    let meta = metadata(&mut file, filename)?;
    let error = mismatch(&meta)?;
    if !error.is_empty() && !setting(database, "allow_extensions_metadata_mismatch") {
        return Err(Error::new_bytes(
            ErrorKind::Io,
            byte_format!("Failed to install '{name}'\n{error}"),
        ));
    }
    if !setting(database, "allow_unsigned_extensions")
        && !signature_valid(&mut file, setting(database, "allow_community_extensions"))?
    {
        return Err(Error::new(
            ErrorKind::Io,
            "Attempting to install an extension file that doesn't have a valid signature, see https://duckdb.org/docs/stable/operations_manual/securing_duckdb/securing_extensions",
        ));
    }
    Ok(meta.version)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    const TEST_PUBLIC_KEY: &str = include_str!("../tests/data/extension-test-public.pem");

    #[test]
    fn reference_version_negotiation_limits() {
        for version in [b"v1.0.0".as_slice(), b"v1.2.0", b"v1.4.999", b"v1.5.6"] {
            assert!(supported(version));
        }
        for version in [
            b"v0.9.0".as_slice(),
            b"v2.0.0",
            b"v1.5.7",
            b"v1.6.0",
            b"1.5.6",
            b"v1.5",
            b"v1.5.6.0",
        ] {
            assert!(!supported(version));
        }
    }

    #[test]
    fn independent_openssl_signatures_chunk_boundaries_and_tampering() -> Result<()> {
        let cases: [(usize, &[u8]); 2] = [
            (
                1024 * 1024 - 256,
                include_bytes!("../tests/data/extension-case1.sig"),
            ),
            (
                1024 * 1024 + 129,
                include_bytes!("../tests/data/extension-case2.sig"),
            ),
        ];
        for (size, signature) in cases {
            let path = std::env::temp_dir().join(format!(
                "rustdb-extension-signature-{}-{size}",
                std::process::id()
            ));
            let mut bytes = vec![b'x'; size];
            for field in [
                b"".as_slice(),
                b"",
                b"",
                b"C_STRUCT",
                b"0.0.1",
                b"v1.2.0",
                b"linux_amd64",
                b"4",
            ] {
                let mut padded = [0; 32];
                padded[..field.len()].copy_from_slice(field);
                bytes.extend_from_slice(&padded);
            }
            bytes.extend_from_slice(signature);
            let mut file = File::create(&path).map_err(|error| io_error(&error))?;
            file.write_all(&bytes).map_err(|error| io_error(&error))?;
            drop(file);
            let mut file = File::open(&path).map_err(|error| io_error(&error))?;
            assert!(signature_valid_with_keys(
                &mut file,
                std::iter::once(TEST_PUBLIC_KEY)
            )?);
            assert!(
                !signature_valid(&mut file, true)?,
                "Synthetic test key must never be a production trust anchor"
            );
            bytes[0] ^= 1;
            std::fs::write(&path, &bytes).map_err(|error| io_error(&error))?;
            let mut file = File::open(&path).map_err(|error| io_error(&error))?;
            assert!(!signature_valid_with_keys(
                &mut file,
                std::iter::once(TEST_PUBLIC_KEY)
            )?);
            bytes[0] ^= 1;
            let last = bytes.len() - 1;
            bytes[last] ^= 1;
            std::fs::write(&path, &bytes).map_err(|error| io_error(&error))?;
            let mut file = File::open(&path).map_err(|error| io_error(&error))?;
            assert!(!signature_valid_with_keys(
                &mut file,
                std::iter::once(TEST_PUBLIC_KEY)
            )?);
            std::fs::remove_file(path).map_err(|error| io_error(&error))?;
        }
        Ok(())
    }
}
