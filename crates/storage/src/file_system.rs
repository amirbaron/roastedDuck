//! Database/WAL file access. Registered handles retain their backend independently of
//! the connection's filesystem registry; local files keep native OS I/O.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::sync::Arc;

use roastedduck_function::registered_files::{self, Cursor, FileSystem, OpenFlags};

/// Recover callback errors carried through the standard stream interfaces without
/// replacing their type, subtype, or diagnostic metadata with a generic I/O error.
pub use roastedduck_function::registered_files::callback_error;

#[derive(Clone, Debug, Default)]
pub struct DatabaseFileSystem {
    registered: Option<Arc<dyn FileSystem>>,
    disabled: Option<Arc<roastedduck_function::file_system::DisabledFileSystems>>,
}

impl DatabaseFileSystem {
    pub fn for_path(path: &[u8]) -> Self {
        Self {
            registered: registered_files::lookup(path),
            disabled: roastedduck_function::file_system::opener()
                .and_then(|client| client.object_cache())
                .and_then(|cache| cache.get(roastedduck_function::file_system::DISABLED_FILE_SYSTEMS_KEY)),
        }
    }

    fn verify_enabled(&self) -> roastedduck_common::Result<()> {
        if let Some(disabled) = &self.disabled {
            disabled.verify_enabled(self.registered.as_ref().map_or("LocalFileSystem", |fs| fs.name()))?;
        }
        Ok(())
    }

    pub fn is_registered(&self) -> bool {
        self.registered.is_some()
    }

    pub fn exists(&self, path: &[u8]) -> roastedduck_common::Result<bool> {
        self.verify_enabled()?;
        match &self.registered {
            Some(fs) => fs.is_file(path),
            None => Ok(roastedduck_common::native_file_path(path)?.exists()),
        }
    }

    pub fn open(&self, path: &[u8], flags: OpenFlags) -> io::Result<StorageFile> {
        self.verify_enabled().map_err(io::Error::other)?;
        if let Some(fs) = &self.registered {
            return fs
                .open_with_flags(path, flags)
                .map(|handle| StorageFile::Registered(Cursor::new(handle)))
                .map_err(io::Error::other);
        }
        OpenOptions::new()
            .read(flags.read)
            .write(flags.write)
            .create(flags.create)
            .truncate(flags.truncate)
            .open(roastedduck_common::native_file_path(path).map_err(io::Error::other)?)
            .map(StorageFile::Local)
    }

    pub fn remove(&self, path: &[u8]) -> io::Result<()> {
        self.verify_enabled().map_err(io::Error::other)?;
        match &self.registered {
            Some(fs) => fs.remove(path).map_err(io::Error::other),
            None => {
                std::fs::remove_file(roastedduck_common::native_file_path(path).map_err(io::Error::other)?)
            }
        }
    }

    pub fn remove_if_exists(&self, path: &[u8]) -> roastedduck_common::Result<()> {
        if self.registered.is_some() && !self.exists(path)? {
            return Ok(());
        }
        match self.remove(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(callback_error(&error).unwrap_or_else(|| {
                roastedduck_common::Error::new_bytes(
                    roastedduck_common::ErrorKind::Io,
                    roastedduck_common::byte_format!("Could not remove file \"{path}\": {error}"),
                )
            })),
        }
    }

    pub fn rename(&self, source: &[u8], target: &[u8]) -> io::Result<()> {
        self.verify_enabled().map_err(io::Error::other)?;
        match &self.registered {
            Some(fs) => fs.rename(source, target).map_err(io::Error::other),
            None => std::fs::rename(
                roastedduck_common::native_file_path(source).map_err(io::Error::other)?,
                roastedduck_common::native_file_path(target).map_err(io::Error::other)?,
            ),
        }
    }
}

#[derive(Debug)]
pub enum StorageFile {
    Local(File),
    Registered(Cursor),
}

impl StorageFile {
    pub fn size(&self) -> io::Result<u64> {
        match self {
            Self::Local(file) => file.metadata().map(|m| m.len()),
            Self::Registered(file) => file.handle.size().map_err(io::Error::other),
        }
    }
    pub fn set_len(&self, size: u64) -> io::Result<()> {
        match self {
            Self::Local(file) => file.set_len(size),
            Self::Registered(file) => file.handle.truncate(size).map_err(io::Error::other),
        }
    }
    pub fn sync_data(&self) -> io::Result<()> {
        match self {
            Self::Local(file) => {
                #[cfg(target_vendor = "apple")]
                {
                    use std::os::fd::AsRawFd;
                    // Match the pinned C++ FileSync configuration's fsync operation.
                    // Apple's std::fs::File::sync_data requests F_FULLFSYNC instead.
                    // Keep the checkpoint/WAL caller's ordering and propagate errno.
                    // SAFETY: the borrowed File keeps this descriptor valid.
                    if unsafe { libc::fsync(file.as_raw_fd()) } == 0 {
                        Ok(())
                    } else {
                        Err(io::Error::last_os_error())
                    }
                }
                #[cfg(not(target_vendor = "apple"))]
                {
                    file.sync_data()
                }
            }
            Self::Registered(file) => file.handle.sync().map_err(io::Error::other),
        }
    }
    pub fn sync_all(&self) -> io::Result<()> {
        match self {
            Self::Local(file) => file.sync_all(),
            Self::Registered(file) => file.handle.sync().map_err(io::Error::other),
        }
    }
}
impl Read for StorageFile {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Local(file) => file.read(bytes),
            Self::Registered(file) => file.read(bytes),
        }
    }
}
impl Write for StorageFile {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        match self {
            Self::Local(file) => file.write(bytes),
            Self::Registered(file) => file.write(bytes),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Local(file) => file.flush(),
            Self::Registered(file) => file.flush(),
        }
    }
}
impl Seek for StorageFile {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        match self {
            Self::Local(file) => file.seek(from),
            Self::Registered(file) => file.seek(from),
        }
    }
}

#[cfg(all(test, target_vendor = "apple"))]
mod cpp_sync_parity_tests {
    use super::StorageFile;
    use std::fs::{self, File, OpenOptions};
    use std::io::{Read, Write};
    use std::os::fd::FromRawFd;

    #[test]
    fn local_sync_preserves_data_after_independent_reopen() {
        let path = std::env::temp_dir().join(format!("roastedduck-cpp-sync-{}.bin", std::process::id()));
        let mut file = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        let expected = b"checkpoint body\0with binary bytes\xff";
        file.write_all(expected).unwrap();
        let file = StorageFile::Local(file);
        file.sync_data().unwrap();
        drop(file);
        let mut actual = Vec::new();
        File::open(&path).unwrap().read_to_end(&mut actual).unwrap();
        assert_eq!(actual, expected);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn local_sync_propagates_real_kernel_error_for_pipe() {
        let mut descriptors = [-1; 2];
        // SAFETY: pipe writes two live descriptors to the provided array.
        assert_eq!(unsafe { libc::pipe(descriptors.as_mut_ptr()) }, 0);
        // SAFETY: ownership of each fresh descriptor is transferred once.
        let read = unsafe { File::from_raw_fd(descriptors[0]) };
        let _write = unsafe { File::from_raw_fd(descriptors[1]) };
        let error = StorageFile::Local(read).sync_data().unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::EINVAL));
    }
}
