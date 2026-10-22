//! The table definitions (`tdefs.h`, `tdefs.c`): each table's flags, its range of columns
//! (which are also its random number streams) and its NULL settings.

use super::columns::MAX_COLUMN;
use super::tables::{MAX_TABLE, S_BRAND};
use super::tdefs_data::{S_TDEFS, W_TDEFS};

/// The table is produced in date order.
pub(crate) const FL_DATE_BASED: u32 = 0x0002;
/// The table is the child in a parent/child link.
pub(crate) const FL_CHILD: u32 = 0x0004;
/// The dimension keeps history: its row count counts unique entities, not revisions.
pub(crate) const FL_TYPE_2: u32 = 0x0020;
/// The table has a low row count; used by `mk_address`.
pub(crate) const FL_SMALL: u32 = 0x0040;
/// The table uses sparse keys.
pub(crate) const FL_SPARSE: u32 = 0x0080;
/// The table is in the source schema.
pub(crate) const FL_SOURCE_DDL: u32 = 0x0400;
/// The table has a child in `param`.
pub(crate) const FL_PARENT: u32 = 0x1000;
/// The verify routine uses the warehouse without change.
pub(crate) const FL_PASSTHRU: u32 = 0x4000;
/// The verify routine includes a print function.
pub(crate) const FL_VPRINT: u32 = 0x8000;
/// The table is not defined.
pub(crate) const FL_NOP: u32 = 0x0001;

/// The constant part of a table description (`tdef`).
#[derive(Debug)]
pub(crate) struct Tdef {
    pub(crate) name: &'static str,
    pub(crate) flags: u32,
    /// The first column (and random number stream) of the table.
    pub(crate) first_column: usize,
    /// The last column of the table.
    pub(crate) last_column: usize,
    /// An additional parameter (revision count, child table, ...).
    pub(crate) param: i32,
    /// The percentage of rows with NULLs, in basis points.
    pub(crate) null_pct: i32,
    /// `kNullBitMap` before the first row sets it.
    pub(crate) null_bit_map: i64,
    /// The columns that are never NULL.
    pub(crate) not_null_bit_map: i64,
}

impl Tdef {
    pub(crate) const fn is(&self, flag: u32) -> bool {
        self.flags & flag != 0
    }
}

/// The number of tables with a definition: the warehouse's and the source schema's.
pub(crate) const TDEF_COUNT: usize = S_BRAND + S_TDEFS.len();
// The pseudo tables after them have no definition.
const _: () = assert!(TDEF_COUNT <= MAX_TABLE + 1);

/// `getSimpleTdefsByNumber`.
pub(crate) const fn get_simple_tdefs_by_number(table: usize) -> &'static Tdef {
    if table >= S_BRAND {
        &S_TDEFS[table - S_BRAND]
    } else {
        &W_TDEFS[table]
    }
}

/// The table of each column, `u8::MAX` for none (the scan of `getTableFromColumn`, done
/// once).
const COLUMN_TABLES: [u8; MAX_COLUMN + 1] = column_tables();

const fn column_tables() -> [u8; MAX_COLUMN + 1] {
    let mut tables = [u8::MAX; MAX_COLUMN + 1];
    let mut column = 0;
    while column <= MAX_COLUMN {
        // The tables after the source schema's are pseudo tables without columns.
        let mut table = 0;
        while table < TDEF_COUNT {
            let tdef = get_simple_tdefs_by_number(table);
            if tdef.first_column <= column && column <= tdef.last_column {
                tables[column] = table as u8;
                break;
            }
            table += 1;
        }
        column += 1;
    }
    tables
}

/// `getTableFromColumn`: the first table whose columns include `column`.
pub(crate) const fn get_table_from_column(column: usize) -> usize {
    let table = COLUMN_TABLES[column];
    assert!(table != u8::MAX, "a column outside of every table");
    table as usize
}
