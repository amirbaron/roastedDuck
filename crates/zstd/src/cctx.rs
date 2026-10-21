//! Frames: one-shot compression (`ZSTD_compress`) and streaming compression
//! (`ZSTD_compressStream2` with buffered input and output) — `zstd_compress.c`.

use crate::block::BlockCompressor;
use crate::params::{
    BLOCK_SIZE_MAX, CONTENT_SIZE_UNKNOWN, CParams, DEFAULT_LEVEL, Strategy, WINDOW_LOG_ABSOLUTE_MIN,
    compress_bound, get_cparams,
};
use crate::window::{Buf, Mem};

const MAGIC_NUMBER: u32 = 0xFD2F_B528;
const BLOCK_HEADER_SIZE: usize = 3;
const BT_RAW: u32 = 0;
const BT_RLE: u32 = 1;
const BT_COMPRESSED: u32 = 2;

/// What a streaming call should do with the input (`ZSTD_EndDirective`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndDirective {
    /// Compress full blocks, buffering the rest (`ZSTD_e_continue`).
    Continue,
    /// Compress everything buffered, ending the current block (`ZSTD_e_flush`).
    Flush,
    /// Compress everything and end the frame (`ZSTD_e_end`).
    End,
}

/// `ZSTD_compressionStage_e`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Created,
    Init,
    Ongoing,
    Ending,
}

/// `ZSTD_cStreamStage`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StreamStage {
    Init,
    Load,
    Flush,
}

/// The parameters of `level` that this port compresses with: the strategies above
/// `ZSTD_dfast` (levels 4 or 5 and above, depending on the size) run the double-fast match
/// finder with tables of at most 2^20 entries, keeping the level's window.
fn effective_params(level: i32, src_size: u64) -> CParams {
    let mut params = get_cparams(level, src_size);
    if params.strategy > Strategy::DFast {
        params.strategy = Strategy::DFast;
        params.hash_log = params.hash_log.min(20);
        params.chain_log = params.chain_log.min(20);
        params.min_match = params.min_match.max(4);
    }
    params
}

/// A compression context (`ZSTD_CCtx`), reusable for many frames.
#[derive(Debug)]
pub struct Compressor {
    level: i32,
    params: CParams,
    /// The size pledged for the frame, or `CONTENT_SIZE_UNKNOWN`.
    pledged_src_size: u64,
    consumed_src_size: u64,
    block_size: usize,
    stage: Stage,
    blocks: BlockCompressor,
    /// The compressed blocks of the current call.
    scratch: Vec<u8>,
    // Streaming.
    stream_stage: StreamStage,
    /// The size of the first input of `ZSTD_e_end` when the frame starts with it
    /// (`pledgedSrcSizePlusOne`).
    pledged_from_end: Option<u64>,
    in_buff: Vec<u8>,
    in_buff_size: usize,
    in_to_compress: usize,
    in_buff_pos: usize,
    in_buff_target: usize,
    out_buff: Vec<u8>,
    out_buff_size: usize,
    out_buff_content_size: usize,
    out_buff_flushed_size: usize,
    frame_ended: bool,
}

impl Default for Compressor {
    fn default() -> Self {
        Self::new(DEFAULT_LEVEL)
    }
}

impl Compressor {
    /// A context compressing at `level` (0 is the default level).
    pub fn new(level: i32) -> Self {
        let params = get_cparams(level, CONTENT_SIZE_UNKNOWN);
        Self {
            level,
            params,
            pledged_src_size: CONTENT_SIZE_UNKNOWN,
            consumed_src_size: 0,
            block_size: BLOCK_SIZE_MAX,
            stage: Stage::Created,
            blocks: BlockCompressor::new(params),
            scratch: Vec::new(),
            stream_stage: StreamStage::Init,
            pledged_from_end: None,
            in_buff: Vec::new(),
            in_buff_size: 0,
            in_to_compress: 0,
            in_buff_pos: 0,
            in_buff_target: 0,
            out_buff: Vec::new(),
            out_buff_size: 0,
            out_buff_content_size: 0,
            out_buff_flushed_size: 0,
            frame_ended: false,
        }
    }

    /// Sets the level of the next frame (`ZSTD_c_compressionLevel`).
    pub fn set_level(&mut self, level: i32) {
        self.level = level;
    }

    /// Abandons the current frame (`ZSTD_CCtx_reset(ZSTD_reset_session_only)`).
    pub fn reset_session(&mut self) {
        self.stream_stage = StreamStage::Init;
        self.pledged_from_end = None;
    }

    /// `ZSTD_compressBegin_internal` / `ZSTD_resetCCtx_internal`.
    fn begin(&mut self, pledged_src_size: u64, buffered: bool) {
        let level = if self.level == 0 {
            DEFAULT_LEVEL
        } else {
            self.level
        };
        self.params = effective_params(level, pledged_src_size);
        self.pledged_src_size = pledged_src_size;
        self.consumed_src_size = 0;
        let window_size = (1u64 << self.params.window_log).min(pledged_src_size).max(1) as usize;
        self.block_size = BLOCK_SIZE_MAX.min(window_size);
        self.stage = Stage::Init;
        self.blocks.reset(self.params);
        if buffered {
            self.in_buff_size = window_size + self.block_size;
            if self.in_buff.len() < self.in_buff_size {
                self.in_buff.resize(self.in_buff_size, 0);
            }
            self.out_buff_size = compress_bound(self.block_size) + 1;
            if self.out_buff.len() < self.out_buff_size {
                self.out_buff.resize(self.out_buff_size, 0);
            }
        }
    }

    /// `ZSTD_writeFrameHeader`.
    fn write_frame_header(&self, out: &mut Vec<u8>, pledged_src_size: u64) {
        let content_size_flag = self.pledged_src_size != CONTENT_SIZE_UNKNOWN;
        let window_size = 1u64 << self.params.window_log;
        let single_segment = content_size_flag && window_size >= pledged_src_size;
        let window_log_byte = ((self.params.window_log - WINDOW_LOG_ABSOLUTE_MIN) << 3) as u8;
        let fcs_code = if content_size_flag {
            u8::from(pledged_src_size >= 256)
                + u8::from(pledged_src_size >= 65536 + 256)
                + u8::from(pledged_src_size >= 0xFFFF_FFFF)
        } else {
            0
        };
        out.extend_from_slice(&MAGIC_NUMBER.to_le_bytes());
        out.push((u8::from(single_segment) << 5) + (fcs_code << 6));
        if !single_segment {
            out.push(window_log_byte);
        }
        match fcs_code {
            0 => {
                if single_segment {
                    out.push(pledged_src_size as u8);
                }
            }
            1 => out.extend_from_slice(&((pledged_src_size - 256) as u16).to_le_bytes()),
            2 => out.extend_from_slice(&(pledged_src_size as u32).to_le_bytes()),
            _ => out.extend_from_slice(&pledged_src_size.to_le_bytes()),
        }
    }

    /// `ZSTD_compress_frameChunk`: compresses `size` bytes at `pos` of the current segment
    /// (buffer `cur`) into blocks.
    fn frame_chunk(
        &mut self,
        out: &mut Vec<u8>,
        mem: Mem<'_>,
        pos: usize,
        size: usize,
        last_frame_chunk: bool,
    ) {
        let start = out.len();
        let mut block_size = self.block_size;
        let mut remaining = size;
        let mut ip = pos;
        let max_dist = 1u32 << self.params.window_log;
        while remaining > 0 {
            let last_block = last_frame_chunk && block_size >= remaining;
            if remaining < block_size {
                block_size = remaining;
            }
            let ms = &mut self.blocks.ms;
            let index = (ip as i64 + ms.window.base) as u32;
            ms.window.enforce_max_dist(index, max_dist);
            if ms.next_to_update < ms.window.low_limit {
                ms.next_to_update = ms.window.low_limit;
            }
            let header_pos = out.len();
            out.extend_from_slice(&[0; BLOCK_HEADER_SIZE]);
            let c_size = self.blocks.compress_block(out, mem, ip, block_size);
            let header = if c_size == 0 {
                out.extend_from_slice(&mem.cur[ip..ip + block_size]);
                u32::from(last_block) + (BT_RAW << 1) + ((block_size as u32) << 3)
            } else if c_size == 1 {
                u32::from(last_block) + (BT_RLE << 1) + ((block_size as u32) << 3)
            } else {
                u32::from(last_block) + (BT_COMPRESSED << 1) + ((c_size as u32) << 3)
            };
            out[header_pos..header_pos + BLOCK_HEADER_SIZE].copy_from_slice(&header.to_le_bytes()[..3]);
            ip += block_size;
            remaining -= block_size;
            self.blocks.is_first_block = false;
        }
        if last_frame_chunk && out.len() > start {
            self.stage = Stage::Ending;
        }
    }

    /// `ZSTD_compressContinue_internal` (frame mode) for `size` bytes at `pos` of `buf`
    /// (`data` is the caller's input, for `Buf::Src`).
    fn compress_continue(
        &mut self,
        out: &mut Vec<u8>,
        buf: Buf,
        data: &[u8],
        pos: usize,
        size: usize,
        last: bool,
    ) {
        if self.stage == Stage::Init {
            self.write_frame_header(out, self.pledged_src_size);
            self.stage = Stage::Ongoing;
        }
        if size == 0 {
            return;
        }
        let ms = &mut self.blocks.ms;
        if !ms.window.update(buf, pos, size) {
            ms.next_to_update = ms.window.dict_limit;
        }
        let dict = ms.window.dict;
        let in_buff = std::mem::take(&mut self.in_buff);
        {
            let select = |which: Buf| -> &[u8] { if which == Buf::InBuff { &in_buff } else { data } };
            let mem = Mem {
                cur: select(buf),
                dict: select(dict),
            };
            self.frame_chunk(out, mem, pos, size, last);
        }
        self.in_buff = in_buff;
        self.consumed_src_size += size as u64;
    }

    /// `ZSTD_writeEpilogue`.
    fn write_epilogue(&mut self, out: &mut Vec<u8>) {
        if self.stage == Stage::Init {
            self.write_frame_header(out, 0);
            self.stage = Stage::Ongoing;
        }
        if self.stage != Stage::Ending {
            out.extend_from_slice(&[1, 0, 0]);
        }
        self.stage = Stage::Created;
    }

    /// `ZSTD_compressEnd_public`.
    fn compress_end(&mut self, out: &mut Vec<u8>, buf: Buf, data: &[u8], pos: usize, size: usize) {
        self.compress_continue(out, buf, data, pos, size, true);
        self.write_epilogue(out);
    }

    /// Compresses `src` into one frame appended to `out` (`ZSTD_compress`: the size is known
    /// and the parameters suit it).
    pub fn compress_into(&mut self, out: &mut Vec<u8>, src: &[u8]) {
        self.begin(src.len() as u64, false);
        self.compress_end(out, Buf::Src, src, 0, src.len());
        self.stream_stage = StreamStage::Init;
    }

    /// `ZSTD_CCtx_init_compressStream2`.
    fn init_stream(&mut self, end: EndDirective, in_size: usize) {
        if end == EndDirective::End {
            self.pledged_from_end = Some(in_size as u64);
        }
        let pledged = self.pledged_from_end.unwrap_or(CONTENT_SIZE_UNKNOWN);
        self.begin(pledged, true);
        self.in_to_compress = 0;
        self.in_buff_pos = 0;
        self.in_buff_target = self.block_size + usize::from(self.block_size as u64 == pledged);
        self.out_buff_content_size = 0;
        self.out_buff_flushed_size = 0;
        self.stream_stage = StreamStage::Load;
        self.frame_ended = false;
    }

    /// Streaming compression (`ZSTD_compressStream2` with buffered input and output):
    /// consumes `input[*in_pos..]` and writes to `output[*out_pos..]`, advancing both.
    /// Returns the bytes still to flush (0 when a frame ended or nothing is pending).
    pub fn compress_stream(
        &mut self,
        output: &mut [u8],
        out_pos: &mut usize,
        input: &[u8],
        in_pos: &mut usize,
        end: EndDirective,
    ) -> usize {
        if self.stream_stage == StreamStage::Init {
            self.init_stream(end, input.len() - *in_pos);
        }
        self.compress_stream_generic(output, out_pos, input, in_pos, end);
        self.out_buff_content_size - self.out_buff_flushed_size
    }

    /// `ZSTD_compressStream_generic`.
    fn compress_stream_generic(
        &mut self,
        output: &mut [u8],
        out_pos: &mut usize,
        input: &[u8],
        in_pos: &mut usize,
        flush_mode: EndDirective,
    ) {
        let iend = input.len();
        let oend = output.len();
        let mut ip = *in_pos;
        let mut op = *out_pos;
        loop {
            match self.stream_stage {
                StreamStage::Init => break,
                StreamStage::Load => {
                    if flush_mode == EndDirective::End
                        && oend - op >= compress_bound(iend - ip)
                        && self.in_buff_pos == 0
                    {
                        // Compress straight from the input into the output.
                        let mut frame = std::mem::take(&mut self.scratch);
                        frame.clear();
                        self.compress_end(&mut frame, Buf::Src, input, ip, iend - ip);
                        output[op..op + frame.len()].copy_from_slice(&frame);
                        op += frame.len();
                        self.scratch = frame;
                        ip = iend;
                        self.frame_ended = true;
                        self.reset_session();
                        break;
                    }
                    let to_load = self.in_buff_target - self.in_buff_pos;
                    let loaded = to_load.min(iend - ip);
                    self.in_buff[self.in_buff_pos..self.in_buff_pos + loaded]
                        .copy_from_slice(&input[ip..ip + loaded]);
                    self.in_buff_pos += loaded;
                    ip += loaded;
                    if flush_mode == EndDirective::Continue && self.in_buff_pos < self.in_buff_target {
                        break;
                    }
                    if flush_mode == EndDirective::Flush && self.in_buff_pos == self.in_to_compress {
                        break;
                    }
                    // Compress the buffered block.
                    let i_size = self.in_buff_pos - self.in_to_compress;
                    let last_block = flush_mode == EndDirective::End && ip == iend;
                    let mut block = std::mem::take(&mut self.scratch);
                    block.clear();
                    let start = self.in_to_compress;
                    if last_block {
                        self.compress_end(&mut block, Buf::InBuff, &[], start, i_size);
                    } else {
                        self.compress_continue(&mut block, Buf::InBuff, &[], start, i_size, false);
                    }
                    self.frame_ended = last_block;
                    self.in_buff_target = self.in_buff_pos + self.block_size;
                    if self.in_buff_target > self.in_buff_size {
                        self.in_buff_pos = 0;
                        self.in_buff_target = self.block_size;
                    }
                    self.in_to_compress = self.in_buff_pos;
                    let direct = oend - op >= compress_bound(i_size);
                    if direct {
                        output[op..op + block.len()].copy_from_slice(&block);
                        op += block.len();
                        self.scratch = block;
                        if self.frame_ended {
                            self.reset_session();
                            break;
                        }
                        continue;
                    }
                    self.out_buff[..block.len()].copy_from_slice(&block);
                    self.out_buff_content_size = block.len();
                    self.out_buff_flushed_size = 0;
                    self.scratch = block;
                    self.stream_stage = StreamStage::Flush;
                }
                StreamStage::Flush => {
                    let to_flush = self.out_buff_content_size - self.out_buff_flushed_size;
                    let flushed = to_flush.min(oend - op);
                    output[op..op + flushed].copy_from_slice(
                        &self.out_buff[self.out_buff_flushed_size..self.out_buff_flushed_size + flushed],
                    );
                    op += flushed;
                    self.out_buff_flushed_size += flushed;
                    if to_flush != flushed {
                        break;
                    }
                    self.out_buff_content_size = 0;
                    self.out_buff_flushed_size = 0;
                    if self.frame_ended {
                        self.reset_session();
                        break;
                    }
                    self.stream_stage = StreamStage::Load;
                }
            }
        }
        *in_pos = ip;
        *out_pos = op;
    }
}

/// Compresses `src` at `level` into one frame (`ZSTD_compress`).
pub fn compress(src: &[u8], level: i32) -> Vec<u8> {
    let mut out = Vec::with_capacity(compress_bound(src.len()));
    Compressor::new(level).compress_into(&mut out, src);
    out
}

/// The frame a stream writes for `src` at `level` when it is given as input
/// (`ZSTD_e_continue`) and then ended without more input (`ZSTD_e_end`): the content size
/// is unknown, unless there is no input at all.
pub fn compress_streamed(src: &[u8], level: i32) -> Vec<u8> {
    let mut compressor = Compressor::new(level);
    let mut out = Vec::with_capacity(compress_bound(src.len()) + 32);
    let mut buffer = vec![0u8; compress_bound(BLOCK_SIZE_MAX) + BLOCK_HEADER_SIZE + 4];
    let mut in_pos = 0;
    while in_pos < src.len() {
        let mut out_pos = 0;
        compressor.compress_stream(
            &mut buffer,
            &mut out_pos,
            src,
            &mut in_pos,
            EndDirective::Continue,
        );
        out.extend_from_slice(&buffer[..out_pos]);
    }
    loop {
        let (mut out_pos, mut empty_pos) = (0, 0);
        let remaining =
            compressor.compress_stream(&mut buffer, &mut out_pos, &[], &mut empty_pos, EndDirective::End);
        out.extend_from_slice(&buffer[..out_pos]);
        if remaining == 0 {
            return out;
        }
    }
}
