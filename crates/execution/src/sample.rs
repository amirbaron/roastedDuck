//! Reservoir sampling (DuckDB's `execution/sample/`): `USING SAMPLE n ROWS` keeps a
//! uniform sample of `n` rows, `USING SAMPLE p% (reservoir)` one per block of rows.
//!
//! Small inputs are sampled by random replacement ("fast" sampling); once enough rows
//! have been seen the sample switches to weighted reservoir sampling (Efraimidis and
//! Spirakis, "Weighted Random Sampling", 2005) with uniform weights. The random draws,
//! including libc++'s `std::shuffle`, follow DuckDB exactly, so a `REPEATABLE (seed)`
//! sample selects the same rows.

use std::cmp::Ordering;
use std::collections::BinaryHeap;

use roastedduck_common::{Result, STANDARD_VECTOR_SIZE};
use roastedduck_function::random::RandomEngine;
use roastedduck_types::{DataChunk, LogicalType, Vector};

/// The sample size that switches fast sampling to reservoir sampling, as a number of
/// rows seen divided by [`FIXED_SAMPLE_SIZE`] (`FAST_TO_SLOW_THRESHOLD`).
const FAST_TO_SLOW_THRESHOLD: usize = 60;
/// DuckDB's `FIXED_SAMPLE_SIZE`.
const FIXED_SAMPLE_SIZE: usize = 2048;
/// Spare capacity of the sample chunk, in multiples of the sample size
/// (`FIXED_SAMPLE_SIZE_MULTIPLIER`).
const FIXED_SAMPLE_SIZE_MULTIPLIER: usize = 10;
/// Rows per reservoir of a percentage sample (`RESERVOIR_THRESHOLD`).
const RESERVOIR_THRESHOLD: usize = 100_000;

/// A reservoir entry: the negated weight and the index into the sample's selection, in
/// a max-heap whose top is the entry with the smallest weight (DuckDB's
/// `std::priority_queue<std::pair<double, idx_t>>`).
#[derive(Clone, Copy, Debug)]
struct WeightEntry {
    negated_weight: f64,
    index: usize,
}

impl PartialEq for WeightEntry {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for WeightEntry {}

impl PartialOrd for WeightEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for WeightEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        self.negated_weight
            .total_cmp(&other.negated_weight)
            .then(self.index.cmp(&other.index))
    }
}

/// The weights and skip counters of weighted reservoir sampling (DuckDB's
/// `BaseReservoirSampling`).
#[derive(Debug)]
struct BaseReservoirSampling {
    random: RandomEngine,
    weights: BinaryHeap<WeightEntry>,
    next_index_to_sample: u64,
    min_weight_threshold: f64,
    min_weighted_entry_index: usize,
    num_entries_to_skip_b4_next_sample: u64,
    num_entries_seen_total: u64,
}

impl BaseReservoirSampling {
    fn new(seed: u64) -> Self {
        Self {
            random: RandomEngine::new(seed),
            weights: BinaryHeap::new(),
            next_index_to_sample: 0,
            min_weight_threshold: 0.0,
            min_weighted_entry_index: 0,
            num_entries_to_skip_b4_next_sample: 0,
            num_entries_seen_total: 0,
        }
    }

    /// Draws how many rows to skip before the next replacement (`SetNextEntry`).
    fn set_next_entry(&mut self) {
        let Some(&top) = self.weights.peek() else {
            return;
        };
        let threshold = -top.negated_weight;
        let r = self.random.next_random32();
        let x_w = r.ln() / threshold.ln();
        self.min_weight_threshold = threshold;
        self.min_weighted_entry_index = top.index;
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "idx_t(ceil(x_w))"
        )]
        let next = if x_w >= 1.0 && x_w.is_finite() {
            x_w.ceil() as u64
        } else {
            1
        };
        self.next_index_to_sample = next;
        self.num_entries_to_skip_b4_next_sample = 0;
    }

    /// `ReplaceElementWithIndex(entry_index, with_weight, pop = false)`.
    fn replace_element_with_index(&mut self, entry_index: usize, with_weight: f64) {
        self.weights.push(WeightEntry {
            negated_weight: -with_weight,
            index: entry_index,
        });
        self.set_next_entry();
    }

    /// `UpdateMinWeightThreshold`.
    fn update_min_weight_threshold(&mut self) {
        if let Some(top) = self.weights.peek() {
            self.min_weight_threshold = -top.negated_weight;
            self.min_weighted_entry_index = top.index;
            return;
        }
        self.min_weight_threshold = 1.0;
    }

    /// Gives every sampled row a weight: the top `count` order statistics of
    /// `num_entries_seen_total` uniform draws, shuffled (`FillWeights`).
    fn fill_weights(&mut self, count: usize) {
        if !self.weights.is_empty() {
            return;
        }
        let mut weights = Vec::with_capacity(count);
        let mut bound = 1.0;
        for i in 0..count {
            // A sample of Beta(n - i, 1).
            #[allow(clippy::cast_precision_loss, reason = "double(n - i)")]
            let exponent = 1.0 / (self.num_entries_seen_total - i as u64) as f64;
            bound *= self.random.next_random().powf(exponent);
            weights.push(bound);
        }
        self.random.libcxx_shuffle(&mut weights);
        for (index, weight) in weights.into_iter().enumerate() {
            // Clamped like DuckDB to avoid zero weights from underflow.
            self.weights.push(WeightEntry {
                negated_weight: -weight.max(f64::MIN_POSITIVE),
                index,
            });
        }
        self.set_next_entry();
    }
}

/// A sample of up to `sample_count` rows (DuckDB's `ReservoirSample`).
#[derive(Debug)]
pub struct ReservoirSample {
    sample_count: usize,
    base: BaseReservoirSampling,
    /// The sampled rows and rows kept for later replacement; `sel` maps sample slots to
    /// its rows.
    reservoir: Option<Vec<Vector>>,
    reservoir_len: usize,
    types: Vec<LogicalType>,
    sel: Vec<u32>,
    sel_size: usize,
}

impl ReservoirSample {
    pub fn new(sample_count: usize, seed: u64) -> Self {
        Self {
            sample_count,
            base: BaseReservoirSampling::new(seed),
            reservoir: None,
            reservoir_len: 0,
            types: Vec::new(),
            sel: vec![0; sample_count],
            sel_size: 0,
        }
    }

    /// Whether weighted reservoir sampling is in use (`SamplingState::RESERVOIR`).
    fn is_weighted(&self) -> bool {
        !self.base.weights.is_empty()
    }

    fn active_sample_count(&self) -> usize {
        if self.is_weighted() {
            self.base.weights.len()
        } else {
            self.sel_size
        }
    }

    fn capacity(&self) -> usize {
        self.sample_count + FIXED_SAMPLE_SIZE_MULTIPLIER * self.sample_count.min(FIXED_SAMPLE_SIZE)
    }

    /// A random permutation prefix of `0..range` (`GetRandomizedVector`).
    fn randomized_vector(&mut self, range: u32, size: u32) -> Vec<u32> {
        let mut result: Vec<u32> = (0..range).collect();
        for i in 0..size {
            let shuffle = self.base.random.next_random_integer32_in(i, range);
            if shuffle != i {
                result.swap(i as usize, shuffle as usize);
            }
        }
        result
    }

    /// Appends rows `rows` of `chunk` to the reservoir (`UpdateSampleAppend`).
    fn append_rows(&mut self, chunk: &DataChunk, rows: &[u32]) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let capacity = self.capacity().max(self.reservoir_len + rows.len());
        let reservoir = self.reservoir.get_or_insert_with(|| {
            self.types
                .iter()
                .map(|ty| Vector::flat(ty.clone(), capacity))
                .collect()
        });
        for (column, source) in reservoir.iter_mut().zip(chunk.columns()) {
            if column.capacity() < self.reservoir_len + rows.len() {
                let mut grown = Vector::flat(column.logical_type().clone(), capacity);
                for row in 0..self.reservoir_len {
                    grown.set_value(row, &column.get_value(row))?;
                }
                *column = grown;
            }
            for (offset, &row) in rows.iter().enumerate() {
                column.set_value(self.reservoir_len + offset, &source.get_value(row as usize))?;
            }
        }
        self.reservoir_len += rows.len();
        Ok(())
    }

    /// Adds rows until the sample is full; returns how many were taken (`FillReservoir`).
    fn fill_reservoir(&mut self, chunk: &DataChunk) -> Result<usize> {
        if self.reservoir.is_none() {
            self.types = chunk.columns().iter().map(|c| c.logical_type().clone()).collect();
        }
        let start = self.active_sample_count();
        if self.reservoir_len >= self.sample_count {
            return Ok(0);
        }
        let ingested = (self.sample_count - self.reservoir_len).min(chunk.len());
        #[allow(
            clippy::cast_possible_truncation,
            reason = "chunks hold at most a vector of rows"
        )]
        let random = self.randomized_vector(ingested as u32, ingested as u32);
        for i in 0..ingested {
            #[allow(clippy::cast_possible_truncation, reason = "sample indexes fit u32")]
            {
                self.sel[start + i] = (start + i) as u32;
            }
        }
        self.append_rows(chunk, &random[..ingested])?;
        self.sel_size += ingested;
        Ok(ingested)
    }

    /// Rows of `chunk_length` new rows to take into the sample, replacing random sampled
    /// rows (`GetReplacementIndexesFast`).
    fn replacement_indexes_fast(&mut self, sample_offset: usize, chunk_length: usize) -> Vec<u32> {
        #[allow(clippy::cast_precision_loss, reason = "as DuckDB")]
        let weight_other =
            chunk_length as f64 / (self.base.num_entries_seen_total + chunk_length as u64) as f64;
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss,
            reason = "round to a count"
        )]
        let num_to_pop = (weight_other * self.sample_count as f64).round() as u32;
        if num_to_pop == 0 {
            return Vec::new();
        }
        #[allow(clippy::cast_possible_truncation, reason = "chunk and sample sizes fit u32")]
        let chunk_indexes = self.randomized_vector(chunk_length as u32, num_to_pop);
        #[allow(clippy::cast_possible_truncation, reason = "sample sizes fit u32")]
        let sel_indexes = self.randomized_vector(self.sel_size as u32, num_to_pop);
        let mut result = Vec::with_capacity(num_to_pop as usize);
        for i in 0..num_to_pop as usize {
            result.push(chunk_indexes[i]);
            #[allow(clippy::cast_possible_truncation, reason = "reservoir offsets fit u32")]
            {
                self.sel[sel_indexes[i] as usize] = (sample_offset + i) as u32;
            }
        }
        result
    }

    /// Rows of the new chunk that replace the lightest sampled rows
    /// (`GetReplacementIndexesSlow`).
    fn replacement_indexes_slow(&mut self, sample_offset: usize, chunk_length: usize) -> Vec<u32> {
        let mut remaining = chunk_length as u64;
        // (row in the chunk, position in the returned selection), in order of discovery.
        let mut replacements: Vec<(u64, usize)> = Vec::new();
        let mut base_offset = 0u64;
        loop {
            let offset = self.base.next_index_to_sample - self.base.num_entries_to_skip_b4_next_sample;
            if offset >= remaining {
                self.base.num_entries_to_skip_b4_next_sample += remaining;
                break;
            }
            let sample_index = replacements.len();
            let row = base_offset + offset;
            if let Some(entry) = replacements.iter_mut().find(|(existing, _)| *existing == row) {
                entry.1 = sample_index;
            } else {
                replacements.push((row, sample_index));
            }
            let r2 = self
                .base
                .random
                .next_random32_in(self.base.min_weight_threshold, 1.0);
            let Some(top) = self.base.weights.pop() else { break };
            self.base.update_min_weight_threshold();
            #[allow(clippy::cast_possible_truncation, reason = "reservoir offsets fit u32")]
            {
                self.sel[top.index] = (sample_offset + sample_index) as u32;
            }
            self.base.replace_element_with_index(top.index, r2);
            remaining -= offset;
            base_offset += offset;
        }
        let mut result = vec![0u32; replacements.len()];
        for (row, position) in replacements {
            #[allow(clippy::cast_possible_truncation, reason = "rows of one chunk")]
            {
                result[position] = row as u32;
            }
        }
        result
    }

    /// Switches to weighted reservoir sampling (`ConvertToReservoirSample`).
    fn convert_to_reservoir_sample(&mut self) {
        self.base.fill_weights(self.sel_size);
    }

    /// Compacts the reservoir to the sampled rows (`Vacuum` through `Copy`).
    fn vacuum(&mut self) -> Result<()> {
        let Some(reservoir) = &self.reservoir else {
            return Ok(());
        };
        let keep = self.active_sample_count().min(self.sample_count);
        let capacity = self.capacity();
        let mut compacted = Vec::with_capacity(reservoir.len());
        for column in reservoir {
            let mut vector = Vector::flat(column.logical_type().clone(), capacity);
            for (target, &source) in self.sel[..keep].iter().enumerate() {
                vector.set_value(target, &column.get_value(source as usize))?;
            }
            compacted.push(vector);
        }
        self.reservoir = Some(compacted);
        self.reservoir_len = keep;
        for (index, slot) in self.sel[..keep].iter_mut().enumerate() {
            #[allow(clippy::cast_possible_truncation, reason = "sample indexes fit u32")]
            {
                *slot = index as u32;
            }
        }
        Ok(())
    }

    /// Adds a chunk of input rows (`AddToReservoir`).
    pub fn add_to_reservoir(&mut self, chunk: &DataChunk) -> Result<()> {
        if chunk.is_empty() || (self.reservoir.is_none() && self.capacity() == 0) {
            return Ok(());
        }
        let consumed = self.fill_reservoir(chunk)?;
        self.base.num_entries_seen_total += consumed as u64;
        if consumed == chunk.len() {
            return Ok(());
        }
        if consumed != 0 {
            let rest: Vec<u32> = (consumed..chunk.len()).map(|row| row as u32).collect();
            let sel = roastedduck_types::SelectionVector::new(rest);
            let slice = DataChunk::new(
                chunk.columns().iter().map(|column| column.slice(&sel)).collect(),
                chunk.len() - consumed,
            );
            return self.add_to_reservoir(&slice);
        }
        let rows = if self.is_weighted() {
            self.replacement_indexes_slow(self.reservoir_len, chunk.len())
        } else {
            self.replacement_indexes_fast(self.reservoir_len, chunk.len())
        };
        if rows.is_empty() {
            self.base.num_entries_seen_total += chunk.len() as u64;
            return Ok(());
        }
        self.append_rows(chunk, &rows)?;
        self.base.num_entries_seen_total += chunk.len() as u64;
        if !self.is_weighted()
            && self.base.num_entries_seen_total >= (FIXED_SAMPLE_SIZE * FAST_TO_SLOW_THRESHOLD) as u64
        {
            self.convert_to_reservoir_sample();
        }
        if self.reservoir_len + FIXED_SAMPLE_SIZE * 3 >= self.capacity() {
            self.vacuum()?;
        }
        Ok(())
    }

    /// The next chunk of sampled rows, removing them from the sample (`GetChunk`).
    pub fn get_chunk(&mut self) -> Option<DataChunk> {
        let reservoir = self.reservoir.as_ref()?;
        if self.reservoir_len == 0 {
            return None;
        }
        let collected = self.active_sample_count();
        if collected == 0 {
            return None;
        }
        let (remaining, size) = if collected > STANDARD_VECTOR_SIZE {
            (collected - STANDARD_VECTOR_SIZE, STANDARD_VECTOR_SIZE)
        } else {
            (0, collected)
        };
        let mut selection = vec![0u32; size];
        for i in remaining..collected {
            selection[i - remaining] = if self.is_weighted() {
                let top = self.base.weights.pop().map_or(0, |entry| entry.index);
                self.base.update_min_weight_threshold();
                self.sel[top]
            } else {
                self.sel[i]
            };
            self.sel_size -= 1;
        }
        let sel = roastedduck_types::SelectionVector::new(selection);
        Some(DataChunk::new(
            reservoir.iter().map(|column| column.slice(&sel)).collect(),
            size,
        ))
    }
}

/// `USING SAMPLE p% (reservoir)`: one reservoir sample per [`RESERVOIR_THRESHOLD`] rows
/// (DuckDB's `ReservoirSamplePercentage`).
#[derive(Debug)]
pub struct ReservoirSamplePercentage {
    random: RandomEngine,
    sample_percentage: f64,
    reservoir_sample_size: usize,
    current_sample: Option<ReservoirSample>,
    finished_samples: Vec<ReservoirSample>,
    current_count: usize,
    finalized: bool,
}

impl ReservoirSamplePercentage {
    pub fn new(percentage: f64, seed: u64) -> Self {
        let mut random = RandomEngine::new(seed);
        let sample_percentage = percentage / 100.0;
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss,
            reason = "(idx_t)(...)"
        )]
        let reservoir_sample_size = (sample_percentage * RESERVOIR_THRESHOLD as f64) as usize;
        let current_sample =
            ReservoirSample::new(reservoir_sample_size, u64::from(random.next_random_integer()));
        Self {
            random,
            sample_percentage,
            reservoir_sample_size,
            current_sample: Some(current_sample),
            finished_samples: Vec::new(),
            current_count: 0,
            finalized: false,
        }
    }

    pub fn add_to_reservoir(&mut self, input: &DataChunk) -> Result<()> {
        let Some(current) = &mut self.current_sample else {
            return Ok(());
        };
        if self.current_count + input.len() <= RESERVOIR_THRESHOLD {
            self.current_count += input.len();
            return current.add_to_reservoir(input);
        }
        let to_current = RESERVOIR_THRESHOLD - self.current_count;
        let to_next = input.len() - to_current;
        let slice = |from: usize, len: usize| {
            let sel = roastedduck_types::SelectionVector::new(
                (from..from + len).map(|row| row as u32).collect::<Vec<_>>(),
            );
            DataChunk::new(
                input.columns().iter().map(|column| column.slice(&sel)).collect(),
                len,
            )
        };
        if to_current > 0 {
            current.add_to_reservoir(&slice(0, to_current))?;
        }
        if let Some(full) = self.current_sample.take() {
            self.finished_samples.push(full);
        }
        let mut next = ReservoirSample::new(
            self.reservoir_sample_size,
            u64::from(self.random.next_random_integer()),
        );
        if to_next > 0 {
            next.add_to_reservoir(&slice(to_current, to_next))?;
        }
        self.current_sample = Some(next);
        self.current_count = to_next;
        Ok(())
    }

    /// Resamples the last, partial block to its share of rows (`Finalize`).
    fn finalize(&mut self) -> Result<()> {
        if let Some(mut current) = self.current_sample.take() {
            #[allow(clippy::cast_precision_loss, reason = "as DuckDB")]
            let sampled_more_than_required = self.current_count as f64
                > self.sample_percentage * RESERVOIR_THRESHOLD as f64
                || self.finished_samples.is_empty();
            if self.current_count > 0 && sampled_more_than_required {
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    clippy::cast_precision_loss,
                    reason = "round to a count"
                )]
                let size = (self.sample_percentage * self.current_count as f64).round() as usize;
                let mut resampled = ReservoirSample::new(size, u64::from(self.random.next_random_integer()));
                while let Some(chunk) = current.get_chunk() {
                    if chunk.is_empty() {
                        break;
                    }
                    resampled.add_to_reservoir(&chunk)?;
                }
                self.finished_samples.push(resampled);
            } else {
                self.finished_samples.push(current);
            }
        }
        self.finalized = true;
        Ok(())
    }

    pub fn get_chunk(&mut self) -> Result<Option<DataChunk>> {
        if !self.finalized {
            self.finalize()?;
        }
        while let Some(front) = self.finished_samples.first_mut() {
            if let Some(chunk) = front.get_chunk()
                && !chunk.is_empty()
            {
                return Ok(Some(chunk));
            }
            self.finished_samples.remove(0);
        }
        Ok(None)
    }
}
