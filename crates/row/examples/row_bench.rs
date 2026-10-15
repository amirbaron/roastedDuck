//! Micro-benchmarks of the row layer: scatter/gather, radix partitioning and sorting.
//!
//! ```text
//! cargo run --release -p rustdb-row --example row_bench -- [rows] [threads]
//! ```
//! Defaults: 10M rows, all cores. Compare with the DuckDB CLI queries printed per case.

use std::sync::Arc;
use std::time::Instant;

use rustdb_buffer::{BufferManager, BufferManagerConfig, MemoryTag};
use rustdb_common::{Result, STANDARD_VECTOR_SIZE};
use rustdb_row::sort::{NullOrder, OrderModifiers, OrderType, Sort, SortColumn, SortConfig};
use rustdb_row::{PartitionedTupleData, PinProperties, TupleDataCollection, TupleDataLayout, ValidityType};
use rustdb_types::{DataChunk, LogicalType, StringT, Vector};

/// `SplitMix64`, as the reference data generator.
fn mix(i: u64) -> u64 {
    let mut z = i.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

fn bigint_column(start: usize, count: usize, salt: u64) -> Vector {
    let mut vector = Vector::flat(LogicalType::BIGINT, count);
    for (i, value) in vector.values_mut::<i64>()[..count].iter_mut().enumerate() {
        *value = (mix((start + i) as u64 ^ salt) >> 1) as i64;
    }
    vector
}

fn ubigint_hash_column(start: usize, count: usize) -> Vector {
    let mut vector = Vector::flat(LogicalType::UBIGINT, count);
    for (i, value) in vector.values_mut::<u64>()[..count].iter_mut().enumerate() {
        *value = mix((start + i) as u64);
    }
    vector
}

fn double_column(start: usize, count: usize) -> Vector {
    let mut vector = Vector::flat(LogicalType::DOUBLE, count);
    for (i, value) in vector.values_mut::<f64>()[..count].iter_mut().enumerate() {
        *value = (mix((start + i) as u64 ^ 7) % 1_000_000) as f64 / 7.0;
    }
    vector
}

/// Strings of 5..=40 lowercase letters ('a' + digits of the hash).
fn varchar_column(start: usize, count: usize) -> Vector {
    let mut vector = Vector::flat(LogicalType::VARCHAR, count);
    let mut buffer = Vec::with_capacity(64);
    for i in 0..count {
        let mut h = mix((start + i) as u64 ^ 0xabc);
        let len = 5 + (h % 36) as usize;
        buffer.clear();
        for _ in 0..len {
            h = mix(h);
            buffer.push(b'a' + (h % 26) as u8);
        }
        let string = vector.add_string(&buffer);
        vector.values_mut::<StringT>()[i] = string;
    }
    vector
}

fn chunks(rows: usize, build: impl Fn(usize, usize) -> Vec<Vector>) -> Vec<DataChunk> {
    (0..rows)
        .step_by(STANDARD_VECTOR_SIZE)
        .map(|start| {
            let count = STANDARD_VECTOR_SIZE.min(rows - start);
            DataChunk::new(build(start, count), count)
        })
        .collect()
}

/// `ROW_BENCH=<substring>` runs only the matching cases.
fn selected(name: &str) -> bool {
    std::env::var("ROW_BENCH").map_or(true, |filter| name.contains(&filter))
}

/// User + system CPU time of the process, in seconds (wall time is unreliable on a busy
/// machine; DuckDB's `.timer on` reports both too).
fn cpu_seconds() -> f64 {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    // SAFETY: `getrusage` fills the struct it is given.
    let usage = unsafe {
        libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr());
        usage.assume_init()
    };
    let seconds = |time: libc::timeval| time.tv_sec as f64 + f64::from(time.tv_usec) / 1e6;
    seconds(usage.ru_utime) + seconds(usage.ru_stime)
}

/// A wall-clock and CPU stopwatch.
struct Timer {
    wall: Instant,
    cpu: f64,
}

impl Timer {
    fn start() -> Self {
        Self {
            wall: Instant::now(),
            cpu: cpu_seconds(),
        }
    }

    fn report(&self, name: &str, rows: usize) {
        let wall = self.wall.elapsed().as_secs_f64();
        let cpu = cpu_seconds() - self.cpu;
        println!(
            "{name:<48} wall {wall:>7.3} s  cpu {cpu:>7.3} s  {:>7.1} M rows/s (wall)",
            rows as f64 / wall / 1e6
        );
    }
}

fn scatter_gather(bm: &BufferManager, name: &str, data: &[DataChunk], types: Vec<LogicalType>) -> Result<()> {
    if !selected(&format!("scatter {name}")) {
        return Ok(());
    }
    let rows: usize = data.iter().map(DataChunk::len).sum();
    let layout = Arc::new(TupleDataLayout::new(types, ValidityType::CanHaveNull)?);
    let timer = Timer::start();
    let mut collection = TupleDataCollection::new(bm.clone(), layout, MemoryTag::HashTable)?;
    let mut append = collection.initialize_append(PinProperties::UnpinAfterDone);
    for chunk in data {
        collection.append(&mut append, chunk)?;
    }
    collection.finalize_pin_state(&mut append.pin_state);
    timer.report(&format!("scatter {name}"), rows);
    let timer = Timer::start();
    let mut scan = collection.initialize_scan(PinProperties::UnpinAfterDone);
    let mut chunk = DataChunk::default();
    let mut scanned = 0;
    while collection.scan(&mut scan, &mut chunk)? {
        scanned += chunk.len();
    }
    assert_eq!(scanned, rows);
    timer.report(&format!("gather  {name}"), rows);
    Ok(())
}

fn partition(bm: &BufferManager, data: &[DataChunk], radix_bits: usize, threads: usize) -> Result<()> {
    if !selected("partition") {
        return Ok(());
    }
    let rows: usize = data.iter().map(DataChunk::len).sum();
    let types = vec![LogicalType::BIGINT, LogicalType::VARCHAR, LogicalType::UBIGINT];
    let layout = Arc::new(TupleDataLayout::new(types, ValidityType::CanHaveNull)?);
    let timer = Timer::start();
    let results: Vec<Result<PartitionedTupleData>> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..threads)
            .map(|thread| {
                let layout = Arc::clone(&layout);
                scope.spawn(move || {
                    let mut partitioned =
                        PartitionedTupleData::new(bm.clone(), layout, MemoryTag::HashTable, radix_bits, 2)?;
                    let mut state = partitioned.initialize_append_state(PinProperties::UnpinAfterDone);
                    for chunk in data.iter().skip(thread).step_by(threads) {
                        partitioned.append(&mut state, chunk)?;
                    }
                    partitioned.flush_append_state(&mut state);
                    Ok(partitioned)
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap_or_else(|_| std::process::abort()))
            .collect()
    });
    let mut total = 0;
    for result in results {
        total += result?.count();
    }
    assert_eq!(total, rows);
    timer.report(
        &format!("partition {} ways, {threads} threads", 1 << radix_bits),
        rows,
    );
    Ok(())
}

fn sort(
    bm: &BufferManager,
    name: &str,
    data: &[DataChunk],
    types: &[LogicalType],
    orders: Vec<SortColumn>,
    threads: usize,
) -> Result<()> {
    if !selected(&format!("sort {name}, {threads} threads")) {
        return Ok(());
    }
    let rows: usize = data.iter().map(DataChunk::len).sum();
    let timer = Timer::start();
    let sort = Sort::new(bm.clone(), orders, types, None)?;
    let global = sort.global_sink_state(SortConfig {
        threads,
        ..SortConfig::default()
    });
    std::thread::scope(|scope| -> Result<()> {
        let handles: Vec<_> = (0..threads)
            .map(|thread| {
                let (sort, global) = (&sort, &global);
                scope.spawn(move || -> Result<()> {
                    let mut local = sort.local_sink_state();
                    for chunk in data.iter().skip(thread).step_by(threads) {
                        sort.sink(chunk, global, &mut local)?;
                    }
                    sort.combine(global, local)
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap_or_else(|_| std::process::abort())?;
        }
        Ok(())
    })?;
    let sink_time = timer.wall.elapsed().as_secs_f64();
    let Some(source) = sort.finalize(global)? else {
        return Ok(());
    };
    let produced: usize = std::thread::scope(|scope| -> Result<usize> {
        let handles: Vec<_> = (0..threads)
            .map(|_| {
                let source = &source;
                scope.spawn(move || -> Result<usize> {
                    let mut local = source.local_state();
                    let mut chunk = DataChunk::default();
                    let mut count = 0;
                    while source.get_data(&mut chunk, &mut local)? {
                        count += chunk.len();
                    }
                    Ok(count)
                })
            })
            .collect();
        let mut total = 0;
        for handle in handles {
            total += handle.join().unwrap_or_else(|_| std::process::abort())?;
        }
        Ok(total)
    })?;
    assert_eq!(produced, rows);
    let total = timer.wall.elapsed().as_secs_f64();
    timer.report(&format!("sort {name}, {threads} threads"), rows);
    println!(
        "{:<48} (wall: sink+run sort {sink_time:.3} s, merge+scan {:.3} s)",
        "",
        total - sink_time
    );
    Ok(())
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let rows: usize = args.next().and_then(|arg| arg.parse().ok()).unwrap_or(10_000_000);
    let cores = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    let threads: usize = args.next().and_then(|arg| arg.parse().ok()).unwrap_or(cores);
    let bm = BufferManager::new(BufferManagerConfig {
        memory_limit: Some(16 << 30),
        ..BufferManagerConfig::default()
    })?;
    println!("rows: {rows}, threads: {threads}");

    let ints = chunks(rows, |start, count| vec![bigint_column(start, count, 0)]);
    let mixed = chunks(rows, |start, count| {
        vec![
            bigint_column(start, count, 1),
            varchar_column(start, count),
            ubigint_hash_column(start, count),
        ]
    });
    if selected("keys") {
        let timer = Timer::start();
        let asc = OrderModifiers::new(OrderType::Ascending, NullOrder::NullsLast);
        let mut total = 0;
        for chunk in &ints {
            total += rustdb_row::sort::create_sort_keys(&[chunk.column(0)], &[asc], chunk.len())?.len();
        }
        timer.report("create_sort_keys BIGINT", total);
    }
    scatter_gather(&bm, "BIGINT", &ints, vec![LogicalType::BIGINT])?;
    scatter_gather(
        &bm,
        "BIGINT, VARCHAR, UBIGINT",
        &mixed,
        vec![LogicalType::BIGINT, LogicalType::VARCHAR, LogicalType::UBIGINT],
    )?;
    for bits in [4, 8] {
        partition(&bm, &mixed, bits, 1)?;
        partition(&bm, &mixed, bits, threads)?;
    }

    let asc = OrderModifiers::new(OrderType::Ascending, NullOrder::NullsLast);
    let desc = OrderModifiers::new(OrderType::Descending, NullOrder::NullsLast);
    let column = |column, modifiers| SortColumn {
        column,
        modifiers,
        stats: None,
    };
    for sort_threads in [1, threads] {
        sort(
            &bm,
            "BIGINT",
            &ints,
            &[LogicalType::BIGINT],
            vec![column(0, asc)],
            sort_threads,
        )?;
        let strings = chunks(rows, |start, count| vec![varchar_column(start, count)]);
        sort(
            &bm,
            "VARCHAR",
            &strings,
            &[LogicalType::VARCHAR],
            vec![column(0, asc)],
            sort_threads,
        )?;
        let multi = chunks(rows, |start, count| {
            vec![
                {
                    let mut small = bigint_column(start, count, 3);
                    for value in &mut small.values_mut::<i64>()[..count] {
                        *value %= 100;
                    }
                    small
                },
                varchar_column(start, count),
                double_column(start, count),
            ]
        });
        sort(
            &bm,
            "BIGINT%100, VARCHAR DESC, DOUBLE",
            &multi,
            &[LogicalType::BIGINT, LogicalType::VARCHAR, LogicalType::DOUBLE],
            vec![column(0, asc), column(1, desc), column(2, asc)],
            sort_threads,
        )?;
    }
    Ok(())
}
