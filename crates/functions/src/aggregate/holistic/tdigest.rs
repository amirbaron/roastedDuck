//! The t-digest DuckDB uses for `approx_quantile` (`third_party/tdigest/t_digest.hpp`, by
//! Derrick R. Burns, Apache 2.0).
//!
//! The arithmetic follows the C++ code operation by operation, including the fused
//! multiply-adds clang emits for `a * b + c` patterns on the reference build.

use std::f64::consts::PI;

/// A cluster of values (`Centroid`).
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Centroid {
    mean: f64,
    weight: f64,
}

impl Centroid {
    fn new(mean: f64, weight: f64) -> Self {
        Self { mean, weight }
    }

    /// `Centroid::add`.
    fn add(&mut self, other: &Self) {
        if self.weight == 0.0 {
            self.weight = other.weight;
            self.mean = other.mean;
        } else {
            self.weight += other.weight;
            self.mean += other.weight * (other.mean - self.mean) / self.weight;
        }
    }
}

/// `TDigest`.
#[derive(Clone, Debug)]
pub(crate) struct TDigest {
    compression: f64,
    min: f64,
    max: f64,
    max_processed: usize,
    max_unprocessed: usize,
    processed_weight: f64,
    unprocessed_weight: f64,
    processed: Vec<Centroid>,
    unprocessed: Vec<Centroid>,
    cumulative: Vec<f64>,
}

impl TDigest {
    pub(crate) fn new(compression: f64) -> Self {
        let max_processed = (2.0 * compression.ceil()) as usize;
        let max_unprocessed = (8.0 * compression.ceil()) as usize;
        Self {
            compression,
            min: f64::MAX,
            // `std::numeric_limits<double>::min()`: the smallest positive normal value.
            max: f64::MIN_POSITIVE,
            max_processed,
            max_unprocessed,
            processed_weight: 0.0,
            unprocessed_weight: 0.0,
            processed: Vec::with_capacity(max_processed),
            unprocessed: Vec::with_capacity(max_unprocessed + 1),
            cumulative: Vec::new(),
        }
    }

    fn is_dirty(&self) -> bool {
        self.processed.len() > self.max_processed || self.unprocessed.len() > self.max_unprocessed
    }

    /// `add(x)`: NaN is ignored.
    pub(crate) fn add(&mut self, value: f64) {
        if value.is_nan() {
            return;
        }
        self.unprocessed.push(Centroid::new(value, 1.0));
        self.unprocessed_weight += 1.0;
        if self.is_dirty() {
            self.process();
        }
    }

    /// `merge(other)` (`add` of one digest).
    pub(crate) fn merge(&mut self, other: &Self) {
        // A single digest is always one batch of DuckDB's `kHighWater` batching.
        self.merge_processed(other);
        self.merge_unprocessed(other);
        if self.is_dirty() {
            self.process();
        }
        self.update_cumulative();
    }

    fn merge_unprocessed(&mut self, other: &Self) {
        self.unprocessed.extend_from_slice(&other.unprocessed);
        self.unprocessed_weight += other.unprocessed_weight;
    }

    /// `mergeProcessed`: a k-way merge of sorted centroid lists.
    fn merge_processed(&mut self, other: &Self) {
        if other.processed.is_empty() {
            return;
        }
        self.processed_weight += other.processed_weight;
        let mut sorted = Vec::with_capacity(other.processed.len() + self.processed.len());
        let (mut i, mut j) = (0, 0);
        let (left, right) = (&other.processed, &self.processed);
        while i < left.len() || j < right.len() {
            let take_left = j >= right.len() || (i < left.len() && left[i].mean <= right[j].mean);
            if take_left {
                sorted.push(left[i]);
                i += 1;
            } else {
                sorted.push(right[j]);
                j += 1;
            }
        }
        self.processed = sorted;
        if let (Some(first), Some(last)) = (self.processed.first(), self.processed.last()) {
            self.min = self.min.min(first.mean);
            self.max = self.max.max(last.mean);
        }
    }

    fn update_cumulative(&mut self) {
        self.cumulative.clear();
        let mut previous = 0.0;
        for centroid in &self.processed {
            let current = centroid.weight;
            self.cumulative.push(previous + current / 2.0);
            previous += current;
        }
        self.cumulative.push(previous);
    }

    fn integrated_location(&self, q: f64) -> f64 {
        self.compression * (2.0f64.mul_add(q, -1.0).asin() + PI / 2.0) / PI
    }

    #[allow(clippy::manual_midpoint, reason = "the reference's arithmetic")]
    fn integrated_q(&self, k: f64) -> f64 {
        ((k.min(self.compression) * PI / self.compression - PI / 2.0).sin() + 1.0) / 2.0
    }

    /// `process`: merges the unprocessed values into the processed centroids.
    pub(crate) fn process(&mut self) {
        self.unprocessed.sort_by(|a, b| a.mean.total_cmp(&b.mean));
        // `std::inplace_merge` is stable: unprocessed centroids come first among equals.
        let mut merged = Vec::with_capacity(self.unprocessed.len() + self.processed.len());
        let (mut i, mut j) = (0, 0);
        while i < self.unprocessed.len() || j < self.processed.len() {
            let take_unprocessed = j >= self.processed.len()
                || (i < self.unprocessed.len() && self.processed[j].mean >= self.unprocessed[i].mean);
            if take_unprocessed {
                merged.push(self.unprocessed[i]);
                i += 1;
            } else {
                merged.push(self.processed[j]);
                j += 1;
            }
        }
        self.processed_weight += self.unprocessed_weight;
        self.unprocessed_weight = 0.0;
        self.processed.clear();
        let Some(&first) = merged.first() else {
            self.unprocessed.clear();
            return;
        };
        self.processed.push(first);
        let mut so_far = first.weight;
        let mut limit = self.processed_weight * self.integrated_q(1.0);
        for centroid in &merged[1..] {
            let projected = so_far + centroid.weight;
            if projected <= limit {
                so_far = projected;
                if let Some(last) = self.processed.last_mut() {
                    last.add(centroid);
                }
            } else {
                let k1 = self.integrated_location(so_far / self.processed_weight);
                limit = self.processed_weight * self.integrated_q(k1 + 1.0);
                so_far += centroid.weight;
                self.processed.push(*centroid);
            }
        }
        self.unprocessed.clear();
        if let (Some(first), Some(last)) = (self.processed.first(), self.processed.last()) {
            self.min = self.min.min(first.mean);
            self.max = self.max.max(last.mean);
        }
        self.update_cumulative();
    }

    /// `weightedAverage`.
    fn weighted_average(x1: f64, w1: f64, x2: f64, w2: f64) -> f64 {
        if x1 <= x2 {
            Self::weighted_average_sorted(x1, w1, x2, w2)
        } else {
            Self::weighted_average_sorted(x2, w2, x1, w1)
        }
    }

    fn weighted_average_sorted(x1: f64, w1: f64, x2: f64, w2: f64) -> f64 {
        let x = x1.mul_add(w1, x2 * w2) / (w1 + w2);
        x1.max(x.min(x2))
    }

    /// `quantile(q)`: processes pending values first.
    pub(crate) fn quantile(&mut self, q: f64) -> f64 {
        if !self.unprocessed.is_empty() || self.is_dirty() {
            self.process();
        }
        self.quantile_processed(q)
    }

    fn quantile_processed(&self, q: f64) -> f64 {
        if !(0.0..=1.0).contains(&q) {
            return f64::NAN;
        }
        let n = self.processed.len();
        match n {
            0 => return f64::NAN,
            1 => return self.processed[0].mean,
            _ => {}
        }
        let index = q * self.processed_weight;
        let (mean0, weight0) = (self.processed[0].mean, self.processed[0].weight);
        if index <= weight0 / 2.0 {
            return (2.0 * index / weight0).mul_add(mean0 - self.min, self.min);
        }
        let position = self.cumulative.partition_point(|&value| value < index);
        if position + 1 < self.cumulative.len() {
            let z1 = index - self.cumulative[position - 1];
            let z2 = self.cumulative[position] - index;
            return Self::weighted_average(
                self.processed[position - 1].mean,
                z2,
                self.processed[position].mean,
                z1,
            );
        }
        let last = self.processed[n - 1];
        let z1 = index - self.processed_weight - last.weight / 2.0;
        let z2 = last.weight / 2.0 - z1;
        Self::weighted_average(last.mean, z1, self.max, z2)
    }
}
