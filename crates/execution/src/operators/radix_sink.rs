//! How threads fill their radix-partitioned aggregate hash tables (DuckDB's
//! `RadixHTConfig` and the strategy of `RadixPartitionedHashTable::Sink`), shared by the
//! grouped aggregate's tables and the tables of `DISTINCT` aggregate arguments.
//!
//! Up to [`GROW_STRATEGY_THREAD_THRESHOLD`] threads, each thread's table grows. With more
//! threads every thread keeps a small, cache-resident pointer table that starts over
//! (keeping its rows) when full; after [`ADAPTIVITY_THRESHOLD`] rows a `HyperLogLog` of the
//! groups decides whether lookups are worth it at all or a bigger table would merge many
//! more duplicates. Partitions that fill up raise the radix bits of all threads, so
//! finalizing them can be split further.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

use roastedduck_common::Result;
use roastedduck_row::RadixPartitioning;
use roastedduck_types::STANDARD_VECTOR_SIZE;

use crate::ExecutionContext;
use crate::aggregate_hashtable::{GroupedAggregateHashTable, capacity_for_count, resize_threshold};

/// Up to this many threads, the threads' tables grow; beyond it they keep a small,
/// cache-resident pointer table and start over when it is full
/// (`RadixHTConfig::GROW_STRATEGY_THREAD_THRESHOLD`).
const GROW_STRATEGY_THREAD_THRESHOLD: usize = 2;
/// Rows after which a thread decides whether to adapt its table
/// (`RadixHTLocalSinkState::ADAPTIVITY_THRESHOLD`); also the largest sink capacity.
const ADAPTIVITY_THRESHOLD: usize = 1 << 20;
/// Partitions filled to this share of a block trigger a repartition (`BLOCK_FILL_FACTOR`).
const BLOCK_FILL_FACTOR: f64 = 0.5;
/// Radix bits added per repartition (`REPARTITION_RADIX_BITS`).
const REPARTITION_RADIX_BITS: usize = 2;
/// The most radix bits of the sink (`MAXIMUM_FINAL_SINK_RADIX_BITS`).
const MAXIMUM_FINAL_SINK_RADIX_BITS: usize = 8;

/// The sink configuration of one radix-partitioned table shared by every thread
/// (`RadixHTConfig` with the global sink state's flags).
#[derive(Debug, Default)]
pub(crate) struct RadixSinkConfig {
    /// Number of threads (`number_of_threads`).
    pub threads: usize,
    /// Pointer table capacity the threads' tables start with (`SinkCapacity`).
    pub sink_capacity: usize,
    /// The most radix bits the sink grows to while in memory (`maximum_sink_radix_bits`).
    pub maximum_sink_radix_bits: usize,
    /// The radix bits of an out-of-core aggregation.
    pub external_radix_bits: usize,
    /// Size of a block, which partitions should fill before more are added.
    pub block_alloc_size: usize,
    /// The radix bits every thread's data ends up with; raised as partitions fill or to
    /// `external_radix_bits` once a thread runs out of its memory share
    /// (`SetRadixBitsToExternal`).
    pub radix_bits: AtomicUsize,
    /// Whether the aggregation went out of core (`external`).
    pub external: AtomicBool,
    /// Whether a thread has combined, after which the radix bits stay (`any_combined`).
    pub any_combined: AtomicBool,
}

/// The radix bits threads start with: a partition per thread, to finalize them on every
/// thread (`RadixHTConfig::InitialSinkRadixBits`); a single thread keeps one partition, so
/// its groups come out in the order they arrived.
pub(crate) fn initial_radix_bits(ctx: &ExecutionContext) -> usize {
    RadixPartitioning::radix_bits_of_power_of_two(ctx.env.threads().next_power_of_two()).min(4)
}

impl RadixSinkConfig {
    /// The configuration of a table whose rows are `row_width` bytes wide
    /// (`RadixHTConfig` construction); `external_radix_bits` is the out-of-core bits.
    pub fn new(ctx: &ExecutionContext, row_width: usize, external_radix_bits: usize) -> Self {
        let threads = ctx.env.threads().max(1);
        let initial_bits = initial_radix_bits(ctx);
        Self {
            threads,
            sink_capacity: sink_capacity(threads),
            maximum_sink_radix_bits: maximum_sink_radix_bits(threads, row_width, initial_bits)
                .min(external_radix_bits),
            external_radix_bits,
            block_alloc_size: ctx.env.buffer_manager.block_alloc_size(),
            radix_bits: AtomicUsize::new(initial_bits),
            external: AtomicBool::new(false),
            any_combined: AtomicBool::new(false),
        }
    }

    /// Whether the threads' tables grow instead of starting over.
    pub fn grows(&self) -> bool {
        self.threads <= GROW_STRATEGY_THREAD_THRESHOLD
    }
}

/// `RadixHTConfig::SinkCapacity`
fn sink_capacity(threads: usize) -> usize {
    if threads <= GROW_STRATEGY_THREAD_THRESHOLD {
        // Grow strategy: start off a bit bigger.
        262_144
    } else {
        // Start off tiny; `decide_adaptation` may grow it later.
        32_768
    }
}

/// `RadixHTConfig::MaximumSinkRadixBits`: wide rows get fewer partitions, otherwise
/// cache misses get out of hand.
fn maximum_sink_radix_bits(threads: usize, row_width: usize, initial_bits: usize) -> usize {
    if threads <= GROW_STRATEGY_THREAD_THRESHOLD {
        return initial_bits; // Don't repartition unless we go external.
    }
    let bits = if row_width >= 64 {
        MAXIMUM_FINAL_SINK_RADIX_BITS - 2
    } else if row_width >= 32 {
        MAXIMUM_FINAL_SINK_RADIX_BITS - 1
    } else {
        MAXIMUM_FINAL_SINK_RADIX_BITS
    };
    bits.max(initial_bits)
}

/// One thread's table and where it is in the sink strategy (`RadixHTLocalSinkState`).
#[derive(Debug)]
pub(crate) struct RadixSinkTable {
    pub table: GroupedAggregateHashTable,
    /// The pointer table capacity (`local_sink_capacity`).
    capacity: usize,
    /// Whether the strategy was adapted to the data (`adapted`).
    adapted: bool,
}

impl RadixSinkTable {
    /// A thread's table under `config` (created with `config.sink_capacity`).
    pub fn new(config: &RadixSinkConfig, mut table: GroupedAggregateHashTable) -> Self {
        let grow = config.grows();
        // Beyond the grow-strategy thread count, the tables count distinct groups so that
        // they can adapt their strategy later.
        table.enable_hll(!grow);
        Self {
            table,
            capacity: config.sink_capacity,
            adapted: grow,
        }
    }

    /// The strategy after each chunk (the rest of DuckDB's `RadixPartitionedHashTable::Sink`):
    /// adapt once enough rows were seen, and when the pointer table is full either let it
    /// grow (few threads) or start a new one, raising the radix bits as the partitions
    /// fill (`MaybeRepartition`).
    pub fn after_add_chunk(&mut self, config: &RadixSinkConfig) -> Result<()> {
        let table = &mut self.table;
        let external = config.external.load(Ordering::Acquire);
        if !self.adapted && table.sink_count() >= ADAPTIVITY_THRESHOLD {
            if !external {
                Self::decide_adaptation(config, table, &mut self.capacity)?;
            }
            table.enable_hll(false);
            self.adapted = true;
        }
        if table.count() + STANDARD_VECTOR_SIZE < resize_threshold(self.capacity) {
            return Ok(()); // Another chunk fits.
        }
        let grow = config.grows();
        if !grow || external {
            // Start a new pointer table and keep appending to the same rows; this works
            // because the table never grows.
            table.abandon();
        }
        if grow {
            return Ok(()); // Repartitioning only happens when going external.
        }
        let current_bits = table.radix_bits();
        let partitions = 1usize << current_bits;
        let row_size_per_partition = table.materialized_count() * table.layout().row_width() / partitions;
        if row_size_per_partition as f64 > BLOCK_FILL_FACTOR * config.block_alloc_size as f64
            && !config.any_combined.load(Ordering::Acquire)
        {
            let bits = (current_bits + REPARTITION_RADIX_BITS).min(config.maximum_sink_radix_bits);
            config.radix_bits.fetch_max(bits, Ordering::AcqRel);
        }
        let global_bits = config.radix_bits.load(Ordering::Acquire);
        if current_bits < global_bits {
            table.repartition(global_bits)?;
            if external {
                table.resize_to(self.capacity)?;
            }
        }
        Ok(())
    }

    /// Picks a better strategy from the distinct groups seen so far (`DecideAdaptation`):
    /// skip the lookups when nearly every row is a new group, or grow the pointer table
    /// when a bigger one would merge many more duplicates.
    fn decide_adaptation(
        config: &RadixSinkConfig,
        table: &mut GroupedAggregateHashTable,
        capacity: &mut usize,
    ) -> Result<()> {
        const SKIP_LOOKUP_UNIQUE_PERCENTAGE_THRESHOLD: f64 = 0.95;
        const CAPACITY_INCREASE_DEDUPLICATION_RATE_THRESHOLD: f64 = 2.0;
        let sink_count = table.sink_count() as f64;
        let deduplicated_count = table.materialized_count();
        let hll_count = table.hll_upper_bound().min(deduplicated_count);
        let deduplicated_percentage = deduplicated_count as f64 / sink_count;
        let hll_percentage = hll_count as f64 / sink_count;
        if hll_percentage > SKIP_LOOKUP_UNIQUE_PERCENTAGE_THRESHOLD {
            // Almost everything is unique: just append, deduplicate when combining.
            table.skip_lookups();
            return Ok(());
        }
        if deduplicated_percentage / hll_percentage > CAPACITY_INCREASE_DEDUPLICATION_RATE_THRESHOLD {
            let new_capacity = capacity_for_count(hll_count).min(ADAPTIVITY_THRESHOLD);
            *capacity = config.sink_capacity.max(new_capacity);
            table.abandon();
            table.resize_to(*capacity)?;
        }
        Ok(())
    }
}

/// A radix-partitioned table's claim on temporary memory from its sink through its
/// source (DuckDB's `RadixHTGlobalSinkState::temporary_memory_state`): while sinking it
/// reserves what the threads' tables need and decides when a thread's tables go out of
/// core (`MaybeRepartition`); once finalized, what combining partitions needs at the
/// least, and how many partitions the source may combine at once (`MaxThreads`).
#[derive(Debug)]
pub(crate) struct RadixMemory {
    pub state: roastedduck_buffer::TemporaryMemoryState,
    context: roastedduck_buffer::TemporaryMemoryContext,
    /// What the threads' tables need at the least: a sink-capacity table per thread with
    /// its partitions' blocks, and more partitions once out of core.
    minimum_reservation: AtomicU64,
    threads: u64,
    /// Serializes the reservation updates (`gstate.Lock()`).
    lock: Mutex<()>,
    /// The bytes of the largest partition and the table that combines it, once finalized
    /// (`max_partition_size`).
    max_partition_size: AtomicU64,
    /// The partitions, and those combined so far (`finalize_done`).
    partition_count: AtomicUsize,
    finalize_done: AtomicUsize,
}

/// `GetCapacityForCount(count) * sizeof(ht_entry_t)`: the pointer table that combines
/// `count` groups.
fn pointer_table_size(count: usize) -> u64 {
    (capacity_for_count(count) * size_of::<u64>()) as u64
}

impl RadixMemory {
    /// Registers a table whose threads fill tables under `config` with rows of
    /// `row_width` bytes (`RadixHTGlobalSinkState` construction): it reserves a table of
    /// the sink capacity per thread, with a block or two per partition.
    pub fn new(
        ctx: &ExecutionContext,
        config: &RadixSinkConfig,
        row_width: usize,
        all_constant: bool,
    ) -> Self {
        let buffer_manager = &ctx.env.buffer_manager;
        let threads = ctx.env.threads().max(1) as u64;
        let context = buffer_manager.temporary_memory_context(threads, 1, ctx.force_external());
        let state = buffer_manager.temporary_memory_manager().register(&context);
        let block_alloc_size = buffer_manager.block_alloc_size() as u64;
        let tuples_per_block = (block_alloc_size / row_width.max(1) as u64).max(1);
        let ht_count = resize_threshold(config.sink_capacity) as u64;
        let partitions = 1u64 << config.maximum_sink_radix_bits;
        let count_per_partition = ht_count / partitions;
        let mut blocks_per_partition = (count_per_partition + tuples_per_block) / tuples_per_block;
        if !all_constant {
            blocks_per_partition += 1;
        }
        let ht_size = partitions * blocks_per_partition * block_alloc_size
            + (config.sink_capacity * size_of::<u64>()) as u64;
        let minimum_reservation = threads * ht_size;
        state.set_minimum_reservation(minimum_reservation);
        state.set_remaining_size_and_update_reservation(&context, minimum_reservation);
        Self {
            state,
            context,
            minimum_reservation: AtomicU64::new(minimum_reservation),
            threads,
            lock: Mutex::default(),
            max_partition_size: AtomicU64::new(0),
            partition_count: AtomicUsize::new(0),
            finalize_done: AtomicUsize::new(0),
        }
    }

    /// Registers a table that reserves nothing until it is finalized: a `DISTINCT`
    /// aggregate's table, whose sink the aggregate's own claim covers.
    pub fn unreserved(ctx: &ExecutionContext) -> Self {
        let buffer_manager = &ctx.env.buffer_manager;
        let threads = ctx.env.threads().max(1) as u64;
        let context = buffer_manager.temporary_memory_context(threads, 1, ctx.force_external());
        Self {
            state: buffer_manager.temporary_memory_manager().register(&context),
            context,
            minimum_reservation: AtomicU64::new(0),
            threads,
            lock: Mutex::default(),
            max_partition_size: AtomicU64::new(0),
            partition_count: AtomicUsize::new(0),
            finalize_done: AtomicUsize::new(0),
        }
    }

    fn guard(&self) -> MutexGuard<'_, ()> {
        self.lock.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// `GetThreadLimit`: how much a thread's tables may hold before they go out of core.
    pub fn thread_limit(&self) -> usize {
        usize::try_from(self.state.reservation() / self.threads / 10 * 8).unwrap_or(usize::MAX)
    }

    /// Whether a thread whose tables hold `size` bytes must go out of core: it is over its
    /// share of the reservation even after asking for twice what the threads need
    /// (`MaybeRepartition`).
    pub fn over_thread_limit(&self, external: bool, size: usize) -> bool {
        if size <= self.thread_limit() {
            return false;
        }
        if !external {
            let _guard = self.guard();
            if size > self.thread_limit() {
                self.state
                    .set_minimum_reservation(self.minimum_reservation.load(Ordering::Acquire));
                let remaining = (self.threads * size as u64).max(self.state.remaining_size());
                self.state
                    .set_remaining_size_and_update_reservation(&self.context, 2 * remaining);
            }
        }
        size > self.thread_limit()
    }

    /// The threads fill `multiplier` times as many partitions: the minimum grows with them
    /// (`SetRadixBitsInternal`).
    pub fn scale_minimum_reservation(&self, multiplier: u64) {
        self.minimum_reservation
            .try_update(Ordering::AcqRel, Ordering::Acquire, |minimum| {
                Some(minimum.saturating_mul(multiplier))
            })
            .ok();
    }

    /// The sink is finalized into `partitions`, `(bytes, groups, combined)` each, where
    /// combined partitions need no combining (`Finalize`): combining at least one
    /// partition at a time needs its rows and its table; nothing is reserved until the
    /// source starts. The aggregate states' heap memory is not kept in arena allocators
    /// here, so DuckDB's `stored_allocators_size` is zero.
    pub fn finalize(&self, partitions: impl IntoIterator<Item = (usize, usize, bool)>) {
        let _guard = self.guard();
        let mut max_partition_size = 0;
        let mut count = 0;
        let mut combined = 0;
        for (bytes, groups, already_combined) in partitions {
            max_partition_size = max_partition_size.max(bytes as u64 + pointer_table_size(groups));
            count += 1;
            combined += usize::from(already_combined);
        }
        self.max_partition_size
            .store(max_partition_size, Ordering::Release);
        self.partition_count.store(count, Ordering::Release);
        self.finalize_done.store(combined, Ordering::Release);
        self.state.set_minimum_reservation(max_partition_size);
        self.state.set_zero();
    }

    /// How many partitions the source may combine at once (`MaxThreads`): as many as fit
    /// in the reservation it asks for, at most one per thread, at least one; zero without
    /// partitions.
    pub fn max_threads(&self, threads: usize) -> usize {
        let partitions = self.partition_count.load(Ordering::Acquire);
        if partitions == 0 {
            return 0;
        }
        let max_threads = threads.min(partitions);
        let max_partition_size = self.max_partition_size.load(Ordering::Acquire);
        self.state.set_remaining_size_and_update_reservation(
            &self.context,
            max_threads as u64 * max_partition_size,
        );
        let usable_memory = self.state.reservation();
        let partitions_fit = usize::try_from(usable_memory / max_partition_size.max(1))
            .unwrap_or(usize::MAX)
            .max(1);
        partitions_fit.min(max_threads)
    }

    /// A partition was combined; once all were, nothing more is needed (`SetZero`).
    pub fn partition_finalized(&self) {
        let done = self.finalize_done.fetch_add(1, Ordering::AcqRel) + 1;
        if done == self.partition_count.load(Ordering::Acquire) {
            let _guard = self.guard();
            self.state.set_zero();
        }
    }
}
