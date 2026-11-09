//! Database-local filesystem callbacks. File contents remain streamed, never cached here.
use crate::ObjectCache;
use roastedduck_common::{ByteString, Error, Result};
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::{Arc, Mutex, PoisonError};
const KEY: &str = "roastedduck_registered_file_systems";
/// File access requested by database storage, including non-truncating WAL opens.
#[derive(Clone, Copy, Debug)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent OS file-open flags, matching std::fs::OpenOptions"
)]
pub struct OpenFlags {
    pub read: bool,
    pub write: bool,
    pub create: bool,
    pub truncate: bool,
}
pub trait FileHandle: std::fmt::Debug + Send + Sync {
    fn read_at(&self, offset: u64, buffer: &mut [u8]) -> Result<usize>;
    /// Optional forward-only input, without retaining file contents. Consumers
    /// that need seeking continue to use `read_at` through `Cursor`.
    fn read_stream(&self) -> Result<Option<Box<dyn Read + Send>>> {
        Ok(None)
    }
    fn write_at(&self, offset: u64, buffer: &[u8]) -> Result<usize>;
    fn size(&self) -> Result<u64>;
    fn modified(&self) -> Result<i64>;
    /// An opaque identity for this version, such as an HTTP `ETag`. Filesystems
    /// without one retain the existing size/modification-time validation.
    fn version_tag(&self) -> Result<String> {
        Ok(String::new())
    }
    fn flush(&self) -> Result<()>;
    /// Explicit successful writer finalization. Remote filesystems can complete an
    /// upload here and propagate failures; ordinary flushes remain non-finalizing.
    fn close(&self) -> Result<()> {
        Ok(())
    }
    fn sync(&self) -> Result<()> {
        self.flush()
    }
    fn truncate(&self, _size: u64) -> Result<()> {
        Err(Error::not_implemented(
            "Registered filesystem does not support file truncation",
        ))
    }
}
/// Incremental filesystem expansion. A page can be empty while more requests
/// remain; None means expansion is complete. File data is never retained here.
pub trait GlobResult: std::fmt::Debug + Send {
    fn expand_next(&mut self) -> Result<Option<Vec<ByteString>>>;
}

pub trait FileSystem: std::fmt::Debug + Send + Sync {
    fn name(&self) -> &str;
    fn is_remote(&self) -> bool {
        false
    }
    fn handles(&self, path: &[u8]) -> bool;
    fn open(&self, path: &[u8], write: bool) -> Result<Arc<dyn FileHandle>>;
    fn open_with_flags(&self, path: &[u8], flags: OpenFlags) -> Result<Arc<dyn FileHandle>> {
        if !flags.write || flags.create && flags.truncate && !flags.read {
            self.open(path, flags.write)
        } else {
            Err(Error::not_implemented(
                "Registered filesystem does not support the requested file open flags",
            ))
        }
    }
    fn glob(&self, path: &[u8]) -> Result<Vec<ByteString>>;
    /// Optional incremental expansion for scanners that can defer later pages.
    /// Existing callback filesystems keep their ordinary eager glob behavior.
    fn glob_lazy(&self, _path: &[u8]) -> Result<Option<Box<dyn GlobResult>>> {
        Ok(None)
    }
    fn is_file(&self, path: &[u8]) -> Result<bool>;
    fn is_dir(&self, path: &[u8]) -> Result<bool>;
    fn mkdir(&self, path: &[u8]) -> Result<()>;
    fn remove(&self, path: &[u8]) -> Result<()>;
    fn remove_files(&self, paths: &[ByteString]) -> Result<()> {
        for path in paths {
            self.remove(path)?;
        }
        Ok(())
    }
    fn list_files_recursive(&self, path: &[u8]) -> Result<Vec<ByteString>> {
        let pattern = roastedduck_common::byte_format!("{path}/**");
        let mut files = Vec::new();
        for file in self.glob(&pattern)? {
            if self.is_file(&file)? {
                files.push(file);
            }
        }
        Ok(files)
    }
    fn rename(&self, source: &[u8], target: &[u8]) -> Result<()>;
}
#[derive(Debug, Default)]
pub struct Registry {
    systems: Mutex<Vec<Arc<dyn FileSystem>>>,
}
impl Registry {
    pub fn initialize(cache: &ObjectCache) -> Arc<Self> {
        cache.get_or_insert_with(KEY, Self::default)
    }
    pub fn register(&self, system: Arc<dyn FileSystem>) -> Result<()> {
        let mut systems = self.systems.lock().unwrap_or_else(PoisonError::into_inner);
        if systems.iter().any(|fs| fs.name() == system.name()) {
            return Err(Error::invalid_input(format!(
                "Filesystem with name {} has already been registered, cannot re-register!",
                system.name()
            )));
        }
        systems.push(system);
        Ok(())
    }
    pub fn remove(&self, name: &str) {
        self.systems
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|fs| fs.name() != name);
    }
    pub fn names(&self) -> Vec<String> {
        self.systems
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|fs| fs.name().to_owned())
            .collect()
    }
    pub fn lookup(&self, path: &[u8]) -> Option<Arc<dyn FileSystem>> {
        self.systems
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .find(|fs| fs.handles(path))
            .cloned()
    }
}
pub fn lookup(path: &[u8]) -> Option<Arc<dyn FileSystem>> {
    let cache = crate::file_system::opener()?.object_cache()?;
    cache.get::<Registry>(KEY)?.lookup(path)
}

/// Preserve the original callback error through `std::io` stream adapters.
pub fn callback_error(error: &std::io::Error) -> Option<Error> {
    error.get_ref()?.downcast_ref::<Error>().cloned()
}
/// A sequential cursor over a positional handle. Each handle supplies atomic reads.
#[derive(Debug)]
pub struct Cursor {
    pub handle: Arc<dyn FileHandle>,
    position: u64,
}
impl Cursor {
    pub fn new(handle: Arc<dyn FileHandle>) -> Self {
        Self { handle, position: 0 }
    }
}
impl Read for Cursor {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let n = self
            .handle
            .read_at(self.position, buffer)
            .map_err(std::io::Error::other)?;
        if n > buffer.len() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Registered filesystem returned an invalid read count",
            ));
        }
        self.position += n as u64;
        Ok(n)
    }
}
impl Write for Cursor {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let n = self
            .handle
            .write_at(self.position, buffer)
            .map_err(std::io::Error::other)?;
        if n > buffer.len() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Registered filesystem returned an invalid write count",
            ));
        }
        self.position += n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.handle.flush().map_err(std::io::Error::other)
    }
}
impl Seek for Cursor {
    fn seek(&mut self, from: SeekFrom) -> std::io::Result<u64> {
        let target = match from {
            SeekFrom::Start(n) => i128::from(n),
            SeekFrom::Current(n) => i128::from(self.position) + i128::from(n),
            SeekFrom::End(n) => {
                i128::from(self.handle.size().map_err(std::io::Error::other)?) + i128::from(n)
            }
        };
        self.position = u64::try_from(target).map_err(|_| std::io::Error::other("Invalid file offset"))?;
        Ok(self.position)
    }
}
/// A local or callback-backed output stream; buffering stays with native writers.
#[derive(Debug)]
pub enum Output {
    Local(std::fs::File),
    Registered(Cursor),
}
impl Output {
    pub fn close(&mut self) -> std::io::Result<()> {
        match self {
            Self::Local(file) => file.flush(),
            Self::Registered(cursor) => cursor.handle.close().map_err(std::io::Error::other),
        }
    }
    pub fn create(path: &[u8]) -> Result<Self> {
        crate::file_system::verify_can_write(path)?;
        if let Some(fs) = lookup(path) {
            return fs.open(path, true).map(|h| Self::Registered(Cursor::new(h)));
        }
        std::fs::File::create(crate::file_system::native_path(path)?)
            .map(Self::Local)
            .map_err(|e| Error::new(roastedduck_common::ErrorKind::Io, e.to_string()))
    }
}
impl Write for Output {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        match self {
            Self::Local(file) => file.write(bytes),
            Self::Registered(file) => file.write(bytes),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Self::Local(file) => file.flush(),
            Self::Registered(file) => file.flush(),
        }
    }
}
