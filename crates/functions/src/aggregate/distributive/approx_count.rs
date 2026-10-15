//! `approx_count_distinct` and DuckDB's `HyperLogLog` sketch (`approx_count.cpp`,
//! `common/types/hyperloglog.cpp`), bit-exact: 64 registers indexed by the low 6 bits of
//! DuckDB's hash, estimated with Ertl's improved estimator.

use rustdb_common::Result;
use rustdb_function::aggregate::{Aggregate, AggregateInput, States, Typed};
use rustdb_function::{AggregateFunction, AggregateFunctionSet, NullHandling};
use rustdb_row::hash::hash_vector;
use rustdb_types::{LogicalType, Vector};

/// DuckDB's `HyperLogLog` (format `HLL_V2`): `M = 2^P` one-byte registers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HyperLogLog {
    registers: [u8; Self::M],
}

impl Default for HyperLogLog {
    fn default() -> Self {
        Self {
            registers: [0; Self::M],
        }
    }
}

impl HyperLogLog {
    const P: u32 = 6;
    const Q: u32 = 64 - Self::P;
    const M: usize = 1 << Self::P;
    const ALPHA: f64 = 0.721_347_520_444_481_7;

    /// Adds an element by its hash (`InsertElement`).
    #[inline]
    pub fn insert_hash(&mut self, hash: u64) {
        let index = (hash & ((1 << Self::P) - 1)) as usize;
        let rest = (hash >> Self::P) | (1u64 << Self::Q);
        let zeros = (rest.trailing_zeros() + 1) as u8;
        self.registers[index] = self.registers[index].max(zeros);
    }

    /// Merges another sketch into this one (`Merge`).
    pub fn merge(&mut self, other: &Self) {
        for (register, &other) in self.registers.iter_mut().zip(&other.registers) {
            *register = (*register).max(other);
        }
    }

    /// The raw registers (the `HLL_V2` serialization payload).
    pub fn registers(&self) -> &[u8] {
        &self.registers
    }

    /// The estimated number of distinct elements (`Count`).
    pub fn count(&self) -> u64 {
        let mut counts = [0u32; Self::Q as usize + 2];
        for &register in &self.registers {
            counts[usize::from(register)] += 1;
        }
        Self::estimate_cardinality(&counts) as u64
    }

    fn estimate_cardinality(counts: &[u32]) -> i64 {
        let m = Self::M as f64;
        let q = Self::Q as usize;
        let mut z = m * tau((m - f64::from(counts[q])) / m);
        for k in (1..=q).rev() {
            z += f64::from(counts[k]);
            z *= 0.5;
        }
        z = m.mul_add(sigma(f64::from(counts[0]) / m), z);
        (Self::ALPHA * m * m / z).round() as i64
    }
}

/// DuckDB's `HLLSigma`.
#[allow(clippy::float_cmp, reason = "iterates to an exact fixed point like DuckDB")]
fn sigma(mut x: f64) -> f64 {
    if x == 1.0 {
        return f64::INFINITY;
    }
    let mut y = 1.0;
    let mut z = x;
    loop {
        x *= x;
        let previous = z;
        z = x.mul_add(y, z);
        y += y;
        if previous == z {
            return z;
        }
    }
}

/// DuckDB's `HLLTau`.
#[allow(clippy::float_cmp, reason = "iterates to an exact fixed point like DuckDB")]
fn tau(mut x: f64) -> f64 {
    if x == 0.0 || x == 1.0 {
        return 0.0;
    }
    let mut y = 1.0;
    let mut z = 1.0 - x;
    loop {
        x = x.sqrt();
        let previous = z;
        y *= 0.5;
        z = (-(1.0 - x).powf(2.0)).mul_add(y, z);
        if previous == z {
            return z / 3.0;
        }
    }
}

/// `approx_count_distinct(ANY) -> BIGINT`.
struct ApproxCountDistinct;

impl Aggregate for ApproxCountDistinct {
    type State = HyperLogLog;

    fn initialize(&self) -> HyperLogLog {
        HyperLogLog::default()
    }

    fn update(
        &self,
        inputs: &[Vector],
        _: AggregateInput<'_>,
        states: &mut States<'_, HyperLogLog>,
        count: usize,
    ) -> Result<()> {
        let input = &inputs[0];
        let mut hashes = Vector::flat(LogicalType::UBIGINT, 0);
        hash_vector(input, count, &mut hashes)?;
        let hashes = hashes.unified::<u64>();
        if let Some(state) = states.as_single()
            && input.is_constant()
        {
            if !input.is_null(0) {
                state.insert_hash(hashes.values[0]);
            }
            return Ok(());
        }
        for row in 0..count {
            if !input.is_null(row) {
                states.get(row).insert_hash(hashes.values[hashes.slot(row)]);
            }
        }
        Ok(())
    }

    fn combine(&self, source: &HyperLogLog, target: &mut HyperLogLog, _: AggregateInput<'_>) -> Result<()> {
        target.merge(source);
        Ok(())
    }

    fn finalize(
        &self,
        state: &mut HyperLogLog,
        _: AggregateInput<'_>,
        result: &mut Vector,
        row: usize,
    ) -> Result<()> {
        result.values_mut::<i64>()[row] = state.count() as i64;
        Ok(())
    }
}

/// `approx_count_distinct(ANY) -> BIGINT`.
pub fn approx_count_distinct_functions() -> AggregateFunctionSet {
    AggregateFunctionSet::single(
        "approx_count_distinct",
        AggregateFunction::new(
            "approx_count_distinct",
            vec![LogicalType::ANY],
            LogicalType::BIGINT,
            Typed(ApproxCountDistinct),
        )
        .with_null_handling(NullHandling::Special),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_sketch_counts_zero() {
        assert_eq!(HyperLogLog::default().count(), 0);
        let mut sketch = HyperLogLog::default();
        sketch.insert_hash(rustdb_row::hash::murmur_hash64(42));
        assert_eq!(sketch.count(), 1);
    }
}
