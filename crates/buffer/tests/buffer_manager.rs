//! Behaviour of the buffer manager: pinning, accounting, eviction, spilling and errors.
#![allow(clippy::unwrap_used)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use rustdb_buffer::{
    BlockHandle, BlockId, BlockManager, BlockRegistry, BlockState, BufferHandle, BufferManager,
    BufferManagerConfig, DEFAULT_BLOCK_ALLOC_SIZE, FileBuffer, MemoryTag,
};
use rustdb_common::{ErrorKind, Result};

const BLOCK: usize = DEFAULT_BLOCK_ALLOC_SIZE;

/// A fresh, unique temporary directory path (not created).
fn temp_dir(name: &str) -> PathBuf {
    let unique = format!(
        "rustdb-buffer-{name}-{}-{:?}",
        std::process::id(),
        std::time::SystemTime::now()
    );
    std::env::temp_dir().join(unique.replace([' ', ':', '{', '}', ','], "_"))
}

fn manager(limit: usize, temp: Option<PathBuf>) -> BufferManager {
    BufferManager::new(BufferManagerConfig {
        memory_limit: Some(limit as u64),
        temp_directory: temp,
        ..BufferManagerConfig::default()
    })
    .unwrap()
}

/// The byte at `i` of the block filled with `seed`: a xorshift stream, which temporary
/// file compression cannot shrink, so a spilled block takes a whole slot.
fn pattern(seed: u8, len: usize) -> impl Iterator<Item = u8> {
    let mut state = 0x9E37_79B9_7F4A_7C15_u64 ^ u64::from(seed);
    (0..len).map(move |_| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state.to_le_bytes()[0]
    })
}

fn fill(handle: &BufferHandle, seed: u8) {
    // SAFETY: the test owns the only pin.
    let data = unsafe { handle.as_mut_slice() };
    for (byte, value) in data.iter_mut().zip(pattern(seed, usize::MAX)) {
        *byte = value;
    }
}

fn check(handle: &BufferHandle, seed: u8) {
    // SAFETY: no writers.
    let data = unsafe { handle.as_slice() };
    assert!(
        data.iter().zip(pattern(seed, data.len())).all(|(&byte, value)| byte == value),
        "block contents differ for seed {seed}"
    );
}

#[test]
fn allocate_pin_and_account_by_tag() {
    let buffers = manager(64 * BLOCK, None);
    let handle = buffers.allocate(MemoryTag::HashTable, BLOCK, false).unwrap();
    assert_eq!(handle.len(), BLOCK);
    assert_eq!(
        handle.as_ptr() as usize % 4096,
        0,
        "temporary blocks are page-aligned"
    );
    fill(&handle, 7);
    let block = Arc::clone(handle.block());
    assert_eq!(block.readers(), 1);
    let second = buffers.pin(&block).unwrap();
    assert_eq!(block.readers(), 2);
    check(&second, 7);
    drop(handle);
    drop(second);
    assert_eq!(block.readers(), 0);
    assert_eq!(buffers.used_memory(), BLOCK as u64);
    let info = buffers.memory_usage_info();
    let hash_table = info.iter().find(|i| i.tag == MemoryTag::HashTable).unwrap();
    assert_eq!(hash_table.size, BLOCK as u64);
    drop(block);
    assert_eq!(buffers.used_memory(), 0);
}

#[test]
fn out_of_memory_without_temp_directory_matches_duckdb() {
    let buffers = manager(2_000_000, None);
    let mut pins = Vec::new();
    let error = loop {
        match buffers.allocate(MemoryTag::OrderBy, BLOCK, false) {
            Ok(pin) => pins.push(pin),
            Err(error) => break error,
        }
    };
    assert_eq!(pins.len(), 7);
    assert_eq!(error.kind(), ErrorKind::OutOfMemory);
    assert_eq!(
        error.to_string(),
        "Out of Memory Error: could not allocate block of size 256.0 KiB (1.7 MiB/1.9 MiB used)\n\
         Database is launched in in-memory mode and no temporary directory is specified.\n\
         Unused blocks cannot be offloaded to disk.\n\n\
         Launch the database with a persistent storage back-end\n\
         Or set SET temp_directory='/path/to/tmp.tmp'\n\n\
         Possible solutions:\n\
         * Reducing the number of threads (SET threads=X)\n\
         * Disabling insertion-order preservation (SET preserve_insertion_order=false)\n\
         * Increasing the memory limit (SET memory_limit='...GB')\n\n\
         See also https://duckdb.org/docs/stable/guides/performance/how_to_tune_workloads"
    );
    // Unpinned blocks that must be kept cannot be evicted without a temp directory either.
    let blocks: Vec<_> = pins.iter().map(|pin| Arc::clone(pin.block())).collect();
    pins.clear();
    assert!(buffers.allocate(MemoryTag::OrderBy, BLOCK, false).is_err());
    drop(blocks);
    assert!(buffers.allocate(MemoryTag::OrderBy, BLOCK, false).is_ok());
}

#[test]
fn destroyable_blocks_are_dropped_on_eviction() {
    let buffers = manager(4 * BLOCK, None);
    let first = buffers.allocate(MemoryTag::ColumnData, BLOCK, true).unwrap();
    let first_block = Arc::clone(first.block());
    drop(first);
    let others: Vec<_> = (0..4)
        .map(|_| buffers.allocate(MemoryTag::ColumnData, BLOCK, true).unwrap())
        .collect();
    assert_eq!(first_block.state(), BlockState::Unloaded);
    // Pinning reserves memory before it can find out the data is gone.
    assert_eq!(
        buffers.try_pin(&first_block).unwrap_err().kind(),
        ErrorKind::OutOfMemory
    );
    drop(others);
    assert!(buffers.try_pin(&first_block).unwrap().is_none());
    assert!(buffers.pin(&first_block).is_err());
}

#[test]
fn spills_to_temp_files_and_reads_back() {
    let dir = temp_dir("spill");
    let buffers = manager(8 * BLOCK, Some(dir.clone()));
    let blocks: Vec<Arc<BlockHandle>> = (0..40u8)
        .map(|seed| {
            let pin = buffers.allocate(MemoryTag::OrderBy, BLOCK, false).unwrap();
            fill(&pin, seed);
            Arc::clone(pin.block())
        })
        .collect();
    assert!(buffers.used_memory() <= 8 * BLOCK as u64);
    let evicted = buffers
        .memory_usage_info()
        .into_iter()
        .find(|info| info.tag == MemoryTag::OrderBy)
        .unwrap()
        .evicted_data;
    assert_eq!(evicted, 32 * BLOCK as u64);
    assert!(buffers.has_files_in_temporary_directory());
    let files = buffers.temporary_files();
    assert_eq!(files.len(), 1);
    assert!(files[0].path.ends_with("duckdb_temp_storage_DEFAULT-0.tmp"));
    assert_eq!(buffers.used_swap(), 32 * BLOCK as u64);

    // Every block reads back intact, in any order.
    for (seed, block) in blocks.iter().enumerate().rev() {
        let pin = buffers.pin(block).unwrap();
        check(&pin, seed as u8);
    }
    drop(blocks);
    assert_eq!(buffers.used_memory(), 0);
    assert_eq!(buffers.used_swap(), 0);
    assert!(!buffers.has_files_in_temporary_directory());
    drop(buffers);
    assert!(
        !dir.exists(),
        "a temp directory we created is removed on shutdown"
    );
}

#[test]
fn odd_sized_buffers_spill_to_their_own_files() {
    let dir = temp_dir("odd");
    let buffers = manager(4 * BLOCK, Some(dir));
    let big = buffers
        .allocate(MemoryTag::HashTable, 3 * BLOCK + 100, false)
        .unwrap();
    fill(&big, 3);
    let big_block = Arc::clone(big.block());
    drop(big);
    let filler = buffers.allocate(MemoryTag::HashTable, 2 * BLOCK, false).unwrap();
    assert_eq!(big_block.state(), BlockState::Unloaded);
    let files = buffers.temporary_files();
    assert_eq!(files.len(), 1);
    assert!(
        files[0]
            .path
            .ends_with(&format!("duckdb_temp_block-{}.block", big_block.block_id()))
    );
    drop(filler);
    let pin = buffers.pin(&big_block).unwrap();
    assert_eq!(pin.len(), 3 * BLOCK + 4096);
    check(&pin, 3);
    assert_eq!(buffers.temporary_files(), Vec::new());
}

#[test]
fn max_temp_directory_size_is_enforced() {
    let dir = temp_dir("swap");
    let buffers = BufferManager::new(BufferManagerConfig {
        memory_limit: Some(2 * BLOCK as u64),
        temp_directory: Some(dir),
        max_temp_directory_size: Some(3 * BLOCK as u64),
        ..BufferManagerConfig::default()
    })
    .unwrap();
    let mut blocks = Vec::new();
    let error = loop {
        match buffers.allocate(MemoryTag::OrderBy, BLOCK, false) {
            Ok(pin) => {
                fill(&pin, blocks.len() as u8);
                blocks.push(Arc::clone(pin.block()));
            }
            Err(error) => break error,
        }
    };
    assert_eq!(blocks.len(), 5, "{error}");
    assert!(error.to_string().starts_with(
        "Out of Memory Error: failed to offload data block of size 256.0 KiB (768.0 KiB/768.0 KiB used).\n\
         This limit was set by the 'max_temp_directory_size' setting."
    ), "{error}");
    assert_eq!(buffers.max_swap(), Some(3 * BLOCK as u64));
    // Lowering the limit below the space in use is refused.
    let error = buffers.set_swap_limit(Some(BLOCK as u64)).unwrap_err();
    assert!(error.to_string().starts_with(
        "Out of Memory Error: failed to adjust the 'max_temp_directory_size', currently used space (768.0 KiB) \
         exceeds the new limit (256.0 KiB)"
    ));
}

#[test]
fn memory_limit_changes_evict_or_fail() {
    let dir = temp_dir("limit");
    let buffers = manager(16 * BLOCK, Some(dir));
    let pinned = buffers.allocate(MemoryTag::Window, BLOCK, false).unwrap();
    let unpinned: Vec<_> = (0..4)
        .map(|_| Arc::clone(buffers.allocate(MemoryTag::Window, BLOCK, false).unwrap().block()))
        .collect();
    buffers.set_memory_limit(2 * BLOCK as u64).unwrap();
    assert!(buffers.used_memory() <= 2 * BLOCK as u64);
    assert_eq!(buffers.max_memory(), 2 * BLOCK as u64);
    let error = buffers.set_memory_limit(100).unwrap_err();
    assert_eq!(
        error.raw_message().lines().next().unwrap(),
        "Failed to change memory limit to 100: could not free up enough memory for the new limit"
    );
    assert_eq!(buffers.max_memory(), 2 * BLOCK as u64);
    drop((pinned, unpinned));
}

#[test]
fn small_memory_and_reservations_are_accounted() {
    let buffers = manager(4 * BLOCK, None);
    let small = buffers.register_small_memory(MemoryTag::Metadata, 1000).unwrap();
    let pin = buffers.pin(&small).unwrap();
    assert_eq!(pin.len(), 1000);
    drop(pin);
    assert_eq!(buffers.used_memory(), 1000);
    buffers.reserve_memory(5000).unwrap();
    assert_eq!(buffers.used_memory(), 6000);
    buffers.free_reserved_memory(5000);
    let data = buffers.allocate_data(70_000).unwrap();
    assert_eq!(data.len(), 70_000);
    assert_eq!(buffers.used_memory(), 71_000);
    drop(data);
    drop(small);
    assert_eq!(buffers.used_memory(), 0);
    let error = buffers.reserve_memory(10 * BLOCK as u64).unwrap_err();
    assert!(
        error
            .raw_message()
            .starts_with("failed to reserve memory data of size 2.5 MiB (0 bytes/1.0 MiB used)")
    );
}

#[test]
fn concurrent_pins_and_evictions_keep_data_intact() {
    let dir = temp_dir("threads");
    let buffers = manager(16 * BLOCK, Some(dir));
    let blocks: Vec<Arc<BlockHandle>> = (0..64u8)
        .map(|seed| {
            let pin = buffers.allocate(MemoryTag::HashTable, BLOCK, false).unwrap();
            fill(&pin, seed);
            Arc::clone(pin.block())
        })
        .collect();
    let blocks = Arc::new(blocks);
    std::thread::scope(|scope| {
        for thread in 0..8usize {
            let blocks = Arc::clone(&blocks);
            let buffers = buffers.clone();
            scope.spawn(move || {
                for round in 0..200usize {
                    let index = (thread * 7 + round * 13) % blocks.len();
                    let pin = buffers.pin(&blocks[index]).unwrap();
                    let again = buffers.pin(&blocks[index]).unwrap();
                    check(&pin, index as u8);
                    drop(pin);
                    drop(again);
                }
            });
        }
    });
    assert!(buffers.used_memory() <= 16 * BLOCK as u64);
    assert!(blocks.iter().all(|block| block.readers() == 0));
}

/// A block manager keeping "persistent" blocks in a map.
#[derive(Debug, Default)]
struct MapBlockManager {
    blocks: Mutex<HashMap<BlockId, Vec<u8>>>,
    registry: BlockRegistry,
}

impl BlockManager for MapBlockManager {
    fn block_alloc_size(&self) -> usize {
        BLOCK
    }

    fn block_header_size(&self) -> usize {
        8
    }

    fn in_memory(&self) -> bool {
        false
    }

    fn read(&self, block_id: BlockId, buffer: &mut FileBuffer) -> Result<()> {
        let blocks = self.blocks.lock().unwrap();
        let data = &blocks[&block_id];
        // SAFETY: the buffer is not loaded into a block yet, so it is unshared.
        let target = unsafe { std::slice::from_raw_parts_mut(buffer.data_ptr(), buffer.size()) };
        target.copy_from_slice(&data[8..]);
        Ok(())
    }

    fn write(&self, block_id: BlockId, buffer: &FileBuffer) -> Result<()> {
        // SAFETY: the block is being converted with a single reader (the caller).
        let data = unsafe { std::slice::from_raw_parts(buffer.data_ptr().sub(8), buffer.alloc_size()) };
        self.blocks.lock().unwrap().insert(block_id, data.to_vec());
        Ok(())
    }

    fn registry(&self) -> &BlockRegistry {
        &self.registry
    }
}

#[test]
fn persistent_blocks_convert_evict_and_reload() {
    let buffers = manager(2 * BLOCK, None);
    let block_manager: Arc<dyn BlockManager> = Arc::new(MapBlockManager::default());
    let pin = buffers
        .allocate_for(MemoryTag::InMemoryTable, block_manager.as_ref(), false)
        .unwrap();
    assert_eq!(pin.len(), BLOCK - 8);
    fill(&pin, 42);
    let temporary = Arc::clone(pin.block());
    let persistent = buffers
        .convert_to_persistent(&block_manager, 5, temporary, pin)
        .unwrap();
    assert_eq!(persistent.block_id(), 5);
    assert_eq!(persistent.state(), BlockState::Loaded);
    assert!(Arc::ptr_eq(
        &persistent,
        &buffers.register_block(&block_manager, 5)
    ));
    assert_eq!(buffers.used_memory(), BLOCK as u64);

    // Persistent blocks are evicted by dropping them; pinning re-reads them.
    let a = buffers.allocate(MemoryTag::HashTable, BLOCK, false).unwrap();
    let b = buffers.allocate(MemoryTag::HashTable, BLOCK, false).unwrap();
    assert_eq!(persistent.state(), BlockState::Unloaded);
    drop(a);
    let pin = buffers.pin(&persistent).unwrap();
    check(&pin, 42);
    drop((pin, b));
    drop(persistent);
    assert!(!block_manager.registry().is_registered(5));
}
