//! Slowly changing dimensions (`scd.c`): the revisions of the history-keeping tables.

use super::build_support::mk_bkey;
use super::context::Context;
use super::tdefs::get_table_from_column;

/// The business key and the validity of a revision (`setSCDKeys`'s outputs).
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ScdKeys {
    pub(crate) business_key: [u8; 16],
    pub(crate) begin_date: i64,
    pub(crate) end_date: i64,
}

impl Context {
    /// `setSCDKeys`: the business key and dates of row `index` of the table of `column`;
    /// `true` if the row starts a new business key. Each key has one, two or three
    /// revisions, by `index % 6`.
    pub(crate) fn set_scd_keys(&self, column: usize, index: i64, keys: &mut ScdKeys) -> bool {
        let dates = &self.scd_dates;
        let table = get_table_from_column(column) as i64;
        let shift = table * 6;
        let mut new_key = false;
        match index % 6 {
            // 1 revision.
            1 => {
                keys.business_key = mk_bkey(index);
                new_key = true;
                keys.begin_date = dates.minimum - shift;
                keys.end_date = -1;
            }
            // 1 of 2 revisions.
            2 => {
                keys.business_key = mk_bkey(index);
                new_key = true;
                keys.begin_date = dates.minimum - shift;
                keys.end_date = dates.h1 - shift;
            }
            // 2 of 2 revisions.
            3 => {
                keys.business_key = mk_bkey(index - 1);
                keys.begin_date = dates.h1 - shift + 1;
                keys.end_date = -1;
            }
            // 1 of 3 revisions.
            4 => {
                keys.business_key = mk_bkey(index);
                new_key = true;
                keys.begin_date = dates.minimum - shift;
                keys.end_date = dates.t1 - shift;
            }
            // 2 of 3 revisions.
            5 => {
                keys.business_key = mk_bkey(index - 1);
                keys.begin_date = dates.t1 - shift + 1;
                keys.end_date = dates.t2 - shift;
            }
            // 3 of 3 revisions.
            _ => {
                keys.business_key = mk_bkey(index - 2);
                keys.begin_date = dates.t2 - shift + 1;
                keys.end_date = -1;
            }
        }
        // No revision can be in the future, per bug 114.
        if keys.end_date > dates.maximum {
            keys.end_date = -1;
        }
        new_key
    }
}

/// `changeSCD`: whether a revision keeps the previous revision's value of a field, by the
/// next bit of `flags`. A kept value is copied from `old`; a new one is saved in `old`.
pub(crate) fn change_scd<T: Clone>(new: &mut T, old: &mut T, flags: &mut i32, first: bool) {
    if *flags != (*flags / 2) * 2 && !first {
        new.clone_from(old);
    } else {
        old.clone_from(new);
    }
    *flags /= 2;
}

/// `changeSCD(SCD_PTR, ...)`, which assigns the pointers it is given (copies of them):
/// the field keeps its new value, only the bit of `flags` is used up. The same holds for
/// `SCD_INT` of a decimal, which copies the `flags` of the `decimal_t` only.
pub(crate) fn skip_scd(flags: &mut i32) {
    *flags /= 2;
}
