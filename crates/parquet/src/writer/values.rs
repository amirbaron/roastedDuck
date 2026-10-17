//! The value encoding of leaf columns (the type-specific parts of DuckDB's
//! `StandardColumnWriter`, `BooleanColumnWriter`, `FixedDecimalColumnWriter` and
//! `EnumColumnWriter`): dictionaries, page encodings and statistics.


use rustdb_common::{Error, Result};
use rustdb_types::{Vector, VectorKind};

use super::bloom::BloomFilter;
use super::encoders::{BssEncoder, DbpEncoder, DlbaEncoder, RleBpEncoder};
use super::stats::{
    BooleanStatistics, DecimalStatistics, StringStatistics, WriterStatistics, decimal_to_bytes,
};
use super::types::{FallbackEncoding, WriteType, xxhash64};
use super::{ParquetVersion, WriterOptions};
use crate::decode::compute_bit_width;
use crate::thrift::{Encoding, Type};

/// A finished dictionary page.
#[derive(Debug)]
pub struct DictionaryPage {
    pub page: Vec<u8>,
    pub count: usize,
    pub bloom_filter: Option<BloomFilter>,
}

/// The type-specific part of a leaf column writer.
pub trait ValueWriter: Send + std::fmt::Debug {
    fn parquet_type(&self) -> Type;
    /// Starts a row group of `num_rows` rows (`InitializeWriteState`).
    fn begin_row_group(&mut self, num_rows: usize, options: &WriterOptions);
    fn has_analyze(&self) -> bool {
        false
    }
    /// Collects dictionary candidates (`Analyze`); `levels_len` is the number of levels
    /// already prepared for this column in the row group.
    fn analyze(
        &mut self,
        vector: &Vector,
        parent: Option<&super::column::Levels>,
        levels_len: usize,
        count: usize,
    ) -> Result<()> {
        let _ = (vector, parent, levels_len, count);
        Ok(())
    }
    fn finalize_analyze(&mut self, options: &WriterOptions) {
        let _ = options;
    }
    /// The encoding of the data pages (`GetEncoding`).
    fn encoding(&self) -> Encoding;
    /// Resets the statistics (`InitializeStatsState`).
    fn init_stats(&mut self);
    /// The statistics, `None` when the column has none.
    fn stats(&self) -> Option<&dyn WriterStatistics>;
    /// Whether every plain value has the same size (`TypeIsConstantSize`).
    fn is_constant_size(&self) -> bool {
        true
    }
    /// The estimated encoded size of a row (`GetRowSize`).
    fn row_size(&self, vector: &Vector, row: usize) -> usize;
    /// Starts a page holding `value_count` values (`InitializePageState`).
    fn begin_page(&mut self, value_count: usize);
    /// Encodes rows `start..end` of `vector` (`WriteVector`).
    fn write_vector(&mut self, out: &mut Vec<u8>, vector: &Vector, start: usize, end: usize) -> Result<()>;
    /// Completes the page (`FlushPageState`).
    fn flush_page(&mut self, out: &mut Vec<u8>) -> Result<()>;
    fn has_dictionary(&self) -> bool {
        false
    }
    fn dictionary_size(&self) -> usize {
        0
    }
    /// The dictionary page (`FlushDictionary`).
    fn flush_dictionary(&mut self, options: &WriterOptions) -> Result<DictionaryPage> {
        let _ = options;
        Err(Error::internal("This page does not have a dictionary"))
    }
}

/// The page state of a [`StandardWriter`] (`StandardWriterPageState`).
#[derive(Debug)]
enum PageEncoder {
    Plain,
    Dictionary {
        encoder: Box<RleBpEncoder>,
        bit_width: u8,
        written_value: bool,
    },
    DeltaBinaryPacked {
        encoder: Box<DbpEncoder>,
        initialized: bool,
    },
    DeltaLengthByteArray {
        encoder: DlbaEncoder,
        initialized: bool,
    },
    ByteStreamSplit {
        encoder: BssEncoder,
        initialized: bool,
    },
}

/// `PrimitiveDictionary`: the distinct values of a row group in first-seen order, with
/// their plain encodings, until it would exceed its limits.
#[derive(Debug)]
struct Dictionary<K> {
    indexes: super::types::FastMap<K, u32>,
    /// The plain-encoded values in index order (`target_stream`).
    target: Vec<u8>,
    maximum_size: usize,
    /// The largest `target` allowed for string values.
    maximum_target_capacity: usize,
    full: bool,
    /// Bloom filter hashes of the values, in index order.
    hashes: Vec<u64>,
}

impl<K: Eq + std::hash::Hash> Dictionary<K> {
    fn new(maximum_size: usize, maximum_target_capacity: usize) -> Self {
        Self {
            indexes: super::types::FastMap::default(),
            target: Vec::new(),
            maximum_size,
            maximum_target_capacity,
            full: false,
            hashes: Vec::new(),
        }
    }

    fn len(&self) -> usize {
        self.indexes.len()
    }

    fn reset(&mut self) {
        self.indexes = super::types::FastMap::default();
        self.target = Vec::new();
        self.hashes = Vec::new();
    }
}

/// `StandardColumnWriter<SRC, TGT, OP>`.
#[derive(Debug)]
pub struct StandardWriter<T: WriteType> {
    encoding: Encoding,
    total_value_count: usize,
    total_string_size: usize,
    key_bit_width: u8,
    dictionary: Dictionary<T::Key>,
    /// Statistics of the dictionary values, used when the dictionary is written.
    dictionary_stats: Option<T::Stats>,
    stats: Option<T::Stats>,
    page: PageEncoder,
    is_string: bool,
}

impl<T: WriteType> StandardWriter<T>
where
    T::Key: std::fmt::Debug,
    T::Stats: std::fmt::Debug,
{
    pub fn new(is_string: bool) -> Self {
        Self {
            encoding: Encoding::PLAIN,
            total_value_count: 0,
            total_string_size: 0,
            key_bit_width: 0,
            dictionary: Dictionary::new(0, 0),
            dictionary_stats: None,
            stats: None,
            page: PageEncoder::Plain,
            is_string,
        }
    }

    /// `PrimitiveDictionary::Insert<true>`.
    fn insert(&mut self, src: T::Src<'_>) -> Result<()> {
        if self.dictionary.full {
            return Ok(());
        }
        if T::lookup(&self.dictionary.indexes, src).is_some() {
            return Ok(());
        }
        let key = T::key(src);
        let size = self.dictionary.len();
        if size + 1 > self.dictionary.maximum_size {
            self.dictionary.full = true;
            return Ok(());
        }
        let before = self.dictionary.target.len();
        T::write_plain(src, &mut self.dictionary.target)?;
        if self.is_string && self.dictionary.target.len() > self.dictionary.maximum_target_capacity {
            self.dictionary.target.truncate(before);
            self.dictionary.full = true;
            return Ok(());
        }
        self.dictionary.indexes.insert(key, size as u32);
        self.dictionary.hashes.push(T::hash(src)?);
        if let Some(stats) = &mut self.dictionary_stats {
            T::update_stats(stats, src)?;
        }
        Ok(())
    }

    fn update_stats(&mut self, src: T::Src<'_>) -> Result<()> {
        if let Some(stats) = &mut self.stats {
            T::update_stats(stats, src)?;
        }
        Ok(())
    }
}

impl<T: WriteType> ValueWriter for StandardWriter<T>
where
    T::Key: std::fmt::Debug,
    T::Stats: std::fmt::Debug,
{
    fn parquet_type(&self) -> Type {
        T::PARQUET_TYPE
    }

    fn begin_row_group(&mut self, num_rows: usize, options: &WriterOptions) {
        let maximum_size = options.dictionary_size_limit.unwrap_or(num_rows / 5);
        self.dictionary = Dictionary::new(maximum_size, options.string_dictionary_page_size_limit);
        self.dictionary_stats = Some(T::new_stats());
        self.encoding = match options.version {
            ParquetVersion::V1 => Encoding::PLAIN_DICTIONARY,
            ParquetVersion::V2 => Encoding::RLE_DICTIONARY,
        };
        self.total_value_count = 0;
        self.total_string_size = 0;
        self.key_bit_width = 0;
    }

    fn has_analyze(&self) -> bool {
        true
    }

    fn analyze(
        &mut self,
        vector: &Vector,
        parent: Option<&super::column::Levels>,
        levels_len: usize,
        count: usize,
    ) -> Result<()> {
        let check_parent_empty = parent.is_some_and(|parent| !parent.is_empty.is_empty());
        let vcount = match parent {
            Some(parent) if check_parent_empty => parent.definition.len() - levels_len,
            _ => count,
        };
        let validity = vector.validity();
        let batch = T::batch(vector);
        let mut vector_index = 0;
        for i in 0..vcount {
            if check_parent_empty && parent.is_some_and(|parent| parent.is_empty[levels_len + i] != 0) {
                continue;
            }
            if validity.is_valid(vector_index) {
                let src = T::get(batch, vector_index);
                self.insert(src)?;
                self.total_value_count += 1;
                self.total_string_size += T::string_bytes(src).map_or(0, <[u8]>::len);
            }
            vector_index += 1;
        }
        Ok(())
    }

    fn finalize_analyze(&mut self, options: &WriterOptions) {
        if self.dictionary.len() == 0 || self.dictionary.full {
            self.dictionary.reset();
            self.dictionary_stats = None;
            self.encoding = match options.version {
                ParquetVersion::V1 => Encoding::PLAIN,
                ParquetVersion::V2 => match T::FALLBACK {
                    FallbackEncoding::Plain => Encoding::PLAIN,
                    FallbackEncoding::DeltaBinaryPacked => Encoding::DELTA_BINARY_PACKED,
                    FallbackEncoding::DeltaLengthByteArray => Encoding::DELTA_LENGTH_BYTE_ARRAY,
                    FallbackEncoding::ByteStreamSplit => Encoding::BYTE_STREAM_SPLIT,
                },
            };
        } else {
            self.key_bit_width = compute_bit_width(self.dictionary.len() as u64);
        }
    }

    fn encoding(&self) -> Encoding {
        self.encoding
    }

    fn init_stats(&mut self) {
        self.stats = T::HAS_STATISTICS.then(T::new_stats);
    }

    fn stats(&self) -> Option<&dyn WriterStatistics> {
        self.stats.as_ref().map(|stats| stats as &dyn WriterStatistics)
    }

    fn is_constant_size(&self) -> bool {
        T::is_constant_size()
    }

    fn row_size(&self, vector: &Vector, row: usize) -> usize {
        if self.has_dictionary() {
            usize::from(self.key_bit_width).div_ceil(8)
        } else {
            T::row_size(T::src(vector, row))
        }
    }

    fn begin_page(&mut self, value_count: usize) {
        self.page = match self.encoding {
            Encoding::PLAIN_DICTIONARY | Encoding::RLE_DICTIONARY => {
                let bit_width = compute_bit_width(self.dictionary.len() as u64);
                PageEncoder::Dictionary {
                    encoder: Box::new(RleBpEncoder::new(bit_width)),
                    bit_width,
                    written_value: false,
                }
            }
            Encoding::DELTA_BINARY_PACKED => PageEncoder::DeltaBinaryPacked {
                encoder: Box::new(DbpEncoder::new(value_count as u64)),
                initialized: false,
            },
            Encoding::DELTA_LENGTH_BYTE_ARRAY => PageEncoder::DeltaLengthByteArray {
                encoder: DlbaEncoder::new(value_count as u64, self.total_string_size),
                initialized: false,
            },
            Encoding::BYTE_STREAM_SPLIT => {
                let width = match T::PARQUET_TYPE {
                    Type::FLOAT | Type::INT32 => 4,
                    _ => 8,
                };
                PageEncoder::ByteStreamSplit {
                    encoder: BssEncoder::new(value_count, width),
                    initialized: false,
                }
            }
            _ => PageEncoder::Plain,
        };
    }

    fn write_vector(&mut self, out: &mut Vec<u8>, vector: &Vector, start: usize, end: usize) -> Result<()> {
        let validity = vector.validity().clone();
        let batch = T::batch(vector);
        let mut page = std::mem::replace(&mut self.page, PageEncoder::Plain);
        let result = (|| -> Result<()> {
            match &mut page {
                PageEncoder::Dictionary {
                    encoder,
                    bit_width,
                    written_value,
                } => {
                    for row in start..end {
                        if !validity.is_valid(row) {
                            continue;
                        }
                        if !*written_value {
                            out.push(*bit_width);
                            encoder.begin_write();
                            *written_value = true;
                        }
                        let index = T::lookup(&self.dictionary.indexes, T::get(batch, row))
                            .ok_or_else(|| Error::internal("value missing from the Parquet dictionary"))?;
                        encoder.write_value(out, index);
                    }
                }
                PageEncoder::DeltaBinaryPacked { encoder, initialized } => {
                    for row in start..end {
                        if !validity.is_valid(row) {
                            continue;
                        }
                        let src = T::get(batch, row);
                        self.update_stats(src)?;
                        let value = T::delta_value(src)?;
                        if *initialized {
                            encoder.write_value(out, value);
                        } else {
                            encoder.begin_write(out, value);
                            *initialized = true;
                        }
                    }
                }
                PageEncoder::DeltaLengthByteArray { encoder, initialized } => {
                    for row in start..end {
                        if !validity.is_valid(row) {
                            continue;
                        }
                        let src = T::get(batch, row);
                        self.update_stats(src)?;
                        let bytes = T::string_bytes(src)
                            .ok_or_else(|| Error::internal("DlbaEncoder should only be used with strings"))?;
                        if *initialized {
                            encoder.write_value(out, bytes);
                        } else {
                            encoder.begin_write(out, bytes);
                            *initialized = true;
                        }
                    }
                }
                PageEncoder::ByteStreamSplit { encoder, initialized } => {
                    if !*initialized {
                        encoder.begin_write();
                        *initialized = true;
                    }
                    let mut bytes = Vec::with_capacity(8);
                    for row in start..end {
                        if !validity.is_valid(row) {
                            continue;
                        }
                        let src = T::get(batch, row);
                        self.update_stats(src)?;
                        bytes.clear();
                        T::write_plain(src, &mut bytes)?;
                        encoder.write_value(&bytes);
                    }
                }
                PageEncoder::Plain => {
                    for row in start..end {
                        if !validity.is_valid(row) {
                            continue;
                        }
                        let src = T::get(batch, row);
                        self.update_stats(src)?;
                        T::write_plain(src, out)?;
                    }
                }
            }
            Ok(())
        })();
        self.page = page;
        result
    }

    fn flush_page(&mut self, out: &mut Vec<u8>) -> Result<()> {
        match &mut self.page {
            PageEncoder::DeltaBinaryPacked { encoder, initialized } => {
                if !*initialized {
                    encoder.begin_write(out, 0);
                }
                encoder.finish_write(out);
            }
            PageEncoder::Dictionary {
                encoder,
                bit_width,
                written_value,
            } => {
                if *written_value {
                    encoder.finish_write(out);
                } else {
                    // Only NULLs: just the bit width.
                    out.push(*bit_width);
                }
            }
            PageEncoder::DeltaLengthByteArray { encoder, initialized } => {
                if !*initialized {
                    encoder.begin_write(out, b"");
                }
                encoder.finish_write(out);
            }
            PageEncoder::ByteStreamSplit { encoder, initialized } => {
                if !*initialized {
                    encoder.begin_write();
                }
                encoder.finish_write(out);
            }
            PageEncoder::Plain => {}
        }
        Ok(())
    }

    fn has_dictionary(&self) -> bool {
        matches!(
            self.encoding,
            Encoding::RLE_DICTIONARY | Encoding::PLAIN_DICTIONARY
        )
    }

    fn dictionary_size(&self) -> usize {
        self.dictionary.len()
    }

    fn flush_dictionary(&mut self, options: &WriterOptions) -> Result<DictionaryPage> {
        let bloom_filter = options.enable_bloom_filters.then(|| {
            let mut filter =
                BloomFilter::new(self.dictionary.len(), options.bloom_filter_false_positive_ratio);
            for &hash in &self.dictionary.hashes {
                filter.insert(hash);
            }
            filter
        });
        // The dictionary values contribute to the statistics.
        if let (Some(dictionary_stats), true) = (self.dictionary_stats.take(), self.stats.is_some()) {
            self.stats = Some(dictionary_stats);
        }
        Ok(DictionaryPage {
            page: std::mem::take(&mut self.dictionary.target),
            count: self.dictionary.len(),
            bloom_filter,
        })
    }
}

/// `BooleanColumnWriter`: bit-packed booleans.
#[derive(Debug, Default)]
pub struct BooleanWriter {
    stats: BooleanStatistics,
    byte: u8,
    byte_position: u8,
}

impl ValueWriter for BooleanWriter {
    fn parquet_type(&self) -> Type {
        Type::BOOLEAN
    }
    fn begin_row_group(&mut self, _: usize, _: &WriterOptions) {}
    fn encoding(&self) -> Encoding {
        Encoding::PLAIN
    }
    fn init_stats(&mut self) {
        self.stats = BooleanStatistics::default();
    }
    fn stats(&self) -> Option<&dyn WriterStatistics> {
        Some(&self.stats)
    }
    fn row_size(&self, _: &Vector, _: usize) -> usize {
        1
    }
    fn begin_page(&mut self, _: usize) {
        self.byte = 0;
        self.byte_position = 0;
    }
    fn write_vector(&mut self, out: &mut Vec<u8>, vector: &Vector, start: usize, end: usize) -> Result<()> {
        let validity = vector.validity();
        for (row, &value) in vector.values::<bool>().iter().enumerate().take(end).skip(start) {
            if !validity.is_valid(row) {
                continue;
            }
            self.stats.max |= value;
            self.stats.min &= value;
            self.byte |= u8::from(value) << self.byte_position;
            self.byte_position += 1;
            if self.byte_position == 8 {
                out.push(self.byte);
                self.byte = 0;
                self.byte_position = 0;
            }
        }
        Ok(())
    }
    fn flush_page(&mut self, out: &mut Vec<u8>) -> Result<()> {
        if self.byte_position > 0 {
            out.push(self.byte);
            self.byte = 0;
            self.byte_position = 0;
        }
        Ok(())
    }
}

/// `FixedDecimalColumnWriter`: 128-bit decimals as 16 big-endian bytes.
#[derive(Debug, Default)]
pub struct FixedDecimalWriter {
    stats: DecimalStatistics,
}

impl ValueWriter for FixedDecimalWriter {
    fn parquet_type(&self) -> Type {
        Type::FIXED_LEN_BYTE_ARRAY
    }
    fn begin_row_group(&mut self, _: usize, _: &WriterOptions) {}
    fn encoding(&self) -> Encoding {
        Encoding::PLAIN
    }
    fn init_stats(&mut self) {
        self.stats = DecimalStatistics::default();
    }
    fn stats(&self) -> Option<&dyn WriterStatistics> {
        Some(&self.stats)
    }
    fn row_size(&self, _: &Vector, _: usize) -> usize {
        16
    }
    fn begin_page(&mut self, _: usize) {}
    fn write_vector(&mut self, out: &mut Vec<u8>, vector: &Vector, start: usize, end: usize) -> Result<()> {
        let validity = vector.validity();
        for (row, &value) in vector.values::<i128>().iter().enumerate().take(end).skip(start) {
            if !validity.is_valid(row) {
                continue;
            }
            self.stats.min = self.stats.min.min(value);
            self.stats.max = self.stats.max.max(value);
            out.extend_from_slice(&decimal_to_bytes(value));
        }
        Ok(())
    }
    fn flush_page(&mut self, _: &mut Vec<u8>) -> Result<()> {
        Ok(())
    }
}

/// `EnumColumnWriter`: the enum's index, with the enum values as dictionary.
#[derive(Debug)]
pub struct EnumWriter {
    values: Vec<String>,
    bit_width: u8,
    stats: StringStatistics,
    encoder: RleBpEncoder,
    written_value: bool,
}

impl EnumWriter {
    pub fn new(values: Vec<String>) -> Self {
        let bit_width = compute_bit_width(values.len() as u64);
        Self {
            values,
            bit_width,
            stats: StringStatistics::default(),
            encoder: RleBpEncoder::new(bit_width),
            written_value: false,
        }
    }

    fn index(vector: &Vector, row: usize) -> Result<u32> {
        use rustdb_types::PhysicalType as P;
        Ok(match vector.logical_type().physical_type() {
            P::UInt8 => u32::from(vector.values::<u8>()[row]),
            P::UInt16 => u32::from(vector.values::<u16>()[row]),
            P::UInt32 => vector.values::<u32>()[row],
            _ => return Err(Error::internal("Unsupported internal enum type")),
        })
    }
}

impl ValueWriter for EnumWriter {
    fn parquet_type(&self) -> Type {
        Type::BYTE_ARRAY
    }
    fn begin_row_group(&mut self, _: usize, _: &WriterOptions) {}
    fn encoding(&self) -> Encoding {
        Encoding::RLE_DICTIONARY
    }
    fn init_stats(&mut self) {
        self.stats = StringStatistics::new(false);
        self.stats.inexact = true;
    }
    fn stats(&self) -> Option<&dyn WriterStatistics> {
        Some(&self.stats)
    }
    fn row_size(&self, _: &Vector, _: usize) -> usize {
        usize::from(self.bit_width).div_ceil(8)
    }
    fn begin_page(&mut self, _: usize) {
        self.encoder = RleBpEncoder::new(self.bit_width);
        self.written_value = false;
    }
    fn write_vector(&mut self, out: &mut Vec<u8>, vector: &Vector, start: usize, end: usize) -> Result<()> {
        let validity = vector.validity();
        for row in start..end {
            if !validity.is_valid(row) {
                continue;
            }
            if !self.written_value {
                out.push(self.bit_width);
                self.encoder.begin_write();
                self.written_value = true;
            }
            self.encoder.write_value(out, Self::index(vector, row)?);
        }
        Ok(())
    }
    fn flush_page(&mut self, out: &mut Vec<u8>) -> Result<()> {
        if self.written_value {
            self.encoder.finish_write(out);
        } else {
            out.push(self.bit_width);
        }
        Ok(())
    }
    fn has_dictionary(&self) -> bool {
        true
    }
    fn dictionary_size(&self) -> usize {
        self.values.len()
    }
    fn flush_dictionary(&mut self, _: &WriterOptions) -> Result<DictionaryPage> {
        let mut page = Vec::new();
        for value in &self.values {
            self.stats.update(value.as_bytes());
            page.extend_from_slice(&(value.len() as u32).to_le_bytes());
            page.extend_from_slice(value.as_bytes());
        }
        Ok(DictionaryPage {
            page,
            count: self.values.len(),
            bloom_filter: None,
        })
    }
}

/// Makes nested vectors flat all the way down, as the writers index slots directly.
pub fn deep_flatten(vector: &Vector, count: usize) -> Result<Vector> {
    if vector.kind() == VectorKind::Flat && children_flat(vector) {
        return Ok(vector.clone());
    }
    let mut flat = Vector::flat(vector.logical_type().clone(), count.max(1));
    crate::vector_ops::copy_rows(vector, 0, count, &mut flat, 0)?;
    Ok(flat)
}

fn children_flat(vector: &Vector) -> bool {
    if let Some(children) = vector.struct_children() {
        return children
            .iter()
            .all(|child| child.kind() == VectorKind::Flat && children_flat(child));
    }
    if let Some((child, _)) = vector.list_child() {
        return child.kind() == VectorKind::Flat && children_flat(child);
    }
    if let Some(child) = vector.array_child() {
        return child.kind() == VectorKind::Flat && children_flat(child);
    }
    true
}

/// Hashes a value the way the writer's bloom filters do.
pub fn hash_bytes(bytes: &[u8]) -> u64 {
    xxhash64(bytes)
}
