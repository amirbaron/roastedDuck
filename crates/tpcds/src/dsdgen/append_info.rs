//! The appends of the generated rows (DuckDB's `append_info-c.cpp`): the `append_*`
//! helpers the table generators call, on the row interface of DuckDB's
//! `InternalAppender`, which converts each value to the type of its column.

use std::fmt::Write as _;

use rustdb_common::{Error, Result, STANDARD_VECTOR_SIZE};
use rustdb_types::{DataChunk, LogicalType, StringHeap, StringT, ValidityMask, Vector};

use super::date::Date;
use super::decimal::Decimal;
use super::tdefs::{TDEF_COUNT, get_simple_tdefs_by_number, get_table_from_column};
use super::{ColumnType, TableInfo};

/// The values of one column of the chunk being filled, in the column's physical type.
#[derive(Debug)]
enum Values {
    /// BIGINT and DECIMAL(15,2).
    Int64(Vec<i64>),
    /// INTEGER, DATE, DECIMAL(5,2) and DECIMAL(7,2).
    Int32(Vec<i32>),
    Varchar(Vec<StringT>, StringHeap),
}

/// One column of the chunk being filled.
#[derive(Debug)]
struct ColumnBuffer {
    ty: ColumnType,
    values: Values,
    /// The validity of the rows so far, if one is NULL.
    validity: Option<ValidityMask>,
}

/// `CastExceptionText`: the error of an appended value out of the range of its column.
fn out_of_range(value: i64, from: &str, to: &str) -> Error {
    Error::invalid_input(format!(
        "Type {from} with value {value} can't be cast because the value is out of range for the destination type {to}"
    ))
}

impl ColumnBuffer {
    fn new(ty: ColumnType) -> Self {
        let values = match ty {
            ColumnType::BigInt => Values::Int64(Vec::new()),
            ColumnType::Decimal(width, _) if width > 9 => {
                Values::Int64(Vec::new())
            }
            ColumnType::Integer | ColumnType::Date | ColumnType::Decimal(..) => {
                Values::Int32(Vec::new())
            }
            ColumnType::Varchar => {
                Values::Varchar(Vec::new(), StringHeap::default())
            }
        };
        Self {
            ty,
            values,
            validity: None,
        }
    }

    fn len(&self) -> usize {
        match &self.values {
            Values::Int64(values) => values.len(),
            Values::Int32(values) => values.len(),
            Values::Varchar(values, _) => values.len(),
        }
    }

    /// `Append(nullptr)`.
    fn push_null(&mut self) {
        let row = self.len();
        self.validity
            .get_or_insert_with(ValidityMask::all_valid)
            .set_invalid(row, STANDARD_VECTOR_SIZE);
        match &mut self.values {
            Values::Int64(values) => values.push(0),
            Values::Int32(values) => values.push(0),
            Values::Varchar(values, _) => values.push(StringT::default()),
        }
    }

    /// `Append<int64_t>`: BIGINT and the physical value of a DECIMAL as they are, INTEGER
    /// and a DECIMAL(..,9) if they fit, VARCHAR as the number's text.
    fn push_i64(&mut self, value: i64) -> Result<()> {
        match &mut self.values {
            Values::Int64(values) => values.push(value),
            Values::Int32(values) => {
                if self.ty == ColumnType::Date {
                    return Err(Error::internal("an integer appended to a DATE column"));
                }
                values.push(i32::try_from(value).map_err(|_| out_of_range(value, "INT64", "INT32"))?);
            }
            Values::Varchar(values, heap) => {
                let mut text = String::new();
                let _ = write!(text, "{value}");
                values.push(heap.add(text.as_bytes()));
            }
        }
        Ok(())
    }

    /// `Append<int32_t>`.
    fn push_i32(&mut self, value: i32) -> Result<()> {
        match &mut self.values {
            Values::Int64(values) => values.push(i64::from(value)),
            Values::Int32(values) => {
                if self.ty == ColumnType::Date {
                    return Err(Error::internal("an integer appended to a DATE column"));
                }
                values.push(value);
            }
            Values::Varchar(values, heap) => {
                let mut text = String::new();
                let _ = write!(text, "{value}");
                values.push(heap.add(text.as_bytes()));
            }
        }
        Ok(())
    }

    /// `Append<date_t>`.
    fn push_date(&mut self, days: i32) -> Result<()> {
        match (&mut self.values, self.ty) {
            (Values::Int32(values), ColumnType::Date) => {
                values.push(days);
                Ok(())
            }
            _ => Err(Error::internal("a date appended to a column of another type")),
        }
    }

    /// `Append<string_t>`.
    fn push_str(&mut self, value: &str) -> Result<()> {
        match &mut self.values {
            Values::Varchar(values, heap) => {
                values.push(heap.add(value.as_bytes()));
                Ok(())
            }
            _ => Err(Error::internal("a string appended to a column of another type")),
        }
    }

    /// The column as a vector; the buffer starts over.
    fn take_vector(&mut self) -> Vector {
        let ty = self.ty.logical_type();
        let mut vector = match &mut self.values {
            Values::Int64(values) => {
                let mut vector = Vector::flat_for_overwrite(ty, values.len());
                vector.values_mut::<i64>()[..values.len()].copy_from_slice(values);
                values.clear();
                vector
            }
            Values::Int32(values) => {
                let mut vector = Vector::flat_for_overwrite(ty, values.len());
                vector.values_mut::<i32>()[..values.len()].copy_from_slice(values);
                values.clear();
                vector
            }
            Values::Varchar(values, heap) => {
                let mut vector = Vector::flat(ty, values.len());
                vector.add_heap(std::mem::take(heap));
                vector.values_mut::<StringT>()[..values.len()].copy_from_slice(values);
                values.clear();
                vector
            }
        };
        if let Some(validity) = self.validity.take() {
            vector.set_validity(validity);
        }
        vector
    }
}

impl ColumnType {
    pub(crate) fn logical_type(self) -> LogicalType {
        match self {
            Self::BigInt => LogicalType::BIGINT,
            Self::Integer => LogicalType::INTEGER,
            Self::Varchar => LogicalType::VARCHAR,
            Self::Date => LogicalType::DATE,
            Self::Decimal(width, scale) => LogicalType::decimal(width, scale),
        }
    }
}

/// Where the filled chunks of the tables go.
pub(crate) trait ChunkSink {
    fn push(&mut self, table: usize, chunk: DataChunk) -> Result<()>;
}

/// The table and bit of the NULL bitmap `nullCheck` reads for each column of a table: the
/// column `first_column + i` of the table, which may belong to the next table when a table
/// has more columns than streams.
#[derive(Debug)]
enum NullColumns {
    /// Column `i` is bit `i` of the table's own bitmap.
    Own,
    /// The table and bit of each column.
    Mapped(Vec<(usize, u32)>),
}

/// The rows of one table being collected into chunks (`tpcds_append_information` with its
/// `InternalAppender`).
#[derive(Debug)]
pub(crate) struct RowAppender {
    table: usize,
    columns: Vec<ColumnBuffer>,
    null_columns: NullColumns,
    /// The next column of the current row.
    column: usize,
    /// The NULL columns of the current row.
    nulls: u64,
    rows: usize,
}

impl RowAppender {
    pub(crate) fn new(table: usize, info: &TableInfo) -> Self {
        let first_column = get_simple_tdefs_by_number(table).first_column;
        let mapped: Vec<(usize, u32)> = (0..info.columns.len())
            .map(|i| {
                let column = first_column + i;
                let owner = get_table_from_column(column);
                (
                    owner,
                    (column - get_simple_tdefs_by_number(owner).first_column) as u32,
                )
            })
            .collect();
        let own = mapped
            .iter()
            .enumerate()
            .all(|(i, &(owner, bit))| owner == table && bit as usize == i);
        Self {
            table,
            columns: info
                .columns
                .iter()
                .map(|&(_, ty)| ColumnBuffer::new(ty))
                .collect(),
            null_columns: if own {
                NullColumns::Own
            } else {
                NullColumns::Mapped(mapped)
            },
            column: 0,
            nulls: 0,
            rows: 0,
        }
    }

    /// `append_row_start`: the NULL columns of the row are those of the NULL bitmaps of
    /// the tables now.
    pub(crate) fn start(&mut self, null_bit_maps: &[i64; TDEF_COUNT]) -> &mut Self {
        debug_assert_eq!(self.column, 0, "a row was not finished");
        self.nulls = match &self.null_columns {
            NullColumns::Own => null_bit_maps[self.table] as u64,
            NullColumns::Mapped(columns) => columns
                .iter()
                .enumerate()
                .filter(|&(_, &(owner, bit))| null_bit_maps[owner].wrapping_shr(bit) & 1 != 0)
                .fold(0, |nulls, (i, _)| nulls | 1 << i),
        };
        self
    }

    /// `IsNull`: whether the next column is NULL in this row, and that column.
    fn next(&mut self) -> (bool, &mut ColumnBuffer) {
        let column = self.column;
        self.column += 1;
        (
            column < 64 && self.nulls >> column & 1 != 0,
            &mut self.columns[column],
        )
    }

    /// `append_varchar`.
    pub(crate) fn varchar(&mut self, value: &str) -> Result<&mut Self> {
        let (null, column) = self.next();
        if null {
            column.push_null();
        } else {
            column.push_str(value)?;
        }
        Ok(self)
    }

    /// `append_key`: negative keys are NULL.
    pub(crate) fn key(&mut self, value: i64) -> Result<&mut Self> {
        let (null, column) = self.next();
        if null || value < 0 {
            column.push_null();
        } else {
            column.push_i64(value)?;
        }
        Ok(self)
    }

    /// `append_integer_decimal`: an integer as a decimal with two digits after the point.
    pub(crate) fn integer_decimal(&mut self, value: i32) -> Result<&mut Self> {
        let (null, column) = self.next();
        if null {
            column.push_null();
        } else {
            column.push_i32(value.wrapping_mul(100))?;
        }
        Ok(self)
    }

    /// `append_integer`.
    pub(crate) fn integer(&mut self, value: i32) -> Result<&mut Self> {
        let (null, column) = self.next();
        if null {
            column.push_null();
        } else {
            column.push_i32(value)?;
        }
        Ok(self)
    }

    /// `append_date`: a julian date; negative dates are NULL.
    pub(crate) fn date(&mut self, value: i64) -> Result<&mut Self> {
        let (null, column) = self.next();
        if null || value < 0 {
            column.push_null();
            return Ok(self);
        }
        let date = Date::from_julian(value as i32);
        let days = rustdb_types::Date::from_ymd(date.year, date.month, date.day)
            .ok_or_else(|| {
                Error::conversion(format!(
                    "Date out of range: {}-{}-{}",
                    date.year, date.month, date.day
                ))
            })?
            .0;
        column.push_date(days)?;
        Ok(self)
    }

    /// `append_decimal`: the scaled integer of a decimal with as many digits after the
    /// point as its column.
    pub(crate) fn decimal(&mut self, value: &Decimal) -> Result<&mut Self> {
        let (null, column) = self.next();
        if null {
            column.push_null();
        } else {
            debug_assert!(
                matches!(column.ty, ColumnType::Decimal(_, scale) if i32::from(scale) == value.precision)
            );
            column.push_i64(value.number)?;
        }
        Ok(self)
    }

    /// `append_row_end`: hands a full chunk to `sink`.
    pub(crate) fn end(&mut self, sink: &mut dyn ChunkSink) -> Result<()> {
        if self.column != self.columns.len() {
            return Err(Error::invalid_input(
                "Call to EndRow before all columns have been appended to!",
            ));
        }
        self.column = 0;
        self.rows += 1;
        if self.rows == STANDARD_VECTOR_SIZE {
            self.flush(sink)?;
        }
        Ok(())
    }

    /// Hands the rows collected so far to `sink`.
    pub(crate) fn flush(&mut self, sink: &mut dyn ChunkSink) -> Result<()> {
        if self.rows == 0 {
            return Ok(());
        }
        let columns = self.columns.iter_mut().map(ColumnBuffer::take_vector).collect();
        let chunk = DataChunk::new(columns, self.rows);
        self.rows = 0;
        sink.push(self.table, chunk)
    }
}
