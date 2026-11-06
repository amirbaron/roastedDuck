//! Frames (`zstd_decompress.c`): one-shot decompression of whole frames into a buffer
//! (`ZSTD_decompressDCtx`) and streaming decompression with buffered output
//! (`ZSTD_decompressStream`).

use super::block::{BlockDecoder, WILDCOPY_OVERLENGTH, Window};
use super::xxhash::Xxh64;
use crate::error::{Error, Result};
use crate::params::{BLOCK_SIZE_MAX, CONTENT_SIZE_UNKNOWN, WINDOW_LOG_ABSOLUTE_MIN};

const MAGIC_NUMBER: u32 = 0xFD2F_B528;
const MAGIC_SKIPPABLE_START: u32 = 0x184D_2A50;
const MAGIC_SKIPPABLE_MASK: u32 = 0xFFFF_FFF0;
const FRAME_ID_SIZE: usize = 4;
const SKIPPABLE_HEADER_SIZE: usize = 8;
/// `ZSTD_FRAMEHEADERSIZE_PREFIX`: the bytes that give the header's size.
const FRAME_HEADER_SIZE_PREFIX: usize = 5;
/// `ZSTD_FRAMEHEADERSIZE_MIN`.
const FRAME_HEADER_SIZE_MIN: usize = 6;
/// `ZSTD_FRAMEHEADERSIZE_MAX`.
const FRAME_HEADER_SIZE_MAX: usize = 18;
const BLOCK_HEADER_SIZE: usize = 3;
/// `ZSTD_WINDOWLOG_MAX` (64-bit).
const WINDOW_LOG_MAX: u32 = 31;
/// `ZSTD_MAXWINDOWSIZE_DEFAULT`: the largest window a stream decodes.
const MAX_WINDOW_SIZE: u64 = (1 << 27) + 1;
/// `ZSTD_NO_FORWARD_PROGRESS_MAX`.
const NO_FORWARD_PROGRESS_MAX: u32 = 16;
/// `ZSTD_did_fieldSize`, `ZSTD_fcs_fieldSize`.
const DID_FIELD_SIZE: [usize; 4] = [0, 1, 2, 4];
const FCS_FIELD_SIZE: [usize; 4] = [0, 2, 4, 8];

const BT_RAW: u32 = 0;
const BT_RLE: u32 = 1;
const BT_COMPRESSED: u32 = 2;

#[inline(always)]
fn read32(src: &[u8], pos: usize) -> u32 {
    u32::from_le_bytes([src[pos], src[pos + 1], src[pos + 2], src[pos + 3]])
}

/// `ZSTD_frameType_e`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum FrameType {
    #[default]
    Frame,
    Skippable,
}

/// `ZSTD_frameHeader`.
#[derive(Clone, Copy, Debug, Default)]
struct FrameHeader {
    /// `CONTENT_SIZE_UNKNOWN` when absent; the content size of a skippable frame.
    frame_content_size: u64,
    window_size: u64,
    block_size_max: usize,
    frame_type: FrameType,
    header_size: usize,
    dict_id: u32,
    checksum_flag: bool,
}

/// `ZSTD_getFrameHeader`'s result.
enum HeaderRead {
    Header(FrameHeader),
    /// More input is needed: the header is at least this long.
    Need(usize),
}

/// `ZSTD_frameHeaderSize_internal`.
fn frame_header_size(src: &[u8]) -> Result<usize> {
    if src.len() < FRAME_HEADER_SIZE_PREFIX {
        return Err(Error::SrcSizeWrong);
    }
    let fhd = src[FRAME_HEADER_SIZE_PREFIX - 1];
    let single_segment = (fhd >> 5) & 1 == 1;
    let fcs_id = usize::from(fhd >> 6);
    Ok(FRAME_HEADER_SIZE_PREFIX
        + usize::from(!single_segment)
        + DID_FIELD_SIZE[usize::from(fhd & 3)]
        + FCS_FIELD_SIZE[fcs_id]
        + usize::from(single_segment && fcs_id == 0))
}

/// `ZSTD_getFrameHeader_advanced` for `ZSTD_f_zstd1`.
fn get_frame_header(src: &[u8]) -> Result<HeaderRead> {
    if src.len() < FRAME_HEADER_SIZE_PREFIX {
        if !src.is_empty() {
            // Fail early on bytes that start no frame.
            let toload = src.len().min(4);
            let mut hbuf = MAGIC_NUMBER.to_le_bytes();
            hbuf[..toload].copy_from_slice(&src[..toload]);
            if u32::from_le_bytes(hbuf) != MAGIC_NUMBER {
                let mut hbuf = MAGIC_SKIPPABLE_START.to_le_bytes();
                hbuf[..toload].copy_from_slice(&src[..toload]);
                if u32::from_le_bytes(hbuf) & MAGIC_SKIPPABLE_MASK != MAGIC_SKIPPABLE_START {
                    return Err(Error::PrefixUnknown);
                }
            }
        }
        return Ok(HeaderRead::Need(FRAME_HEADER_SIZE_PREFIX));
    }
    let magic = read32(src, 0);
    if magic != MAGIC_NUMBER {
        if magic & MAGIC_SKIPPABLE_MASK == MAGIC_SKIPPABLE_START {
            if src.len() < SKIPPABLE_HEADER_SIZE {
                return Ok(HeaderRead::Need(SKIPPABLE_HEADER_SIZE));
            }
            return Ok(HeaderRead::Header(FrameHeader {
                frame_content_size: u64::from(read32(src, FRAME_ID_SIZE)),
                frame_type: FrameType::Skippable,
                ..FrameHeader::default()
            }));
        }
        return Err(Error::PrefixUnknown);
    }
    let fh_size = frame_header_size(src)?;
    if src.len() < fh_size {
        return Ok(HeaderRead::Need(fh_size));
    }
    let fhd = src[FRAME_HEADER_SIZE_PREFIX - 1];
    let mut pos = FRAME_HEADER_SIZE_PREFIX;
    let checksum_flag = (fhd >> 2) & 1 == 1;
    let single_segment = (fhd >> 5) & 1 == 1;
    if fhd & 0x08 != 0 {
        return Err(Error::FrameParameterUnsupported);
    }
    let mut window_size = 0u64;
    if !single_segment {
        let wl_byte = src[pos];
        pos += 1;
        let window_log = u32::from(wl_byte >> 3) + WINDOW_LOG_ABSOLUTE_MIN;
        if window_log > WINDOW_LOG_MAX {
            return Err(Error::FrameParameterWindowTooLarge);
        }
        window_size = 1u64 << window_log;
        window_size += (window_size >> 3) * u64::from(wl_byte & 7);
    }
    let dict_id = match fhd & 3 {
        0 => 0,
        1 => u32::from(src[pos]),
        2 => u32::from(u16::from_le_bytes([src[pos], src[pos + 1]])),
        _ => read32(src, pos),
    };
    pos += DID_FIELD_SIZE[usize::from(fhd & 3)];
    let frame_content_size = match fhd >> 6 {
        0 if single_segment => u64::from(src[pos]),
        0 => CONTENT_SIZE_UNKNOWN,
        1 => u64::from(u16::from_le_bytes([src[pos], src[pos + 1]])) + 256,
        2 => u64::from(read32(src, pos)),
        _ => u64::from_le_bytes(src[pos..pos + 8].try_into().unwrap_or_default()),
    };
    if single_segment {
        window_size = frame_content_size;
    }
    Ok(HeaderRead::Header(FrameHeader {
        frame_content_size,
        window_size,
        block_size_max: window_size.min(BLOCK_SIZE_MAX as u64) as usize,
        frame_type: FrameType::Frame,
        header_size: fh_size,
        dict_id,
        checksum_flag,
    }))
}

/// `blockProperties_t`.
#[derive(Clone, Copy, Debug)]
struct BlockProperties {
    block_type: u32,
    last_block: bool,
    orig_size: usize,
}

/// `ZSTD_getcBlockSize`: the block header at the start of `src`; returns the size of the
/// block's content (1 for RLE blocks).
fn get_c_block_size(src: &[u8]) -> Result<(usize, BlockProperties)> {
    if src.len() < BLOCK_HEADER_SIZE {
        return Err(Error::SrcSizeWrong);
    }
    let header = u32::from(src[0]) | u32::from(src[1]) << 8 | u32::from(src[2]) << 16;
    let c_size = (header >> 3) as usize;
    let properties = BlockProperties {
        block_type: (header >> 1) & 3,
        last_block: header & 1 == 1,
        orig_size: c_size,
    };
    match properties.block_type {
        BT_RLE => Ok((1, properties)),
        3 => Err(Error::CorruptionDetected),
        _ => Ok((c_size, properties)),
    }
}

/// `readSkippableFrameSize`.
fn skippable_frame_size(src: &[u8]) -> Result<usize> {
    if src.len() < SKIPPABLE_HEADER_SIZE {
        return Err(Error::SrcSizeWrong);
    }
    let size = read32(src, FRAME_ID_SIZE);
    if size.checked_add(SKIPPABLE_HEADER_SIZE as u32).is_none() {
        return Err(Error::FrameParameterUnsupported);
    }
    let skippable_size = SKIPPABLE_HEADER_SIZE + size as usize;
    if skippable_size > src.len() {
        return Err(Error::SrcSizeWrong);
    }
    Ok(skippable_size)
}

/// `ZSTD_findFrameCompressedSize`: the size of the frame at the start of `src`.
pub fn find_frame_compressed_size(src: &[u8]) -> Result<usize> {
    if src.len() >= SKIPPABLE_HEADER_SIZE && read32(src, 0) & MAGIC_SKIPPABLE_MASK == MAGIC_SKIPPABLE_START {
        return skippable_frame_size(src);
    }
    let HeaderRead::Header(header) = get_frame_header(src)? else {
        return Err(Error::SrcSizeWrong);
    };
    let mut ip = header.header_size;
    loop {
        let (c_block_size, properties) = get_c_block_size(&src[ip..])?;
        if BLOCK_HEADER_SIZE + c_block_size > src.len() - ip {
            return Err(Error::SrcSizeWrong);
        }
        ip += BLOCK_HEADER_SIZE + c_block_size;
        if properties.last_block {
            break;
        }
    }
    if header.checksum_flag {
        if src.len() - ip < 4 {
            return Err(Error::SrcSizeWrong);
        }
        ip += 4;
    }
    Ok(ip)
}

/// `ZSTD_getFrameContentSize`: the content size the frame at the start of `src` declares,
/// if any.
pub fn frame_content_size(src: &[u8]) -> Result<Option<u64>> {
    match get_frame_header(src)? {
        HeaderRead::Header(header) if header.frame_type == FrameType::Skippable => Ok(Some(0)),
        HeaderRead::Header(header) if header.frame_content_size != CONTENT_SIZE_UNKNOWN => {
            Ok(Some(header.frame_content_size))
        }
        HeaderRead::Header(_) => Ok(None),
        HeaderRead::Need(_) => Err(Error::SrcSizeWrong),
    }
}

/// `ZSTD_dStage`: what `decompress_continue` expects next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    GetFrameHeaderSize,
    DecodeFrameHeader,
    DecodeBlockHeader,
    DecompressBlock,
    DecompressLastBlock,
    CheckChecksum,
    DecodeSkippableHeader,
    SkipFrame,
}

/// `ZSTD_dStreamStage`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StreamStage {
    Init,
    LoadHeader,
    Read,
    Load,
    Flush,
}

/// A decompression context (`ZSTD_DCtx`), reusable for many frames: whole frames into a
/// buffer with [`Decompressor::decompress`], or streams of any split with
/// [`Decompressor::decompress_stream`].
#[derive(Debug)]
pub struct Decompressor {
    block: BlockDecoder,
    fparams: FrameHeader,
    validate_checksum: bool,
    xxh: Xxh64,
    // `ZSTD_decompressContinue`.
    stage: Stage,
    expected: usize,
    header_buffer: [u8; FRAME_HEADER_SIZE_MAX],
    header_size: usize,
    block_type: u32,
    rle_size: usize,
    decoded_size: u64,
    previous_dst_end: Option<usize>,
    window: Window,
    // `ZSTD_decompressStream`.
    stream_stage: StreamStage,
    lh_size: usize,
    in_buff: Vec<u8>,
    in_pos: usize,
    out_buff: Vec<u8>,
    out_buff_size: usize,
    out_start: usize,
    out_end: usize,
    hostage_byte: bool,
    no_forward_progress: u32,
}

impl Default for Decompressor {
    fn default() -> Self {
        Self::new()
    }
}

impl Decompressor {
    /// `ZSTD_createDCtx`.
    pub fn new() -> Self {
        Self {
            block: BlockDecoder::default(),
            fparams: FrameHeader::default(),
            validate_checksum: false,
            xxh: Xxh64::new(),
            stage: Stage::GetFrameHeaderSize,
            expected: FRAME_HEADER_SIZE_PREFIX,
            header_buffer: [0; FRAME_HEADER_SIZE_MAX],
            header_size: 0,
            block_type: 3,
            rle_size: 0,
            decoded_size: 0,
            previous_dst_end: None,
            window: Window::default(),
            stream_stage: StreamStage::Init,
            lh_size: 0,
            in_buff: Vec::new(),
            in_pos: 0,
            out_buff: Vec::new(),
            out_buff_size: 0,
            out_start: 0,
            out_end: 0,
            hostage_byte: false,
            no_forward_progress: 0,
        }
    }

    /// Abandons the current stream (`ZSTD_DCtx_reset(ZSTD_reset_session_only)`).
    pub fn reset(&mut self) {
        self.stream_stage = StreamStage::Init;
        self.no_forward_progress = 0;
    }

    /// `ZSTD_decompressBegin`.
    fn begin(&mut self) {
        self.expected = FRAME_HEADER_SIZE_PREFIX;
        self.stage = Stage::GetFrameHeaderSize;
        self.decoded_size = 0;
        self.previous_dst_end = None;
        self.window = Window::default();
        self.block_type = 3;
        self.block.begin();
    }

    /// `ZSTD_checkContinuity`: output not following the previous block's starts a new
    /// segment; the previous one becomes the extDict.
    fn check_continuity(&mut self, dst: usize, dst_size: usize) {
        if Some(dst) != self.previous_dst_end && dst_size > 0 {
            match self.previous_dst_end {
                Some(previous_end) => {
                    self.window = Window {
                        prefix_start: dst,
                        virtual_start: dst as isize - (previous_end - self.window.prefix_start) as isize,
                        dict_end: previous_end,
                    };
                }
                None => {
                    self.window = Window {
                        prefix_start: dst,
                        virtual_start: dst as isize,
                        dict_end: dst,
                    };
                }
            }
            self.previous_dst_end = Some(dst);
        }
    }

    /// `ZSTD_decodeFrameHeader`.
    fn decode_frame_header(&mut self, header: &[u8]) -> Result<()> {
        let HeaderRead::Header(fparams) = get_frame_header(header)? else {
            return Err(Error::SrcSizeWrong);
        };
        self.fparams = fparams;
        if fparams.dict_id != 0 {
            return Err(Error::DictionaryWrong);
        }
        self.validate_checksum = fparams.checksum_flag;
        if self.validate_checksum {
            self.xxh = Xxh64::new();
        }
        Ok(())
    }

    /// `ZSTD_decompressFrame`: decodes the frame at the start of `*src` into `dst` from
    /// `op_start`, advancing `*src` past it.
    fn decompress_frame(&mut self, dst: &mut [u8], op_start: usize, src: &mut &[u8]) -> Result<usize> {
        let input = *src;
        if input.len() < FRAME_HEADER_SIZE_MIN + BLOCK_HEADER_SIZE {
            return Err(Error::SrcSizeWrong);
        }
        let frame_header_size = frame_header_size(&input[..FRAME_HEADER_SIZE_PREFIX])?;
        if input.len() < frame_header_size + BLOCK_HEADER_SIZE {
            return Err(Error::SrcSizeWrong);
        }
        self.decode_frame_header(&input[..frame_header_size])?;
        let mut ip = frame_header_size;
        let mut op = op_start;
        let oend = dst.len();
        loop {
            let (c_block_size, properties) = get_c_block_size(&input[ip..])?;
            ip += BLOCK_HEADER_SIZE;
            if c_block_size > input.len() - ip {
                return Err(Error::SrcSizeWrong);
            }
            let block = &input[ip..ip + c_block_size];
            let decoded = match properties.block_type {
                BT_COMPRESSED => self.block.decompress_block(
                    block,
                    dst,
                    op,
                    oend,
                    self.window,
                    self.fparams.block_size_max,
                )?,
                BT_RAW => {
                    if c_block_size > oend - op {
                        return Err(Error::DstSizeTooSmall);
                    }
                    dst[op..op + c_block_size].copy_from_slice(block);
                    c_block_size
                }
                _ => {
                    let size = properties.orig_size;
                    if size > oend - op {
                        return Err(Error::DstSizeTooSmall);
                    }
                    dst[op..op + size].fill(block[0]);
                    size
                }
            };
            if self.validate_checksum {
                self.xxh.update(&dst[op..op + decoded]);
            }
            op += decoded;
            ip += c_block_size;
            if properties.last_block {
                break;
            }
        }
        if self.fparams.frame_content_size != CONTENT_SIZE_UNKNOWN
            && (op - op_start) as u64 != self.fparams.frame_content_size
        {
            return Err(Error::CorruptionDetected);
        }
        if self.fparams.checksum_flag {
            if input.len() - ip < 4 {
                return Err(Error::ChecksumWrong);
            }
            if read32(input, ip) != self.xxh.digest() as u32 {
                return Err(Error::ChecksumWrong);
            }
            ip += 4;
        }
        *src = &input[ip..];
        Ok(op - op_start)
    }

    /// Decompresses every frame of `src` into `dst` (`ZSTD_decompressDCtx`); skippable
    /// frames are skipped. Returns the decompressed size.
    pub fn decompress(&mut self, dst: &mut [u8], src: &[u8]) -> Result<usize> {
        let mut src = src;
        let mut op = 0usize;
        let mut more_than_one_frame = false;
        while src.len() >= FRAME_HEADER_SIZE_PREFIX {
            if read32(src, 0) & MAGIC_SKIPPABLE_MASK == MAGIC_SKIPPABLE_START {
                let size = skippable_frame_size(src)?;
                src = &src[size..];
                continue;
            }
            self.begin();
            self.check_continuity(op, dst.len() - op);
            match self.decompress_frame(dst, op, &mut src) {
                Err(Error::PrefixUnknown) if more_than_one_frame => return Err(Error::SrcSizeWrong),
                Err(error) => return Err(error),
                Ok(size) => op += size,
            }
            more_than_one_frame = true;
        }
        if !src.is_empty() {
            return Err(Error::SrcSizeWrong);
        }
        Ok(op)
    }

    /// `ZSTD_nextSrcSizeToDecompressWithInputSize`: raw blocks may stream partially.
    fn next_src_size_with_input(&self, input_size: usize) -> usize {
        if !matches!(self.stage, Stage::DecompressBlock | Stage::DecompressLastBlock)
            || self.block_type != BT_RAW
        {
            return self.expected;
        }
        input_size.min(self.expected).max(1)
    }

    /// `ZSTD_decompressContinue` into `out_buff[dst..dst + dst_capacity]`: `src` is exactly
    /// the input expected next. Returns the bytes decoded.
    fn decompress_continue(&mut self, dst: usize, dst_capacity: usize, src: &[u8]) -> Result<usize> {
        if src.len() != self.next_src_size_with_input(src.len()) {
            return Err(Error::SrcSizeWrong);
        }
        self.check_continuity(dst, dst_capacity);
        match self.stage {
            Stage::GetFrameHeaderSize => {
                if read32(src, 0) & MAGIC_SKIPPABLE_MASK == MAGIC_SKIPPABLE_START {
                    self.header_buffer[..src.len()].copy_from_slice(src);
                    self.expected = SKIPPABLE_HEADER_SIZE - src.len();
                    self.stage = Stage::DecodeSkippableHeader;
                    return Ok(0);
                }
                self.header_size = frame_header_size(src)?;
                self.header_buffer[..src.len()].copy_from_slice(src);
                self.expected = self.header_size - src.len();
                self.stage = Stage::DecodeFrameHeader;
                Ok(0)
            }
            Stage::DecodeFrameHeader => {
                let at = self.header_size - src.len();
                self.header_buffer[at..self.header_size].copy_from_slice(src);
                let header = self.header_buffer;
                self.decode_frame_header(&header[..self.header_size])?;
                self.expected = BLOCK_HEADER_SIZE;
                self.stage = Stage::DecodeBlockHeader;
                Ok(0)
            }
            Stage::DecodeBlockHeader => {
                let (c_block_size, properties) = get_c_block_size(src)?;
                if c_block_size > self.fparams.block_size_max {
                    return Err(Error::CorruptionDetected);
                }
                self.expected = c_block_size;
                self.block_type = properties.block_type;
                self.rle_size = properties.orig_size;
                if c_block_size > 0 {
                    self.stage = if properties.last_block {
                        Stage::DecompressLastBlock
                    } else {
                        Stage::DecompressBlock
                    };
                    return Ok(0);
                }
                // An empty block.
                if properties.last_block {
                    self.end_frame();
                } else {
                    self.expected = BLOCK_HEADER_SIZE;
                    self.stage = Stage::DecodeBlockHeader;
                }
                Ok(0)
            }
            Stage::DecompressBlock | Stage::DecompressLastBlock => {
                let r_size = match self.block_type {
                    BT_COMPRESSED => {
                        let size = self.block.decompress_block(
                            src,
                            &mut self.out_buff,
                            dst,
                            dst + dst_capacity,
                            self.window,
                            self.fparams.block_size_max,
                        )?;
                        self.expected = 0;
                        size
                    }
                    BT_RAW => {
                        if src.len() > dst_capacity {
                            return Err(Error::DstSizeTooSmall);
                        }
                        self.out_buff[dst..dst + src.len()].copy_from_slice(src);
                        self.expected -= src.len();
                        src.len()
                    }
                    _ => {
                        if self.rle_size > dst_capacity {
                            return Err(Error::DstSizeTooSmall);
                        }
                        self.out_buff[dst..dst + self.rle_size].fill(src[0]);
                        self.expected = 0;
                        self.rle_size
                    }
                };
                if r_size > self.fparams.block_size_max {
                    return Err(Error::CorruptionDetected);
                }
                self.decoded_size += r_size as u64;
                if self.validate_checksum {
                    self.xxh.update(&self.out_buff[dst..dst + r_size]);
                }
                self.previous_dst_end = Some(dst + r_size);
                if self.expected > 0 {
                    return Ok(r_size);
                }
                if self.stage == Stage::DecompressLastBlock {
                    if self.fparams.frame_content_size != CONTENT_SIZE_UNKNOWN
                        && self.decoded_size != self.fparams.frame_content_size
                    {
                        return Err(Error::CorruptionDetected);
                    }
                    self.end_frame();
                } else {
                    self.stage = Stage::DecodeBlockHeader;
                    self.expected = BLOCK_HEADER_SIZE;
                }
                Ok(r_size)
            }
            Stage::CheckChecksum => {
                if self.validate_checksum && read32(src, 0) != self.xxh.digest() as u32 {
                    return Err(Error::ChecksumWrong);
                }
                self.expected = 0;
                self.stage = Stage::GetFrameHeaderSize;
                Ok(0)
            }
            Stage::DecodeSkippableHeader => {
                let at = SKIPPABLE_HEADER_SIZE - src.len();
                self.header_buffer[at..SKIPPABLE_HEADER_SIZE].copy_from_slice(src);
                self.expected = read32(&self.header_buffer, FRAME_ID_SIZE) as usize;
                self.stage = Stage::SkipFrame;
                Ok(0)
            }
            Stage::SkipFrame => {
                self.expected = 0;
                self.stage = Stage::GetFrameHeaderSize;
                Ok(0)
            }
        }
    }

    /// After the last block: the checksum, or the end of the frame.
    fn end_frame(&mut self) {
        if self.fparams.checksum_flag {
            self.expected = 4;
            self.stage = Stage::CheckChecksum;
        } else {
            self.expected = 0;
            self.stage = Stage::GetFrameHeaderSize;
        }
    }

    /// `ZSTD_decompressContinueStream` with buffered output.
    fn decompress_continue_stream(&mut self, src: &[u8]) -> Result<()> {
        let skip_frame = self.stage == Stage::SkipFrame;
        let dst_size = if skip_frame {
            0
        } else {
            self.out_buff_size - self.out_start
        };
        let decoded = self.decompress_continue(self.out_start, dst_size, src)?;
        if decoded == 0 && !skip_frame {
            self.stream_stage = StreamStage::Read;
        } else {
            self.out_end = self.out_start + decoded;
            self.stream_stage = StreamStage::Flush;
        }
        Ok(())
    }

    /// `ZSTD_decodingBufferSize_internal`: a window, two blocks and their wild copies, or
    /// the whole content when smaller.
    fn decoding_buffer_size(
        window_size: u64,
        frame_content_size: u64,
        block_size_max: usize,
    ) -> Result<usize> {
        let block_size = window_size.min(BLOCK_SIZE_MAX as u64).min(block_size_max as u64);
        let needed = window_size + block_size * 2 + WILDCOPY_OVERLENGTH as u64 * 2;
        usize::try_from(frame_content_size.min(needed)).map_err(|_| Error::FrameParameterWindowTooLarge)
    }

    /// Streaming decompression (`ZSTD_decompressStream`): consumes `input[*in_pos..]` and
    /// writes to `output[*out_pos..]`, advancing both, across any split of the frames.
    /// Returns 0 when a frame is complete and flushed, otherwise a hint of the input size
    /// to provide next.
    pub fn decompress_stream(
        &mut self,
        output: &mut [u8],
        out_pos: &mut usize,
        input: &[u8],
        in_pos: &mut usize,
    ) -> Result<usize> {
        if *in_pos > input.len() {
            return Err(Error::SrcSizeWrong);
        }
        if *out_pos > output.len() {
            return Err(Error::DstSizeTooSmall);
        }
        let istart = *in_pos;
        let iend = input.len();
        let ostart = *out_pos;
        let oend = output.len();
        let mut ip = istart;
        let mut op = ostart;
        loop {
            match self.stream_stage {
                StreamStage::Init => {
                    self.stream_stage = StreamStage::LoadHeader;
                    self.lh_size = 0;
                    self.in_pos = 0;
                    self.out_start = 0;
                    self.out_end = 0;
                    self.hostage_byte = false;
                }
                StreamStage::LoadHeader => {
                    match get_frame_header(&self.header_buffer[..self.lh_size])? {
                        HeaderRead::Need(h_size) => {
                            let to_load = h_size - self.lh_size;
                            let remaining = iend - ip;
                            if to_load > remaining {
                                self.header_buffer[self.lh_size..self.lh_size + remaining]
                                    .copy_from_slice(&input[ip..iend]);
                                self.lh_size += remaining;
                                *in_pos = iend;
                                *out_pos = op;
                                // Check the first bytes.
                                get_frame_header(&self.header_buffer[..self.lh_size])?;
                                return Ok(
                                    h_size.max(FRAME_HEADER_SIZE_MIN) - self.lh_size + BLOCK_HEADER_SIZE
                                );
                            }
                            self.header_buffer[self.lh_size..h_size]
                                .copy_from_slice(&input[ip..ip + to_load]);
                            self.lh_size = h_size;
                            ip += to_load;
                            continue;
                        }
                        HeaderRead::Header(header) => self.fparams = header,
                    }
                    // The whole frame is in the input and fits in the output: decode it in
                    // one pass.
                    if self.fparams.frame_content_size != CONTENT_SIZE_UNKNOWN
                        && self.fparams.frame_type != FrameType::Skippable
                        && (oend - op) as u64 >= self.fparams.frame_content_size
                        && let Ok(c_size) = find_frame_compressed_size(&input[istart..iend])
                        && c_size <= iend - istart
                    {
                        let size = self.decompress(&mut output[op..], &input[istart..istart + c_size])?;
                        ip = istart + c_size;
                        op += size;
                        self.expected = 0;
                        self.stream_stage = StreamStage::Init;
                        break;
                    }
                    self.begin();
                    if read32(&self.header_buffer, 0) & MAGIC_SKIPPABLE_MASK == MAGIC_SKIPPABLE_START {
                        self.expected = read32(&self.header_buffer, FRAME_ID_SIZE) as usize;
                        self.stage = Stage::SkipFrame;
                    } else {
                        let header = self.header_buffer;
                        self.decode_frame_header(&header[..self.lh_size])?;
                        self.expected = BLOCK_HEADER_SIZE;
                        self.stage = Stage::DecodeBlockHeader;
                    }
                    self.fparams.window_size = self.fparams.window_size.max(1 << WINDOW_LOG_ABSOLUTE_MIN);
                    if self.fparams.window_size > MAX_WINDOW_SIZE {
                        return Err(Error::FrameParameterWindowTooLarge);
                    }
                    let needed_in = self.fparams.block_size_max.max(4);
                    let needed_out = Self::decoding_buffer_size(
                        self.fparams.window_size,
                        self.fparams.frame_content_size,
                        self.fparams.block_size_max,
                    )?;
                    if self.in_buff.len() < needed_in {
                        self.in_buff = vec![0; needed_in];
                    }
                    if self.out_buff.len() < needed_out {
                        self.out_buff = vec![0; needed_out];
                    }
                    self.out_buff_size = needed_out;
                    self.stream_stage = StreamStage::Read;
                }
                StreamStage::Read => {
                    let needed = self.next_src_size_with_input(iend - ip);
                    if needed == 0 {
                        // The end of the frame.
                        self.stream_stage = StreamStage::Init;
                        break;
                    }
                    if iend - ip >= needed {
                        self.decompress_continue_stream(&input[ip..ip + needed])?;
                        ip += needed;
                        continue;
                    }
                    if ip == iend {
                        break;
                    }
                    self.stream_stage = StreamStage::Load;
                }
                StreamStage::Load => {
                    let needed = self.expected;
                    let to_load = needed - self.in_pos;
                    let loaded;
                    if self.stage == Stage::SkipFrame {
                        loaded = to_load.min(iend - ip);
                    } else {
                        if to_load > self.in_buff.len() - self.in_pos {
                            return Err(Error::CorruptionDetected);
                        }
                        loaded = to_load.min(iend - ip);
                        self.in_buff[self.in_pos..self.in_pos + loaded]
                            .copy_from_slice(&input[ip..ip + loaded]);
                    }
                    ip += loaded;
                    self.in_pos += loaded;
                    if loaded < to_load {
                        break;
                    }
                    self.in_pos = 0;
                    if self.stage == Stage::SkipFrame {
                        // The skipped content was not kept: end the skippable frame.
                        self.expected = 0;
                        self.stage = Stage::GetFrameHeaderSize;
                        self.out_end = self.out_start;
                        self.stream_stage = StreamStage::Flush;
                        continue;
                    }
                    let in_buff = std::mem::take(&mut self.in_buff);
                    let result = self.decompress_continue_stream(&in_buff[..needed]);
                    self.in_buff = in_buff;
                    result?;
                }
                StreamStage::Flush => {
                    let to_flush = self.out_end - self.out_start;
                    let flushed = to_flush.min(oend - op);
                    output[op..op + flushed]
                        .copy_from_slice(&self.out_buff[self.out_start..self.out_start + flushed]);
                    op += flushed;
                    self.out_start += flushed;
                    if flushed != to_flush {
                        break;
                    }
                    self.stream_stage = StreamStage::Read;
                    if (self.out_buff_size as u64) < self.fparams.frame_content_size
                        && self.out_start + self.fparams.block_size_max > self.out_buff_size
                    {
                        self.out_start = 0;
                        self.out_end = 0;
                    }
                }
            }
        }
        *in_pos = ip;
        *out_pos = op;
        if ip == istart && op == ostart {
            self.no_forward_progress += 1;
            if self.no_forward_progress >= NO_FORWARD_PROGRESS_MAX {
                if op == oend {
                    return Err(Error::NoForwardProgressDestFull);
                }
                if ip == iend {
                    return Err(Error::NoForwardProgressInputEmpty);
                }
            }
        } else {
            self.no_forward_progress = 0;
        }
        let mut next = self.expected;
        if next == 0 {
            // The frame is decoded.
            if self.out_end == self.out_start {
                if self.hostage_byte {
                    if *in_pos >= input.len() {
                        // The hostage byte is not in this input.
                        self.stream_stage = StreamStage::Read;
                        return Ok(1);
                    }
                    *in_pos += 1;
                }
                return Ok(0);
            }
            if !self.hostage_byte {
                // Keep the last input byte until all output is flushed.
                *in_pos -= 1;
                self.hostage_byte = true;
            }
            return Ok(1);
        }
        if matches!(self.stage, Stage::DecompressBlock) {
            next += BLOCK_HEADER_SIZE;
        }
        Ok(next - self.in_pos)
    }
}
