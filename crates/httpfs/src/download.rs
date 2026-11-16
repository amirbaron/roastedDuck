//! Statement-scoped full downloads stored in spillable, accounted Rust buffers.
use std::io::Read;
use std::sync::{Arc, Mutex, PoisonError};

use roastedduck_buffer::{BlockHandle, BufferManager, MemoryTag};
use roastedduck_common::{Error, Result};

const CHUNK: usize = 1024 * 1024;

#[derive(Debug, Default)]
pub(super) struct State(pub Mutex<Option<Arc<Downloaded>>>);

impl State {
    pub fn current(path: &str) -> Arc<Self> {
        roastedduck_function::file_system::opener().map_or_else(
            || Arc::new(Self::default()),
            |client| {
                client
                    .query_cache()
                    .get_or_insert_with(format!("httpfs.full:{path}"), Self::default)
            },
        )
    }

    pub fn get(&self) -> Option<Arc<Downloaded>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

#[derive(Debug)]
pub(super) struct Downloaded {
    manager: BufferManager,
    blocks: Vec<Arc<BlockHandle>>,
    pub size: u64,
}

impl Downloaded {
    pub fn read(mut source: impl Read, manager: BufferManager) -> Result<Self> {
        let mut result = Self {
            manager,
            blocks: Vec::new(),
            size: 0,
        };
        loop {
            let pin = result.manager.allocate(MemoryTag::Extension, CHUNK, false)?;
            // SAFETY: this newly allocated block has a unique pin. It is only
            // published after filling; every reader pins immutable initialized bytes.
            let bytes = unsafe {
                std::ptr::write_bytes(pin.as_ptr(), 0, CHUNK);
                std::slice::from_raw_parts_mut(pin.as_ptr(), CHUNK)
            };
            let mut count = 0;
            while count < CHUNK {
                let read = source.read(&mut bytes[count..]).map_err(super::io_error)?;
                if read == 0 {
                    break;
                }
                count += read;
            }
            if count != 0 {
                result.size = result
                    .size
                    .checked_add(count as u64)
                    .ok_or_else(|| Error::out_of_range("HTTP download is too large"))?;
                result.blocks.push(Arc::clone(pin.block()));
            }
            if count < CHUNK {
                break;
            }
        }
        Ok(result)
    }

    pub fn read_at(&self, offset: u64, bytes: &mut [u8]) -> Result<usize> {
        let count = bytes
            .len()
            .min(usize::try_from(self.size.saturating_sub(offset)).unwrap_or(usize::MAX));
        let mut copied = 0;
        while copied < count {
            let position = offset + copied as u64;
            let block = usize::try_from(position / CHUNK as u64).map_err(super::io_error)?;
            let start = (position % CHUNK as u64) as usize;
            let length = (CHUNK - start).min(count - copied);
            let pin = self.manager.pin(&self.blocks[block])?;
            // SAFETY: the pin keeps this immutable block resident and the range
            // is restricted to bytes initialized by read, including the last block.
            bytes[copied..copied + length]
                .copy_from_slice(unsafe { std::slice::from_raw_parts(pin.as_ptr().add(start), length) });
            copied += length;
        }
        Ok(count)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use roastedduck_buffer::BufferManagerConfig;

    struct Pattern {
        position: u64,
        size: u64,
    }

    impl Read for Pattern {
        fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
            let count = bytes.len().min((self.size - self.position) as usize);
            for (index, byte) in bytes[..count].iter_mut().enumerate() {
                *byte = ((self.position + index as u64) % 251) as u8;
            }
            self.position += count as u64;
            Ok(count)
        }
    }

    #[test]
    fn full_download_spills_and_reads_across_block_boundaries() {
        let path = std::env::temp_dir().join(format!(
            "httpfs-download-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let manager = BufferManager::new(BufferManagerConfig {
            memory_limit: Some((3 * CHUNK) as u64),
            temp_directory: Some(path.clone()),
            ..BufferManagerConfig::default()
        })
        .unwrap();
        let size = (8 * CHUNK + 17) as u64;
        let file = Downloaded::read(Pattern { position: 0, size }, manager.clone()).unwrap();
        assert_eq!(file.size, size);
        assert!(manager.used_memory() <= (3 * CHUNK) as u64);
        assert!(
            path.exists(),
            "the download must spill, not bypass the memory limit"
        );
        let mut bytes = vec![0; CHUNK + 29];
        for offset in [0, CHUNK as u64 - 3, 7 * CHUNK as u64 + 1, size, size + 1] {
            let count = file.read_at(offset, &mut bytes).unwrap();
            assert_eq!(count, bytes.len().min(size.saturating_sub(offset) as usize));
            assert!(
                bytes[..count]
                    .iter()
                    .enumerate()
                    .all(|(i, byte)| *byte == ((offset + i as u64) % 251) as u8)
            );
        }
        drop(file);
        assert_eq!(manager.used_memory(), 0);
        drop(manager);
        if path.exists() {
            std::fs::remove_dir_all(path).unwrap();
        }
    }

    #[test]
    fn incomplete_download_releases_its_buffers() {
        struct Broken(bool);
        impl Read for Broken {
            fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
                if self.0 {
                    self.0 = false;
                    bytes.fill(42);
                    return Ok(bytes.len());
                }
                Err(std::io::Error::new(
                    std::io::ErrorKind::ConnectionReset,
                    "fixture disconnect",
                ))
            }
        }
        let manager = BufferManager::new(BufferManagerConfig::default()).unwrap();
        assert!(Downloaded::read(Broken(true), manager.clone()).is_err());
        assert_eq!(manager.used_memory(), 0);
    }
}
