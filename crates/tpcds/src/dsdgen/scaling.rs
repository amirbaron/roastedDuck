//! Row counts per scale factor (`scaling.c`).

use super::constants::{CALENDAR_SALES, CALENDAR_UNIFORM, DATE_MAXIMUM, DATE_MINIMUM};
use super::context::Context;
use super::date::{Date, is_leap};
use super::dist::dist;
use super::tables::{
    CALL_CENTER, CATALOG_SALES, INVENTORY, ITEM, MAX_TABLE, PSEUDO_TABLE_START, S_CATALOG_ORDER, S_INVENTORY,
    S_PURCHASE, S_WEB_ORDER, STORE_SALES, WAREHOUSE, WEB_SALES,
};
use super::tdefs::{FL_TYPE_2, get_simple_tdefs_by_number};

/// `arScaleVolume`: the scale factors the `rowcounts` distribution defines.
const SCALE_VOLUMES: [i32; 9] = [1, 10, 100, 300, 1000, 3000, 10000, 30000, 100_000];

/// `getScaleSlot`.
fn scale_slot(target_gb: i32) -> usize {
    SCALE_VOLUMES
        .iter()
        .take_while(|&&volume| target_gb > volume)
        .count()
}

/// `LogScale`: interpolates between the defined scale factors.
fn log_scale(table: usize, target_gb: i32) -> i64 {
    let rowcounts = dist("rowcounts");
    let row = table as i32 + 1;
    let i = scale_slot(target_gb);
    let delta = rowcounts.weight(row, i + 1) - rowcounts.weight(row, i);
    let offset = (target_gb - SCALE_VOLUMES[i - 1]) as f32 / (SCALE_VOLUMES[i] - SCALE_VOLUMES[i - 1]) as f32;
    i64::from((offset * delta as f32) as i32) + i64::from(rowcounts.weight(row, 1))
}

/// `StaticScale`.
fn static_scale(table: usize) -> i64 {
    i64::from(dist("rowcounts").weight(table as i32 + 1, 1))
}

/// `LinearScale`: builds the target from the defined scale factors, largest first.
fn linear_scale(table: usize, mut target_gb: i32) -> i64 {
    let rowcounts = dist("rowcounts");
    let mut rowcount = 0;
    for i in (0..SCALE_VOLUMES.len()).rev() {
        while target_gb >= SCALE_VOLUMES[i] {
            rowcount += i64::from(rowcounts.weight(table as i32 + 1, i + 1));
            target_gb -= SCALE_VOLUMES[i];
        }
    }
    rowcount
}

/// The base row count of every table at scale factor `scale` (`get_rowcount`'s
/// initialization of `arRowcount`).
pub(crate) fn base_rowcounts(scale: f64) -> Vec<i64> {
    let rowcounts = dist("rowcounts");
    let int_scale = if scale < 1.0 { 1 } else { scale as i32 };
    (CALL_CENTER..=MAX_TABLE)
        .map(|table| {
            let row = table as i32 + 1;
            let mut rowcount = match int_scale {
                100_000 => i64::from(rowcounts.weight(row, 9)),
                30000 => i64::from(rowcounts.weight(row, 8)),
                10000 => i64::from(rowcounts.weight(row, 7)),
                3000 => i64::from(rowcounts.weight(row, 6)),
                1000 => i64::from(rowcounts.weight(row, 5)),
                300 => i64::from(rowcounts.weight(row, 4)),
                100 => i64::from(rowcounts.weight(row, 3)),
                10 => i64::from(rowcounts.weight(row, 2)),
                1 => i64::from(rowcounts.weight(row, 1)),
                _ => match rowcounts.member_int(row, 3) {
                    2 => linear_scale(table, scale as i32),
                    1 => static_scale(table),
                    3 => log_scale(table, scale as i32),
                    _ => 0,
                },
            };
            // Now adjust for the multiplier.
            let mut multiplier = 1;
            if table < PSEUDO_TABLE_START && get_simple_tdefs_by_number(table).is(FL_TYPE_2) {
                multiplier = 2;
            }
            for _ in 1..=rowcounts.member_int(row, 2) {
                multiplier *= 10;
            }
            rowcount *= multiplier;
            if rowcount >= 0 && scale < 1.0 {
                if !(rowcounts.member_int(row, 3) == 1 && multiplier == 1) {
                    rowcount = i64::from((rowcount as f64 * scale) as i32);
                }
                if rowcount == 0 {
                    rowcount = 1;
                }
            }
            rowcount
        })
        .collect()
}

impl Context {
    /// `get_rowcount`.
    pub(crate) fn get_rowcount(&self, table: usize) -> i64 {
        if table == INVENTORY {
            return self.sc_w_inventory();
        }
        if table == S_INVENTORY {
            return self.get_id_count(ITEM) * self.get_rowcount(WAREHOUSE) * 6;
        }
        self.rowcounts[table]
    }

    /// `getIDCount`: the number of unique entities of a table, without the revisions of a
    /// history-keeping dimension.
    pub(crate) fn get_id_count(&self, table: usize) -> i64 {
        let rowcount = self.get_rowcount(table);
        if table >= PSEUDO_TABLE_START || !get_simple_tdefs_by_number(table).is(FL_TYPE_2) {
            return rowcount;
        }
        let mut unique = (rowcount / 6) * 3;
        unique += match rowcount % 6 {
            1 => 1,
            2 | 3 => 2,
            4 | 5 => 3,
            _ => 0,
        };
        unique
    }

    /// `sc_w_inventory`: every item's inventory in every warehouse, weekly.
    pub(crate) fn sc_w_inventory(&self) -> i64 {
        let mut rowcount = self.get_id_count(ITEM);
        rowcount *= self.get_rowcount(WAREHOUSE);
        let mut days = Date::parse(DATE_MAXIMUM).julian;
        days -= Date::parse(DATE_MINIMUM).julian;
        days += 1;
        days += 6;
        days /= 7;
        rowcount * i64::from(days)
    }

    /// `dateScaling`: the number of rows of a fact table on day `julian`.
    pub(crate) fn date_scaling(&self, table: usize, julian: i64) -> i64 {
        let date = Date::from_julian(julian as i32);
        let (mut rowcount, mut date_weight) = match table {
            STORE_SALES | CATALOG_SALES | WEB_SALES => (self.get_rowcount(table), CALENDAR_SALES),
            S_CATALOG_ORDER => (self.get_rowcount(CATALOG_SALES), CALENDAR_SALES),
            S_PURCHASE => (self.get_rowcount(STORE_SALES), CALENDAR_SALES),
            S_WEB_ORDER => (self.get_rowcount(WEB_SALES), CALENDAR_SALES),
            S_INVENTORY | INVENTORY => (
                self.get_rowcount(WAREHOUSE) * self.get_id_count(ITEM),
                CALENDAR_UNIFORM,
            ),
            _ => unreachable!(
                "Table {} is not yet fully defined",
                get_simple_tdefs_by_number(table).name
            ),
        };
        // Inventory is uniform throughout the year.
        if table != INVENTORY {
            if is_leap(date.year) {
                date_weight += 1;
            }
            // Assumes a date range of 5 years.
            let calendar_total = i64::from(self.dists.calendar.max(date_weight)) * 5;
            let day_weight = self.dists.calendar.weight(date.day_number(), date_weight);
            rowcount *= i64::from(day_weight);
            rowcount += calendar_total / 2;
            rowcount /= calendar_total;
        }
        rowcount
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sf1_rowcounts() {
        let context = Context::new(1.0).unwrap();
        assert_eq!(context.get_rowcount(CALL_CENTER), 6);
        assert_eq!(context.get_rowcount(INVENTORY), 11_745_000);
        assert_eq!(context.get_rowcount(ITEM), 18000);
        assert_eq!(context.get_rowcount(WAREHOUSE), 5);
    }
}
