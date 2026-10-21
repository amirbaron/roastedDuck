//! The errors of decompression, named as `ZSTD_getErrorName` names them
//! (`zstd_errors.h`, `error_private.c`).

use std::fmt;

/// A decompression error (`ZSTD_ErrorCode`, the ones the decoder raises).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// `ZSTD_error_GENERIC`.
    Generic,
    /// `ZSTD_error_prefix_unknown`: the input does not start with a frame.
    PrefixUnknown,
    /// `ZSTD_error_frameParameter_unsupported`.
    FrameParameterUnsupported,
    /// `ZSTD_error_frameParameter_windowTooLarge`.
    FrameParameterWindowTooLarge,
    /// `ZSTD_error_corruption_detected`.
    CorruptionDetected,
    /// `ZSTD_error_checksum_wrong`.
    ChecksumWrong,
    /// `ZSTD_error_literals_headerWrong`.
    LiteralsHeaderWrong,
    /// `ZSTD_error_dictionary_corrupted`.
    DictionaryCorrupted,
    /// `ZSTD_error_dictionary_wrong`: the frame needs a dictionary.
    DictionaryWrong,
    /// `ZSTD_error_tableLog_tooLarge`.
    TableLogTooLarge,
    /// `ZSTD_error_maxSymbolValue_tooLarge`.
    MaxSymbolValueTooLarge,
    /// `ZSTD_error_maxSymbolValue_tooSmall`.
    MaxSymbolValueTooSmall,
    /// `ZSTD_error_dstSize_tooSmall`.
    DstSizeTooSmall,
    /// `ZSTD_error_srcSize_wrong`.
    SrcSizeWrong,
    /// `ZSTD_error_noForwardProgress_destFull`.
    NoForwardProgressDestFull,
    /// `ZSTD_error_noForwardProgress_inputEmpty`.
    NoForwardProgressInputEmpty,
}

impl Error {
    /// `ZSTD_getErrorName`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Generic => "Error (generic)",
            Self::PrefixUnknown => "Unknown frame descriptor",
            Self::FrameParameterUnsupported => "Unsupported frame parameter",
            Self::FrameParameterWindowTooLarge => "Frame requires too much memory for decoding",
            Self::CorruptionDetected => "Data corruption detected",
            Self::ChecksumWrong => "Restored data doesn't match checksum",
            Self::LiteralsHeaderWrong => "Header of Literals' block doesn't respect format specification",
            Self::DictionaryCorrupted => "Dictionary is corrupted",
            Self::DictionaryWrong => "Dictionary mismatch",
            Self::TableLogTooLarge => "tableLog requires too much memory : unsupported",
            Self::MaxSymbolValueTooLarge => "Unsupported max Symbol Value : too large",
            Self::MaxSymbolValueTooSmall => "Specified maxSymbolValue is too small",
            Self::DstSizeTooSmall => "Destination buffer is too small",
            Self::SrcSizeWrong => "Src size is incorrect",
            Self::NoForwardProgressDestFull => {
                "Operation made no progress over multiple calls, due to output buffer being full"
            }
            Self::NoForwardProgressInputEmpty => {
                "Operation made no progress over multiple calls, due to input being empty"
            }
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl std::error::Error for Error {}

impl From<Error> for std::io::Error {
    fn from(error: Error) -> Self {
        Self::other(error)
    }
}

pub(crate) type Result<T> = std::result::Result<T, Error>;
