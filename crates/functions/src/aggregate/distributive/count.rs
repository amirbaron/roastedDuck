//! `count(x)`, `count()` and `count_star()` (DuckDB's `count.cpp`).

use rustdb_common::Result;
use rustdb_function::aggregate::{Aggregate, AggregateInput, States, Typed};
use rustdb_function::{AggregateFunction, AggregateFunctionSet, NullHandling};
use rustdb_types::{LogicalType, Sel, Vector, VectorKind};

/// Counts the non-NULL rows of its argument.
#[derive(Debug)]
struct Count;

impl Aggregate for Count {
    type State = i64;

    fn initialize(&self) -> i64 {
        0
    }

    fn update(
        &self,
        inputs: &[Vector],
        _: AggregateInput<'_>,
        states: &mut States<'_, i64>,
        count: usize,
    ) -> Result<()> {
        let input = &inputs[0];
        if let Some(state) = states.as_single() {
            *state += count_valid(input, count) as i64;
            return Ok(());
        }
        if input.kind() == VectorKind::Constant {
            if !input.is_null(0) {
                for row in 0..count {
                    *states.get(row) += 1;
                }
            }
            return Ok(());
        }
        let validity = input.validity();
        let sel = input.sel();
        for row in 0..count {
            if validity.is_valid(sel.slot(row)) {
                *states.get(row) += 1;
            }
        }
        Ok(())
    }

    const SELECTION_UPDATE: bool = true;

    fn update_selection(
        &self,
        inputs: &[Vector],
        _: AggregateInput<'_>,
        state: &mut i64,
        sel: &[u32],
    ) -> Result<()> {
        let input = &inputs[0];
        let validity = input.validity();
        if input.kind() == VectorKind::Constant {
            if !input.is_null(0) {
                *state += sel.len() as i64;
            }
        } else if validity.is_all_valid() {
            *state += sel.len() as i64;
        } else {
            let data_sel = input.sel();
            let valid = sel
                .iter()
                .filter(|&&row| validity.is_valid(data_sel.slot(row as usize)))
                .count();
            *state += valid as i64;
        }
        Ok(())
    }

    fn combine(&self, source: &i64, target: &mut i64, _: AggregateInput<'_>) -> Result<()> {
        *target += *source;
        Ok(())
    }

    fn finalize(
        &self,
        state: &mut i64,
        _: AggregateInput<'_>,
        result: &mut Vector,
        row: usize,
    ) -> Result<()> {
        result.values_mut::<i64>()[row] = *state;
        Ok(())
    }

    fn finalize_all(
        &self,
        states: &mut States<'_, i64>,
        count: usize,
        _: AggregateInput<'_>,
        result: &mut Vector,
        offset: usize,
    ) -> Result<()> {
        finalize_counts(states, count, result, offset);
        Ok(())
    }
}

/// Writes `count` counts into rows `offset..` of `result`, borrowing its data once
/// (`StateFinalize` with `CountFunction::Finalize`).
fn finalize_counts(states: &mut States<'_, i64>, count: usize, result: &mut Vector, offset: usize) {
    let data = &mut result.values_mut::<i64>()[offset..offset + count];
    for (index, value) in data.iter_mut().enumerate() {
        *value = *states.get(index);
    }
}

/// The number of non-NULL rows among the first `count` rows of `input`
/// (DuckDB's `CountUpdate`: whole validity words at a time for flat vectors).
fn count_valid(input: &Vector, count: usize) -> usize {
    if input.kind() == VectorKind::Constant {
        return if input.is_null(0) { 0 } else { count };
    }
    let validity = input.validity();
    if validity.is_all_valid() {
        return count;
    }
    match input.sel() {
        Sel::Identity => match validity.words() {
            Some(words) => {
                let full = count / 64;
                let mut total: usize = words
                    .iter()
                    .take(full)
                    .map(|word| word.count_ones() as usize)
                    .sum();
                // Rows past the materialised words are valid.
                total += 64 * full.saturating_sub(words.len());
                let rest = count % 64;
                if rest > 0 {
                    total += words
                        .get(full)
                        .map_or(rest, |word| (word & ((1u64 << rest) - 1)).count_ones() as usize);
                }
                total
            }
            None => count,
        },
        sel => (0..count).filter(|&row| validity.is_valid(sel.slot(row))).count(),
    }
}

/// Counts rows (`count_star`, `count()`).
#[derive(Debug)]
struct CountStar;

impl Aggregate for CountStar {
    type State = i64;

    fn initialize(&self) -> i64 {
        0
    }

    fn update(
        &self,
        _: &[Vector],
        _: AggregateInput<'_>,
        states: &mut States<'_, i64>,
        count: usize,
    ) -> Result<()> {
        if let Some(state) = states.as_single() {
            *state += count as i64;
            return Ok(());
        }
        for row in 0..count {
            *states.get(row) += 1;
        }
        Ok(())
    }

    const SELECTION_UPDATE: bool = true;

    fn update_selection(
        &self,
        _: &[Vector],
        _: AggregateInput<'_>,
        state: &mut i64,
        sel: &[u32],
    ) -> Result<()> {
        *state += sel.len() as i64;
        Ok(())
    }

    fn combine(&self, source: &i64, target: &mut i64, _: AggregateInput<'_>) -> Result<()> {
        *target += *source;
        Ok(())
    }

    fn finalize(
        &self,
        state: &mut i64,
        _: AggregateInput<'_>,
        result: &mut Vector,
        row: usize,
    ) -> Result<()> {
        result.values_mut::<i64>()[row] = *state;
        Ok(())
    }

    fn finalize_all(
        &self,
        states: &mut States<'_, i64>,
        count: usize,
        _: AggregateInput<'_>,
        result: &mut Vector,
        offset: usize,
    ) -> Result<()> {
        finalize_counts(states, count, result, offset);
        Ok(())
    }
}

/// `count_star() -> BIGINT` (DuckDB's `CountStarFun::GetFunction`).
pub fn count_star_function() -> AggregateFunction {
    AggregateFunction::new("count_star", Vec::new(), LogicalType::BIGINT, Typed(CountStar))
        .with_null_handling(NullHandling::Special)
        .order_independent()
}

/// `count(ANY) -> BIGINT` (DuckDB's `CountFunctionBase::GetFunction`).
pub fn count_function() -> AggregateFunction {
    AggregateFunction::new("count", vec![LogicalType::ANY], LogicalType::BIGINT, Typed(Count))
        .with_null_handling(NullHandling::Special)
        .order_independent()
}

/// The `count` set: `count(ANY)` and `count()`, which takes the set's name like every
/// function of a set (`CreateAggregateFunctionInfo`).
pub fn count_functions() -> AggregateFunctionSet {
    let mut set = AggregateFunctionSet::new("count");
    set.add(count_function());
    let mut count_star = count_star_function();
    count_star.name = "count".into();
    set.add(count_star);
    set
}

/// The `count_star` set.
pub fn count_star_functions() -> AggregateFunctionSet {
    AggregateFunctionSet::single("count_star", count_star_function())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustdb_types::{SelectionVector, Value};

    #[test]
    fn counts_valid_rows_across_words() {
        let mut vector = Vector::flat(LogicalType::INTEGER, 200);
        for row in (0..200).step_by(3) {
            vector.set_value(row, &Value::null(LogicalType::INTEGER)).unwrap();
        }
        let expected = (0..150).filter(|row| row % 3 != 0).count();
        assert_eq!(count_valid(&vector, 150), expected);
        let sliced = vector.slice(&SelectionVector::new(vec![0, 1, 2, 3]));
        assert_eq!(count_valid(&sliced, 4), 2);
        let null = Vector::constant_null(LogicalType::INTEGER);
        assert_eq!(count_valid(&null, 10), 0);
    }
}
