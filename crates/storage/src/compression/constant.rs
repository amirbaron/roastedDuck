//! Constant segments (DuckDB's `numeric_constant.cpp` and empty validity): the values are
//! fully described by the segment statistics, so no block is stored.

use rustdb_common::{Error, Result};
use rustdb_types::{LogicalTypeId, ValidityMask, Vector};

use crate::table::ColumnSegment;

/// Writes `count` rows of the segment's constant into `result` at `result_offset`.
pub(crate) fn scan(
    segment: &ColumnSegment,
    count: usize,
    result: &mut Vector,
    result_offset: usize,
) -> Result<()> {
    let stats = segment.statistics();
    if segment.logical_type().id() == LogicalTypeId::Validity {
        if stats.can_have_null() && !stats.can_have_no_null() {
            let capacity = result.capacity().max(result_offset + count);
            let mut words = result
                .validity()
                .words()
                .map_or_else(|| vec![u64::MAX; capacity.div_ceil(64)], <[u64]>::to_vec);
            words.resize(capacity.div_ceil(64), u64::MAX);
            for row in result_offset..result_offset + count {
                words[row / 64] &= !(1u64 << (row % 64));
            }
            result.set_validity(ValidityMask::from_words(words));
        }
        return Ok(());
    }
    let value = stats
        .numeric_min()
        .ok_or_else(|| Error::internal("constant segment without a numeric minimum"))?;
    drop(stats);
    for row in result_offset..result_offset + count {
        result.set_value(row, &value)?;
    }
    Ok(())
}
