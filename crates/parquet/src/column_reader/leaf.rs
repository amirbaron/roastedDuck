//! The reader of one leaf column chunk (DuckDB's base `ColumnReader` with its
//! `DictionaryDecoder`, `RLEDecoder`, `DeltaBinaryPackedDecoder`,
//! `DeltaLengthByteArrayDecoder`, `DeltaByteArrayDecoder` and `ByteStreamSplitDecoder`):
//! page headers, decompression, levels and value decoding.

use std::sync::Arc;

use rustdb_common::{Error, Result, STANDARD_VECTOR_SIZE};
use rustdb_storage::TableFilter;
use rustdb_types::{LogicalType, SelectionVector, StringT, Vector, VectorKind};

use super::ReaderContext;
use super::values::{
    ByteBooleanDecoder, PlainDecoder, PlainSource, StringDecoder, create_decoder, valid_count,
};
use crate::compression::decompress;
use crate::decode::{
    BlockCursor, BssDecoder, ByteBuffer, DbpDecoder, PageBlock, RleBpDecoder, compute_bit_width,
    runtime_error,
};
use crate::file::{ParquetFile, ReadHead};
use crate::schema::ColumnSchema;
use crate::thrift::{
    ColumnChunk, CompactReader, CompressionCodec, Encoding, PageHeader, PageType, RowGroup, ThriftValue, Type,
};
use rustdb_types::vector_ops::copy_rows;

/// The minimum number of bytes fetched when a page header lies past the loaded chunk.
const MIN_FETCH: usize = 1 << 16;

/// The bytes of a column chunk, loaded on first use (a prefetched `ThriftFileTransport`).
#[derive(Debug, Default)]
struct ChunkBuffer {
    /// The file offset of `data[0]`.
    start: u64,
    data: Vec<u8>,
    /// In the prefetch mode, the prefetched range holding the chunk: bytes it holds are
    /// read from it instead of `data` (`ThriftFileTransport::read` from a read head).
    head: Option<Arc<ReadHead>>,
}

impl ChunkBuffer {
    /// Whether the prefetched range holds the `len` bytes at `offset`.
    fn head_holds(&self, offset: u64, len: u64) -> bool {
        self.head
            .as_ref()
            .is_some_and(|head| offset >= head.location && offset.saturating_add(len) <= head.end())
    }

    /// The `len` bytes at `offset` of the prefetched range, which holds them.
    fn head_bytes(&self, file: &ParquetFile, offset: u64, len: usize) -> Result<&[u8]> {
        let head = self
            .head
            .as_ref()
            .ok_or_else(|| Error::internal("column chunk without a prefetched range"))?;
        let begin = (offset - head.location) as usize;
        Ok(&head.bytes(file)?[begin..begin + len])
    }

    /// Loads `len` bytes at `offset`, reusing the buffer's allocation.
    fn load(&mut self, file: &ParquetFile, offset: u64, len: usize) -> Result<()> {
        resize_uninitialized(&mut self.data, len);
        self.start = offset;
        let result = file.read_exact_at(&mut self.data, offset);
        if result.is_err() {
            self.data.clear();
        }
        result
    }

    /// The bytes `offset..offset + len`, loading them if needed.
    fn bytes(&mut self, file: &ParquetFile, offset: u64, len: usize) -> Result<&[u8]> {
        if self.head_holds(offset, len as u64) {
            return self.head_bytes(file, offset, len);
        }
        let end = offset.checked_add(len as u64);
        let loaded_end = self.start + self.data.len() as u64;
        if offset < self.start || end.is_none_or(|end| end > loaded_end) {
            let available = file.size().saturating_sub(offset);
            let fetch = (len.max(MIN_FETCH) as u64).min(available);
            if (fetch as usize) < len {
                return Err(runtime_error(format!(
                    "Could not read enough bytes from file \"{}\": attempted to read {len} bytes from location {offset}",
                    file.path()
                )));
            }
            self.load(file, offset, fetch as usize)?;
        }
        let begin = (offset - self.start) as usize;
        Ok(&self.data[begin..begin + len])
    }

    /// Parses a page header at `offset`, returning it and its encoded length.
    fn page_header(&mut self, file: &ParquetFile, offset: u64) -> Result<(PageHeader, usize)> {
        if self.head_holds(offset, 1) {
            let head_end = self.head.as_ref().map_or(offset, |head| head.end());
            let bytes = self.head_bytes(file, offset, (head_end - offset) as usize)?;
            let mut reader = CompactReader::new(bytes);
            if let Ok(header) = PageHeader::read(&mut reader) {
                return Ok((header, reader.position()));
            }
        }
        let loaded_end = self.start + self.data.len() as u64;
        if offset >= self.start && offset < loaded_end {
            let begin = (offset - self.start) as usize;
            let mut reader = CompactReader::new(&self.data[begin..]);
            if let Ok(header) = PageHeader::read(&mut reader) {
                return Ok((header, reader.position()));
            }
            if loaded_end >= file.size() {
                // The whole rest of the file is loaded: the header is corrupt.
                let mut reader = CompactReader::new(&self.data[begin..]);
                let header = PageHeader::read(&mut reader)?;
                return Ok((header, reader.position()));
            }
        }
        // Load the rest of the chunk (or file) from the header on.
        let available = file.size().saturating_sub(offset) as usize;
        let len = available.min(MIN_FETCH.max(self.data.len() * 2));
        self.bytes(file, offset, len)?;
        let mut reader = CompactReader::new(&self.data[..]);
        let header = PageHeader::read(&mut reader)?;
        Ok((header, reader.position()))
    }
}

/// The AAD ordinals of the pages being read from an encrypted file (DuckDB's
/// `CryptoMetaData` of a column reader).
#[derive(Debug, Default, Clone, Copy)]
struct PageCrypto {
    row_group: i16,
    /// The page ordinal `ReadPageHeaders` counts (from 0 per call).
    page_ordinal: i16,
    has_dictionary: bool,
    has_index: bool,
    has_bloom_filter: bool,
}

impl PageCrypto {
    /// `GetFinalPageOrdinal`.
    fn final_ordinal(self, module_type: i8, ordinal: i16) -> i16 {
        use crate::crypto::module;
        match module_type {
            module::DATA_PAGE_HEADER => {
                if self.has_dictionary || self.has_index || self.has_bloom_filter {
                    ordinal - 1
                } else {
                    ordinal
                }
            }
            module::DATA_PAGE => ordinal,
            _ => -1,
        }
    }

    /// `GetModuleHeader`.
    fn header_module(self, ordinal: i16) -> i8 {
        use crate::crypto::module;
        if ordinal > 0 {
            module::DATA_PAGE_HEADER
        } else if self.has_dictionary {
            module::DICTIONARY_PAGE_HEADER
        } else if self.has_index {
            module::OFFSET_INDEX
        } else if self.has_bloom_filter {
            module::BLOOM_FILTER_HEADER
        } else {
            module::DATA_PAGE_HEADER
        }
    }

    /// `GetModule`.
    fn data_module(self, page_type: PageType, ordinal: i16) -> Result<i8> {
        use crate::crypto::module;
        if self.has_bloom_filter && ordinal == 0 {
            return Ok(module::BLOOM_FILTER_BITSET);
        }
        Ok(match page_type {
            PageType::DATA_PAGE | PageType::DATA_PAGE_V2 => module::DATA_PAGE,
            PageType::DICTIONARY_PAGE => module::DICTIONARY_PAGE,
            PageType::INDEX_PAGE if self.has_index => module::OFFSET_INDEX,
            PageType::INDEX_PAGE => module::COLUMN_INDEX,
            _ => return Err(Error::invalid_input("Module not found")),
        })
    }
}

/// The decoding of the current data page (`ColumnEncoding` with its decoder state).
#[derive(Debug, Default)]
enum PageDecoding {
    #[default]
    Invalid,
    Plain,
    Dictionary(RleBpDecoder),
    /// RLE-encoded booleans.
    Rle(RleBpDecoder),
    DeltaBinaryPacked(Box<DbpDecoder>),
    /// The value lengths and the next one; the bytes follow in the block.
    DeltaLengthByteArray {
        lengths: Vec<u32>,
        next: usize,
    },
    /// The values re-encoded as plain values in their own block.
    DeltaByteArray {
        block: PageBlock,
        position: usize,
    },
    ByteStreamSplit(BssDecoder),
}

/// Reads one leaf column chunk after another.
pub struct LeafReader {
    logical_type: LogicalType,
    max_define: u8,
    max_repeat: u8,
    column_index: usize,
    parquet_type: Type,
    /// The length of `FIXED_LEN_BYTE_ARRAY` values.
    type_length: usize,
    decoder: Box<dyn PlainDecoder>,
    context: ReaderContext,
    /// Validation of `DELTA_LENGTH_BYTE_ARRAY` strings.
    strings: Option<StringDecoder>,

    codec: CompressionCodec,
    total_compressed_size: u64,
    chunk_read_offset: u64,
    group_rows_available: u64,
    chunk: ChunkBuffer,
    /// The bytes of the chunk from its first page on (within the file).
    chunk_len: u64,

    /// The decompressed current page (one padding byte at the end, like DuckDB's).
    block: PageBlock,
    /// The position of the next value in `block`.
    block_position: usize,
    block_end: usize,
    page_rows_available: u64,
    decoding: PageDecoding,
    defines: Option<RleBpDecoder>,
    repeats: Option<RleBpDecoder>,
    /// The dictionary values, with a NULL slot at `dictionary_size` when the column has
    /// definition levels.
    dictionary: Option<Vector>,
    dictionary_size: usize,
    /// For each filter of the scan, which dictionary entries pass it (computed on first
    /// use; `DictionaryDecoder`'s `filter_result`).
    dictionary_filters: Vec<Option<Vec<bool>>>,
    pending_skips: u64,
    /// The AAD state of an encrypted file (`aad_crypto_metadata`).
    crypto: PageCrypto,
    /// Scratch space for decoded values and dictionary indices.
    scratch: Vec<u8>,
    indices: Vec<u32>,
}

impl std::fmt::Debug for LeafReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LeafReader")
            .field("type", &self.logical_type)
            .field("column_index", &self.column_index)
            .finish_non_exhaustive()
    }
}

fn read_failure(file_name: &str, what: &str) -> Error {
    Error::invalid_input(format!("Failed to read file \"{file_name}\": {what}"))
}

impl LeafReader {
    pub fn new(context: &ReaderContext, schema: &ColumnSchema) -> Result<Self> {
        let decoder = create_decoder(schema, &context.file_name)?;
        let strings = (schema.logical_type.physical_type() == rustdb_types::PhysicalType::Varchar)
            .then(|| StringDecoder::new(schema, Arc::clone(&context.file_name)));
        Ok(Self {
            logical_type: schema.logical_type.clone(),
            max_define: schema.max_define,
            max_repeat: schema.max_repeat,
            column_index: schema.column_index,
            parquet_type: schema.parquet_type,
            type_length: schema.type_length as usize,
            decoder,
            context: context.clone(),
            strings,
            codec: CompressionCodec::UNCOMPRESSED,
            total_compressed_size: 0,
            chunk_read_offset: 0,
            group_rows_available: 0,
            chunk: ChunkBuffer::default(),
            chunk_len: 0,
            block: PageBlock::default(),
            block_position: 0,
            block_end: 0,
            page_rows_available: 0,
            decoding: PageDecoding::Invalid,
            defines: None,
            repeats: None,
            dictionary: None,
            dictionary_size: 0,
            dictionary_filters: Vec::new(),
            pending_skips: 0,
            crypto: PageCrypto::default(),
            scratch: Vec::new(),
            indices: Vec::new(),
        })
    }

    pub fn logical_type(&self) -> &LogicalType {
        &self.logical_type
    }

    fn file_name(&self) -> &str {
        &self.context.file_name
    }

    fn has_defines(&self) -> bool {
        self.max_define > 0
    }

    fn has_repeats(&self) -> bool {
        self.max_repeat > 0
    }

    pub fn group_rows_available(&self) -> u64 {
        self.group_rows_available
    }

    pub fn total_compressed_size(&self) -> u64 {
        self.total_compressed_size
    }

    /// `ColumnReader::InitializeRead`.
    pub fn initialize_read(&mut self, row_group: &RowGroup) -> Result<()> {
        let chunk: &ColumnChunk = row_group
            .columns
            .get(self.column_index)
            .ok_or_else(|| Error::internal("column chunk index out of range"))?;
        if chunk.file_path.is_some() {
            return Err(Error::invalid_input(format!(
                "Failed to read file \"{}\": Only inlined data files are supported (no references)",
                self.file_name()
            )));
        }
        let meta = chunk.meta_data.clone().unwrap_or_default();
        // `InitializeCryptoMetadata`: the row group's ordinal and the column's.
        self.crypto = PageCrypto {
            row_group: row_group.ordinal.unwrap_or(0),
            page_ordinal: 0,
            has_dictionary: meta.dictionary_page_offset.is_some(),
            has_index: meta.index_page_offset.is_some(),
            has_bloom_filter: meta.bloom_filter_offset.is_some(),
        };
        self.chunk_read_offset = meta.data_page_offset as u64;
        if let Some(dictionary_offset) = meta.dictionary_page_offset
            && dictionary_offset >= 4
        {
            self.chunk_read_offset = dictionary_offset as u64;
        }
        self.group_rows_available = meta.num_values as u64;
        self.codec = meta.codec;
        self.total_compressed_size = meta.total_compressed_size as u64;
        // Load the chunk lazily from its first page (`FileOffset`) on.
        let mut start = meta.data_page_offset as u64;
        if let Some(offset) = meta.dictionary_page_offset {
            start = start.min(offset as u64);
        }
        if let Some(offset) = meta.index_page_offset {
            start = start.min(offset as u64);
        }
        // Keep the allocation of the previous chunk.
        self.chunk.start = start;
        self.chunk.data.clear();
        self.chunk.head = None;
        // From the first page to the end of the chunk, as far as the file goes.
        let available = self.context.file.size().saturating_sub(start);
        self.chunk_len = self
            .total_compressed_size
            .saturating_add(self.chunk_read_offset.saturating_sub(start))
            .min(available);
        self.pending_skips = 0;
        self.page_rows_available = 0;
        self.dictionary = None;
        self.dictionary_size = 0;
        self.decoding = PageDecoding::Invalid;
        self.decoder.reset_page();
        Ok(())
    }

    /// Loads the chunk bytes on the first page read, unless a prefetched range holds
    /// them.
    fn ensure_chunk_loaded(&mut self) -> Result<()> {
        if !self.chunk.data.is_empty() || self.chunk.head_holds(self.chunk.start, self.chunk_len) {
            return Ok(());
        }
        let start = self.chunk.start;
        let len = self.chunk_len as usize;
        if len > 0 {
            let file = Arc::clone(&self.context.file);
            self.chunk.load(&file, start, len)?;
        }
        Ok(())
    }

    /// Registers the column chunk for prefetching (`ColumnReader::RegisterPrefetch` at
    /// `FileOffset()`).
    pub fn register_prefetch(
        &self,
        buffer: &mut crate::file::ReadAheadBuffer,
        file: &ParquetFile,
        allow_merge: bool,
    ) -> Result<()> {
        buffer.add_read_head(file, self.chunk.start, self.total_compressed_size, allow_merge)
    }

    /// Reads the chunk from the prefetched range holding its start, if any.
    pub fn attach_read_head(&mut self, buffer: &crate::file::ReadAheadBuffer) {
        self.chunk.head = buffer.read_head(self.chunk.start).cloned();
    }

    /// Outside the prefetch mode, DuckDB reads every page header through the external
    /// file cache as a prefetch of up to 256 bytes (`PrepareRead`'s
    /// `ASSUMED_HEADER_SIZE`): the cache gets the same range, filled from the loaded
    /// chunk where it holds it.
    fn cache_page_header_range(&self, offset: u64) -> Result<()> {
        const ASSUMED_HEADER_SIZE: u64 = 256;
        let file = &self.context.file;
        if self.context.prefetch || !file.handle().caches_reads() {
            return Ok(());
        }
        let len = file.size().saturating_sub(offset).min(ASSUMED_HEADER_SIZE);
        let loaded_end = self.chunk.start + self.chunk.data.len() as u64;
        if offset >= self.chunk.start && offset + len <= loaded_end {
            let begin = (offset - self.chunk.start) as usize;
            file.handle().read_from(len, offset, &self.chunk.data[begin..])?;
        } else {
            file.handle().read(len, offset)?;
        }
        Ok(())
    }

    /// `PrepareRead`: reads the next page header and prepares its page.
    fn prepare_page(&mut self) -> Result<()> {
        // Drop the decoders of the previous page first: they share its block, which can
        // then be reused for the next one.
        self.decoding = PageDecoding::Invalid;
        self.defines = None;
        self.repeats = None;
        self.ensure_chunk_loaded()?;
        self.cache_page_header_range(self.chunk_read_offset)?;
        let file = Arc::clone(&self.context.file);
        let (header, header_len) = match self.context.decryption.clone() {
            Some(decryption) => self.encrypted_page_header(&decryption)?,
            None => self.chunk.page_header(&file, self.chunk_read_offset)?,
        };
        self.chunk_read_offset += header_len as u64;
        if header.compressed_page_size < 0 || header.uncompressed_page_size < 0 {
            return Err(read_failure(self.file_name(), "Page sizes must be >= 0"));
        }
        match header.type_ {
            PageType::DATA_PAGE_V2 => {
                self.prepare_page_v2(&header)?;
                self.prepare_data_page(&header)?;
            }
            PageType::DATA_PAGE => {
                self.prepare_page_v1(&header)?;
                self.prepare_data_page(&header)?;
            }
            PageType::DICTIONARY_PAGE => {
                self.prepare_page_v1(&header)?;
                let size = header
                    .dictionary_page_header
                    .as_ref()
                    .map_or(0, |dict| dict.num_values);
                let size = usize::try_from(size).map_err(|_| {
                    read_failure(
                        self.file_name(),
                        "Invalid dictionary page header (num_values < 0)",
                    )
                })?;
                self.initialize_dictionary(size)?;
            }
            _ => {
                // Index pages and unknown page types are skipped.
                self.chunk_read_offset += header.compressed_page_size as u64;
                if self.context.decryption.is_some() {
                    // The module's length field and nonce/tag are not in the page size.
                    self.chunk_read_offset += crate::crypto::LENGTH_BYTES as u64;
                }
            }
        }
        self.decoder.reset_page();
        Ok(())
    }

    /// `ReadEncrypted` of a page header: decrypts the header module at the current
    /// position; returns the header and the module's length.
    fn encrypted_page_header(
        &mut self,
        decryption: &crate::crypto::FileDecryption,
    ) -> Result<(PageHeader, usize)> {
        let module_type = self.crypto.header_module(self.crypto.page_ordinal);
        self.crypto.page_ordinal = self.crypto.final_ordinal(module_type, self.crypto.page_ordinal);
        let aad = crate::crypto::module_aad(
            &decryption.file_aad,
            module_type,
            self.crypto.row_group,
            self.column_index as i16,
            self.crypto.page_ordinal,
        )?;
        let file = Arc::clone(&self.context.file);
        let offset = self.chunk_read_offset;
        let total =
            crate::crypto::module_len(self.chunk.bytes(&file, offset, crate::crypto::LENGTH_BYTES)?)?;
        let bytes = self.chunk.bytes(&file, offset, total)?;
        let (plain, total) = crate::crypto::decrypt_module(&decryption.key, &aad, bytes)?;
        let header = PageHeader::read(&mut crate::thrift::CompactReader::new(&plain))?;
        Ok((header, total))
    }

    /// The bytes of a page's payload (`compressed_page_size`, without the encryption
    /// overhead Arrow counts in it) at the current position, decrypted for an encrypted
    /// file (`ReadData` / `ReadDataEncrypted`).
    fn read_page_payload(&mut self, header: &PageHeader) -> Result<std::borrow::Cow<'_, [u8]>> {
        let Some(decryption) = self.context.decryption.clone() else {
            let len = header.compressed_page_size as usize;
            return self.read_page_bytes(len).map(std::borrow::Cow::Borrowed);
        };
        let len = self.payload_size(header);
        let module_type = self.crypto.data_module(header.type_, self.crypto.page_ordinal)?;
        self.crypto.page_ordinal = self.crypto.final_ordinal(module_type, self.crypto.page_ordinal);
        let aad = crate::crypto::module_aad(
            &decryption.file_aad,
            module_type,
            self.crypto.row_group,
            self.column_index as i16,
            self.crypto.page_ordinal,
        )?;
        let file = Arc::clone(&self.context.file);
        let offset = self.chunk_read_offset;
        let total =
            crate::crypto::module_len(self.chunk.bytes(&file, offset, crate::crypto::LENGTH_BYTES)?)?;
        let bytes = self.chunk.bytes(&file, offset, total)?;
        let (plain, total) = crate::crypto::decrypt_data(&decryption.key, &aad, bytes, len)?;
        self.chunk_read_offset += total as u64;
        Ok(std::borrow::Cow::Owned(plain))
    }

    /// The size of a page's payload: `compressed_page_size`, less the encryption
    /// overhead Arrow adds to it in files with an AAD file identifier.
    fn payload_size(&self, header: &PageHeader) -> usize {
        let size = header.compressed_page_size as usize;
        match &self.context.decryption {
            Some(decryption) if !decryption.file_aad.is_empty() => size.saturating_sub(
                crate::crypto::LENGTH_BYTES + crate::crypto::NONCE_BYTES + crate::crypto::TAG_BYTES,
            ),
            _ => size,
        }
    }

    /// Reads `len` page bytes at the current position.
    fn read_page_bytes(&mut self, len: usize) -> Result<&[u8]> {
        let file = Arc::clone(&self.context.file);
        let offset = self.chunk_read_offset;
        self.chunk_read_offset += len as u64;
        self.chunk.bytes(&file, offset, len)
    }

    /// `PreparePage`: reads and decompresses a v1 data page or a dictionary page.
    fn prepare_page_v1(&mut self, header: &PageHeader) -> Result<()> {
        let uncompressed = header.uncompressed_page_size as usize;
        let compressed = self.payload_size(header);
        let codec = self.codec;
        let mut block = self.take_block(uncompressed + 1);
        if codec == CompressionCodec::UNCOMPRESSED {
            if compressed != uncompressed {
                return Err(runtime_error("Page size mismatch"));
            }
            let bytes = self.read_page_payload(header)?;
            block[..compressed].copy_from_slice(&bytes);
        } else {
            let file_name = Arc::clone(&self.context.file_name);
            let bytes = self.read_page_payload(header)?;
            decompress(codec, &bytes, &mut block[..uncompressed], &file_name)?;
        }
        self.set_block(block);
        Ok(())
    }

    /// `PreparePageV2`: levels are stored uncompressed in front of the values.
    fn prepare_page_v2(&mut self, header: &PageHeader) -> Result<()> {
        let v2 = header.data_page_header_v2.clone().unwrap_or_default();
        let uncompressed_size = header.uncompressed_page_size as usize;
        let compressed_size = self.payload_size(header);
        let mut block = self.take_block(uncompressed_size + 1);
        let mut is_uncompressed = v2.is_compressed == Some(false);
        if self.codec == CompressionCodec::UNCOMPRESSED {
            if compressed_size != uncompressed_size {
                return Err(read_failure(self.file_name(), "Page size mismatch"));
            }
            is_uncompressed = true;
        }
        if is_uncompressed {
            let bytes = self.read_page_payload(header)?;
            let len = bytes.len().min(uncompressed_size);
            block[..len].copy_from_slice(&bytes[..len]);
            block[len..].fill(0);
            self.set_block(block);
            return Ok(());
        }
        if v2.repetition_levels_byte_length < 0 || v2.definition_levels_byte_length < 0 {
            return Err(read_failure(
                self.file_name(),
                "header inconsistency, repetition_levels_byte_length and definition_levels_byte_length must be >= 0",
            ));
        }
        let level_bytes =
            v2.repetition_levels_byte_length as usize + v2.definition_levels_byte_length as usize;
        if level_bytes > uncompressed_size {
            return Err(read_failure(
                self.file_name(),
                "header inconsistency, uncompressed_page_size needs to be larger than repetition_levels_byte_length + definition_levels_byte_length",
            ));
        }
        if compressed_size < level_bytes {
            return Err(read_failure(
                self.file_name(),
                "header inconsistency, compressed_page_size is smaller than repetition_levels_byte_length + definition_levels_byte_length",
            ));
        }
        let file_name = Arc::clone(&self.context.file_name);
        let codec = self.codec;
        let payload = self.read_page_payload(header)?;
        block[..level_bytes].copy_from_slice(&payload[..level_bytes]);
        let compressed_bytes = compressed_size - level_bytes;
        if compressed_bytes == 0 && uncompressed_size > level_bytes {
            return Err(read_failure(
                &file_name,
                "header inconsistency, compressed_page_size is too small for the declared value region",
            ));
        }
        if compressed_bytes > 0 {
            decompress(
                codec,
                &payload[level_bytes..compressed_size],
                &mut block[level_bytes..uncompressed_size],
                &file_name,
            )?;
        }
        drop(payload);
        self.set_block(block);
        Ok(())
    }

    /// A buffer of `len` bytes for the next page, reusing the current page's when no
    /// vector references it any more (DuckDB's `ResizeableBuffer`). Its content is
    /// unspecified, except for the last byte (the padding), which is zero.
    fn take_block(&mut self, len: usize) -> Vec<u8> {
        let mut block = Arc::try_unwrap(std::mem::take(&mut self.block)).unwrap_or_default();
        self.block_position = 0;
        self.block_end = 0;
        resize_uninitialized(&mut block, len);
        if let Some(last) = block.last_mut() {
            *last = 0;
        }
        block
    }

    fn set_block(&mut self, block: Vec<u8>) {
        self.block_end = block.len();
        self.block = Arc::new(block);
        self.block_position = 0;
    }

    fn cursor(&self, start: usize, len: usize) -> BlockCursor {
        BlockCursor::new(Arc::clone(&self.block), start..start + len)
    }

    /// Reads a little-endian `u32` at the block position (`block->read<uint32_t>()`).
    fn read_block_u32(&mut self) -> Result<u32> {
        let mut buffer = ByteBuffer::new(&self.block[self.block_position..self.block_end]);
        let value = buffer.read_u32()?;
        self.block_position += 4;
        Ok(value)
    }

    /// `PrepareDataPage`: sets up the level decoders and the value decoding.
    fn prepare_data_page(&mut self, header: &PageHeader) -> Result<()> {
        let is_v1 = header.type_ == PageType::DATA_PAGE;
        if is_v1 && header.data_page_header.is_none() {
            return Err(read_failure(
                self.file_name(),
                "Missing data page header from data page",
            ));
        }
        if !is_v1 && header.data_page_header_v2.is_none() {
            return Err(read_failure(
                self.file_name(),
                "Missing data page header from data page v2",
            ));
        }
        let (num_values, encoding, v2_lengths) = if is_v1 {
            let v1 = header
                .data_page_header
                .as_ref()
                .map_or_else(Default::default, Clone::clone);
            (v1.num_values, v1.encoding, None)
        } else {
            let v2 = header
                .data_page_header_v2
                .as_ref()
                .map_or_else(Default::default, Clone::clone);
            (
                v2.num_values,
                v2.encoding,
                Some((v2.repetition_levels_byte_length, v2.definition_levels_byte_length)),
            )
        };
        self.page_rows_available = num_values.max(0) as u64;
        if self.has_repeats() {
            let len = match v2_lengths {
                None => self.read_block_u32()? as usize,
                Some((repetition, _)) => repetition.max(0) as usize,
            };
            ByteBuffer::new(&self.block[self.block_position..self.block_end]).available(len)?;
            let cursor = self.cursor(self.block_position, len);
            self.repeats = Some(RleBpDecoder::new(
                cursor,
                compute_bit_width(u64::from(self.max_repeat)),
            )?);
            self.block_position += len;
        } else if let Some((repetition, _)) = v2_lengths
            && repetition > 0
        {
            self.skip_block(repetition as usize)?;
        }
        if self.has_defines() {
            let len = match v2_lengths {
                None => self.read_block_u32()? as usize,
                Some((_, definition)) => definition.max(0) as usize,
            };
            ByteBuffer::new(&self.block[self.block_position..self.block_end]).available(len)?;
            let cursor = self.cursor(self.block_position, len);
            self.defines = Some(RleBpDecoder::new(
                cursor,
                compute_bit_width(u64::from(self.max_define)),
            )?);
            self.block_position += len;
        } else if let Some((_, definition)) = v2_lengths
            && definition > 0
        {
            self.skip_block(definition as usize)?;
        }
        self.decoding = match encoding {
            Encoding::RLE_DICTIONARY | Encoding::PLAIN_DICTIONARY => {
                let mut buffer = ByteBuffer::new(&self.block[self.block_position..self.block_end]);
                let width = buffer.read_u8()?;
                let cursor = self.cursor(self.block_position + 1, self.block_end - self.block_position - 1);
                self.block_position = self.block_end;
                PageDecoding::Dictionary(RleBpDecoder::new(cursor, width)?)
            }
            Encoding::RLE => {
                if self.logical_type.id() != rustdb_types::LogicalTypeId::Boolean {
                    return Err(runtime_error("RLE encoding is only supported for boolean data"));
                }
                self.skip_block(4)?;
                let cursor = self.cursor(self.block_position, self.block_end - self.block_position);
                PageDecoding::Rle(RleBpDecoder::new(cursor, 1)?)
            }
            Encoding::DELTA_BINARY_PACKED => {
                let cursor = self.cursor(self.block_position, self.block_end - self.block_position);
                self.block_position = self.block_end;
                PageDecoding::DeltaBinaryPacked(Box::new(DbpDecoder::new(cursor)?))
            }
            Encoding::DELTA_LENGTH_BYTE_ARRAY => self.initialize_delta_length_byte_array()?,
            Encoding::DELTA_BYTE_ARRAY => self.initialize_delta_byte_array()?,
            Encoding::BYTE_STREAM_SPLIT => {
                // The padding byte is not part of the streams.
                let cursor = self.cursor(self.block_position, self.block_end - self.block_position - 1);
                self.block_position = self.block_end;
                PageDecoding::ByteStreamSplit(BssDecoder::new(cursor))
            }
            Encoding::PLAIN => PageDecoding::Plain,
            _ => return Err(read_failure(self.file_name(), "Unsupported page encoding")),
        };
        Ok(())
    }

    fn skip_block(&mut self, len: usize) -> Result<()> {
        ByteBuffer::new(&self.block[self.block_position..self.block_end]).available(len)?;
        self.block_position += len;
        Ok(())
    }

    /// `DeltaByteArrayDecoder::ReadDbpData`: a `DELTA_BINARY_PACKED` run of `u32`s at the
    /// block position.
    fn read_dbp_lengths(&mut self) -> Result<Vec<u32>> {
        let cursor = self.cursor(self.block_position, self.block_end - self.block_position);
        let mut decoder = DbpDecoder::new(cursor)?;
        let count = usize::try_from(decoder.total_values()).map_err(|_| runtime_error("Out of buffer"))?;
        // Every value takes at least one bit; refuse absurd counts before allocating.
        if count > (self.block_end - self.block_position).saturating_mul(64) + 1 {
            return Err(runtime_error("DBP decode did not find enough values"));
        }
        let mut values = vec![0u32; count];
        decoder.get_batch(&mut values)?;
        decoder.finalize()?;
        self.block_position = decoder.position();
        Ok(values)
    }

    /// `DeltaLengthByteArrayDecoder::InitializePage`.
    fn initialize_delta_length_byte_array(&mut self) -> Result<PageDecoding> {
        if self.logical_type.physical_type() != rustdb_types::PhysicalType::Varchar {
            return Err(runtime_error(
                "Delta Length Byte Array encoding is only supported for string/blob data",
            ));
        }
        let lengths = self.read_dbp_lengths()?;
        let total: u64 = lengths.iter().map(|&len| u64::from(len)).sum();
        if total > (self.block_end - self.block_position) as u64 {
            return Err(runtime_error("Out of buffer"));
        }
        Ok(PageDecoding::DeltaLengthByteArray { lengths, next: 0 })
    }

    /// `DeltaByteArrayDecoder::InitializePage`: rebuilds the values as plain values.
    fn initialize_delta_byte_array(&mut self) -> Result<PageDecoding> {
        let prefixes = self.read_dbp_lengths()?;
        let suffixes = self.read_dbp_lengths()?;
        if prefixes.len() != suffixes.len() {
            return Err(runtime_error(
                "DELTA_BYTE_ARRAY - prefix and suffix counts are different - corrupt file?",
            ));
        }
        let is_fixed = self.parquet_type == Type::FIXED_LEN_BYTE_ARRAY;
        let fixed_len = if is_fixed { self.type_length } else { 0 };
        let mut total = 0usize;
        for (&prefix, &suffix) in prefixes.iter().zip(&suffixes) {
            let len = prefix as usize + suffix as usize;
            if is_fixed && len != fixed_len {
                return Err(runtime_error(
                    "DELTA_BYTE_ARRAY on FIXED_LEN_BYTE_ARRAY: decoded length does not match type length",
                ));
            }
            total += len;
        }
        if !is_fixed {
            total += prefixes.len() * 4;
        }
        let mut output = Vec::with_capacity(total);
        let mut previous: Vec<u8> = Vec::new();
        let data = &self.block[..self.block_end];
        let mut position = self.block_position;
        for (&prefix, &suffix) in prefixes.iter().zip(&suffixes) {
            let (prefix, suffix) = (prefix as usize, suffix as usize);
            if prefix > previous.len() {
                return Err(runtime_error(
                    "DELTA_BYTE_ARRAY - prefix is out of range - corrupt file?",
                ));
            }
            if !is_fixed {
                output.extend_from_slice(&((prefix + suffix) as u32).to_le_bytes());
            }
            let suffix_bytes = data
                .get(position..position + suffix)
                .ok_or_else(|| runtime_error("Out of buffer"))?;
            position += suffix;
            previous.truncate(prefix);
            previous.extend_from_slice(suffix_bytes);
            output.extend_from_slice(&previous);
        }
        self.block_position = position;
        Ok(PageDecoding::DeltaByteArray {
            block: Arc::new(output),
            position: 0,
        })
    }

    /// `DictionaryDecoder::InitializeDictionary`.
    fn initialize_dictionary(&mut self, size: usize) -> Result<()> {
        let has_nulls = self.has_defines();
        let capacity = size + usize::from(has_nulls);
        let mut dictionary = Vector::flat(self.logical_type.clone(), capacity.max(1));
        if has_nulls {
            dictionary.validity_mut().set_invalid(size, capacity);
        }
        let block = Arc::clone(&self.block);
        let mut source = PlainSource::new(&block[self.block_position..self.block_end], Some(&block));
        self.decoder.reset_page();
        self.decoder
            .plain(&mut source, None, 0, size, 0, &mut dictionary)?;
        self.dictionary = Some(dictionary);
        self.dictionary_size = size;
        self.dictionary_filters.clear();
        Ok(())
    }

    /// Which entries of the current dictionary pass `filter`, the scan's filter number
    /// `filter_index` (`DictionaryDecoder::Filter`): a dictionary-encoded read is then
    /// filtered by looking up its dictionary indexes instead of comparing every value.
    pub fn dictionary_filter(
        &mut self,
        filter_index: usize,
        filter: &TableFilter,
    ) -> Result<Option<&[bool]>> {
        let Some(dictionary) = &self.dictionary else {
            return Ok(None);
        };
        if self.dictionary_filters.len() <= filter_index {
            self.dictionary_filters.resize(filter_index + 1, None);
        }
        let slot = &mut self.dictionary_filters[filter_index];
        if slot.is_none() {
            let entries = dictionary.capacity();
            let mut passing: Vec<u32> = (0..entries as u32).collect();
            filter.select(dictionary, entries, &mut passing)?;
            let mut mask = vec![false; entries];
            for entry in passing {
                mask[entry as usize] = true;
            }
            *slot = Some(mask);
        }
        Ok(slot.as_deref())
    }

    /// `ReadPageHeaders`: makes sure a page with values is ready; returns how many of
    /// `max_read` values to read from it.
    fn read_page_headers(&mut self, max_read: u64) -> Result<usize> {
        // The AAD page ordinal counts from 0 in every call, like DuckDB's.
        let mut page_ordinal = 0;
        while self.page_rows_available == 0 {
            self.crypto.page_ordinal = page_ordinal;
            self.prepare_page()?;
            page_ordinal += 1;
        }
        Ok(max_read
            .min(self.page_rows_available)
            .min(STANDARD_VECTOR_SIZE as u64) as usize)
    }

    /// `PrepareRead`: decodes the levels of `count` values at `offset`; returns whether all
    /// of them are valid (without writing the definition levels).
    fn read_levels(
        &mut self,
        count: usize,
        defines: &mut [u8],
        repeats: &mut [u8],
        offset: usize,
    ) -> Result<bool> {
        if self.has_repeats() {
            let decoder = self
                .repeats
                .as_mut()
                .ok_or_else(|| Error::internal("missing repetition level decoder"))?;
            decoder.get_batch(&mut repeats[offset..offset + count])?;
        }
        if self.has_defines() {
            let has_repeats = self.has_repeats();
            let max_define = self.max_define;
            let decoder = self
                .defines
                .as_mut()
                .ok_or_else(|| Error::internal("missing definition level decoder"))?;
            if !has_repeats && decoder.has_repeated_batch(count as u32, u64::from(max_define))? {
                decoder.skip_repeated_batch(count as u32);
                return Ok(true);
            }
            decoder.get_batch(&mut defines[offset..offset + count])?;
            return Ok(false);
        }
        Ok(true)
    }

    /// Applies skips registered with [`Self::skip`] (`ApplyPendingSkips`).
    fn apply_pending_skips(&mut self) -> Result<()> {
        if self.pending_skips == 0 {
            return Ok(());
        }
        let total = self.pending_skips;
        self.pending_skips = 0;
        let mut skip_defines = vec![0u8; STANDARD_VECTOR_SIZE];
        let mut skip_repeats = vec![0u8; STANDARD_VECTOR_SIZE];
        let mut to_skip = total;
        while to_skip > 0 {
            let skip_now = self.read_page_headers(to_skip)?;
            let all_valid = self.read_levels(skip_now, &mut skip_defines, &mut skip_repeats, 0)?;
            let defines = (!all_valid).then_some(&skip_defines[..]);
            let count = valid_count(defines, self.max_define, skip_now);
            match &mut self.decoding {
                PageDecoding::Dictionary(decoder) => {
                    if self.dictionary.is_none() {
                        return Err(runtime_error(
                            "Parquet file is likely corrupted, missing dictionary",
                        ));
                    }
                    decoder.skip(count)?;
                }
                PageDecoding::DeltaBinaryPacked(decoder) => match self.parquet_type {
                    Type::INT32 => decoder.skip::<i32>(count)?,
                    Type::INT64 => decoder.skip::<i64>(count)?,
                    _ => return Err(runtime_error("DELTA_BINARY_PACKED should only be INT32 or INT64")),
                },
                PageDecoding::Rle(decoder) => decoder.skip(count)?,
                PageDecoding::DeltaLengthByteArray { lengths, next } => {
                    if *next + count > lengths.len() {
                        return Err(dlba_mismatch(*next + count, lengths.len()));
                    }
                    let bytes: usize = lengths[*next..*next + count]
                        .iter()
                        .map(|&len| len as usize)
                        .sum();
                    *next += count;
                    ByteBuffer::new(&self.block[self.block_position..self.block_end]).available(bytes)?;
                    self.block_position += bytes;
                }
                PageDecoding::DeltaByteArray { block, position } => {
                    let mut source = PlainSource::new(&block[*position..], Some(block));
                    let before = source.buffer.len();
                    self.decoder
                        .skip(&mut source, defines, self.max_define, skip_now)?;
                    *position += before - source.buffer.len();
                }
                PageDecoding::ByteStreamSplit(decoder) => {
                    let width = bss_width(self.parquet_type)?;
                    decoder.skip(width, count)?;
                }
                PageDecoding::Plain | PageDecoding::Invalid => {
                    let block = Arc::clone(&self.block);
                    let mut source =
                        PlainSource::new(&block[self.block_position..self.block_end], Some(&block));
                    let before = source.buffer.len();
                    self.decoder
                        .skip(&mut source, defines, self.max_define, skip_now)?;
                    self.block_position += before - source.buffer.len();
                }
            }
            self.page_rows_available -= skip_now as u64;
            to_skip -= skip_now as u64;
        }
        self.group_rows_available = self.group_rows_available.saturating_sub(total);
        Ok(())
    }

    /// `ColumnReader::Skip`.
    pub fn skip(&mut self, num_values: usize) {
        self.pending_skips += num_values as u64;
    }

    /// `ColumnReader::Read`: reads `num_values` values into `result`.
    pub fn read(
        &mut self,
        num_values: usize,
        defines: &mut [u8],
        repeats: &mut [u8],
        result: &mut Vector,
    ) -> Result<usize> {
        self.apply_pending_skips()?;
        let mut offset = 0;
        let mut to_read = num_values as u64;
        while to_read > 0 {
            let read_now = self.read_page_headers(to_read)?;
            self.read_data(read_now, defines, repeats, result, offset)?;
            offset += read_now;
            to_read -= read_now as u64;
        }
        self.group_rows_available = self.group_rows_available.saturating_sub(num_values as u64);
        Ok(num_values)
    }

    /// `ReadData`: decodes `count` values of the current page at row `offset`.
    fn read_data(
        &mut self,
        count: usize,
        defines: &mut [u8],
        repeats: &mut [u8],
        result: &mut Vector,
        offset: usize,
    ) -> Result<()> {
        if offset != 0 && result.kind() != VectorKind::Flat {
            let mut flat = Vector::flat(self.logical_type.clone(), STANDARD_VECTOR_SIZE);
            copy_rows(result, 0, offset, &mut flat, 0)?;
            *result = flat;
        }
        let all_valid = self.read_levels(count, defines, repeats, offset)?;
        let defines: Option<&[u8]> = (!all_valid).then_some(&*defines);
        let max_define = self.max_define;
        match &mut self.decoding {
            PageDecoding::Dictionary(_) => self.read_dictionary(defines, count, result, offset)?,
            PageDecoding::DeltaBinaryPacked(decoder) => {
                let valid = valid_defines(defines, max_define, offset, count);
                match self.parquet_type {
                    Type::INT32 => {
                        let mut values = vec![0i32; valid];
                        decoder.get_batch(&mut values)?;
                        self.scratch.clear();
                        self.scratch
                            .extend(values.iter().flat_map(|value| value.to_le_bytes()));
                    }
                    Type::INT64 => {
                        let mut values = vec![0i64; valid];
                        decoder.get_batch(&mut values)?;
                        self.scratch.clear();
                        self.scratch
                            .extend(values.iter().flat_map(|value| value.to_le_bytes()));
                    }
                    _ => return Err(runtime_error("DELTA_BINARY_PACKED should only be INT32 or INT64")),
                }
                let mut source = PlainSource::new(&self.scratch, None);
                self.decoder
                    .plain(&mut source, defines, max_define, count, offset, result)?;
            }
            PageDecoding::Rle(decoder) => {
                let valid = valid_defines(defines, max_define, offset, count);
                self.scratch.clear();
                self.scratch.resize(valid, 0);
                decoder.get_batch(&mut self.scratch[..])?;
                let mut source = PlainSource::new(&self.scratch, None);
                ByteBooleanDecoder.plain(&mut source, defines, max_define, count, offset, result)?;
            }
            PageDecoding::DeltaLengthByteArray { lengths, next } => {
                let strings = self
                    .strings
                    .as_ref()
                    .ok_or_else(|| Error::internal("DELTA_LENGTH_BYTE_ARRAY on a non-string column"))?;
                read_delta_length_byte_array(
                    &self.block,
                    &mut self.block_position,
                    self.block_end,
                    lengths,
                    next,
                    strings,
                    defines,
                    max_define,
                    count,
                    offset,
                    result,
                )?;
            }
            PageDecoding::DeltaByteArray { block, position } => {
                let mut source = PlainSource::new(&block[*position..], Some(block));
                let before = source.buffer.len();
                self.decoder
                    .plain(&mut source, defines, max_define, count, offset, result)?;
                *position += before - source.buffer.len();
            }
            PageDecoding::ByteStreamSplit(decoder) => {
                let valid = valid_defines(defines, max_define, offset, count);
                let width = bss_width(self.parquet_type)?;
                decoder.get_batch(width, valid, &mut self.scratch)?;
                let mut source = PlainSource::new(&self.scratch, None);
                self.decoder
                    .plain(&mut source, defines, max_define, count, offset, result)?;
            }
            PageDecoding::Plain | PageDecoding::Invalid => {
                let block = Arc::clone(&self.block);
                let mut source = PlainSource::new(&block[self.block_position..self.block_end], Some(&block));
                let before = source.buffer.len();
                self.decoder
                    .plain(&mut source, defines, max_define, count, offset, result)?;
                self.block_position += before - source.buffer.len();
            }
        }
        self.page_rows_available -= count as u64;
        Ok(())
    }

    /// `DictionaryDecoder::Read`.
    fn read_dictionary(
        &mut self,
        defines: Option<&[u8]>,
        count: usize,
        result: &mut Vector,
        offset: usize,
    ) -> Result<()> {
        let Some(dictionary) = &self.dictionary else {
            return Err(runtime_error(
                "Parquet file is likely corrupted, missing dictionary",
            ));
        };
        let PageDecoding::Dictionary(decoder) = &mut self.decoding else {
            return Err(Error::internal("dictionary read without a dictionary page"));
        };
        let size = self.dictionary_size;
        self.indices.clear();
        self.indices.resize(count, 0);
        match defines {
            None => {
                decoder.get_batch(&mut self.indices[..])?;
                // A branch-free maximum (vectorized) instead of an early-exit search.
                if self.indices.iter().copied().max().unwrap_or(0) as usize >= size && count > 0 {
                    return Err(runtime_error(
                        "Parquet file is likely corrupted, dictionary offset out of range",
                    ));
                }
            }
            Some(defines) => {
                let rows = &defines[offset..offset + count];
                let valid: usize = rows
                    .iter()
                    .map(|&level| usize::from(level == self.max_define))
                    .sum();
                let mut offsets = vec![0u32; valid];
                decoder.get_batch(&mut offsets)?;
                let mut next = offsets.iter();
                for (row, &level) in rows.iter().enumerate() {
                    if level == self.max_define {
                        let index = *next.next().ok_or_else(|| Error::internal("dictionary offsets"))?;
                        if index as usize >= size {
                            return Err(runtime_error(
                                "Parquet file is likely corrupted, dictionary offset out of range",
                            ));
                        }
                        self.indices[row] = index;
                    } else {
                        self.indices[row] = size as u32;
                    }
                }
            }
        }
        let sliced = dictionary.slice(&SelectionVector::new(std::mem::take(&mut self.indices)));
        if offset == 0 {
            *result = sliced;
        } else {
            copy_rows(&sliced, 0, count, result, offset)?;
        }
        Ok(())
    }
}

/// Sets the length of `buffer` to `len` without clearing the bytes it keeps (only new
/// bytes are zeroed): callers overwrite the content.
fn resize_uninitialized(buffer: &mut Vec<u8>, len: usize) {
    if buffer.len() >= len {
        buffer.truncate(len);
    } else {
        buffer.resize(len, 0);
    }
}

fn dlba_mismatch(attempted: usize, available: usize) -> Error {
    Error::new(
        rustdb_common::ErrorKind::Io,
        format!(
            "DELTA_LENGTH_BYTE_ARRAY - length mismatch between values and byte array lengths (attempted read of {attempted} from {available} entries) - corrupt file?"
        ),
    )
}

/// The number of rows `offset..offset + count` holding a value.
fn valid_defines(defines: Option<&[u8]>, max_define: u8, offset: usize, count: usize) -> usize {
    defines.map_or(count, |defines| {
        defines[offset..offset + count]
            .iter()
            .map(|&level| usize::from(level == max_define))
            .sum()
    })
}

/// The value width of a `BYTE_STREAM_SPLIT` column.
fn bss_width(parquet_type: Type) -> Result<usize> {
    match parquet_type {
        Type::FLOAT => Ok(4),
        Type::DOUBLE => Ok(8),
        _ => Err(runtime_error(
            "BYTE_STREAM_SPLIT encoding is only supported for FLOAT or DOUBLE data",
        )),
    }
}

/// `DeltaLengthByteArrayDecoder::Read`.
#[allow(
    clippy::too_many_arguments,
    reason = "the decoder state is split across the leaf reader"
)]
fn read_delta_length_byte_array(
    block: &PageBlock,
    position: &mut usize,
    end: usize,
    lengths: &[u32],
    next: &mut usize,
    strings: &StringDecoder,
    defines: Option<&[u8]>,
    max_define: u8,
    count: usize,
    offset: usize,
    result: &mut Vector,
) -> Result<()> {
    if defines.is_none() && *next + count > lengths.len() {
        return Err(dlba_mismatch(*next + count, lengths.len()));
    }
    result.add_keep_alive(Arc::clone(block) as Arc<dyn std::any::Any + Send + Sync>);
    let start = *position;
    let validate_each = strings.kind == super::values::StringKind::Json;
    let capacity = result.capacity().max(offset + count);
    for row in offset..offset + count {
        if let Some(defines) = defines {
            if defines[row] != max_define {
                result.validity_mut().set_invalid(row, capacity);
                continue;
            }
            if *next >= lengths.len() {
                return Err(dlba_mismatch(*next, lengths.len()));
            }
        }
        let len = lengths[*next] as usize;
        *next += 1;
        let bytes = block
            .get(*position..*position + len)
            .filter(|_| *position + len <= end)
            .ok_or_else(|| runtime_error("Out of buffer"))?;
        if validate_each {
            strings.verify(bytes)?;
        }
        // SAFETY: the bytes live in `block`, which `result` keeps alive (above).
        result.values_mut::<StringT>()[row] = unsafe { StringT::from_raw(bytes) };
        *position += len;
    }
    if !validate_each {
        strings.verify(&block[start..*position])?;
    }
    Ok(())
}
