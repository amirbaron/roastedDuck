//! `COPY ... TO` (DuckDB's `LogicalCopyToFile`, `FilenamePattern` and the copy
//! function return types).

use std::fmt::Write as _;
use std::sync::Arc;

use roastedduck_function::{CopyFunction, FunctionData};
use roastedduck_types::LogicalType;

/// What happens to existing files (DuckDB's `CopyOverwriteMode`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyOverwriteMode {
    ErrorOnConflict,
    Overwrite,
    OverwriteOrIgnore,
    Append,
}

/// What a COPY returns (DuckDB's `CopyFunctionReturnType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyFunctionReturnType {
    ChangedRows,
    ChangedRowsAndFileList,
    WrittenFileStatistics,
}

impl CopyFunctionReturnType {
    /// `GetCopyFunctionReturnNames`.
    pub fn names(self) -> Vec<roastedduck_common::ByteString> {
        match self {
            Self::ChangedRows => vec!["Count".into()],
            Self::ChangedRowsAndFileList => vec!["Count".into(), "Files".into()],
            Self::WrittenFileStatistics => [
                "filename",
                "count",
                "file_size_bytes",
                "footer_size_bytes",
                "column_statistics",
                "partition_keys",
            ]
            .into_iter()
            .map(Into::into)
            .collect(),
        }
    }

    /// `GetCopyFunctionReturnLogicalTypes`.
    pub fn types(self) -> Vec<LogicalType> {
        match self {
            Self::ChangedRows => vec![LogicalType::BIGINT],
            Self::ChangedRowsAndFileList => {
                vec![LogicalType::BIGINT, LogicalType::list(LogicalType::VARCHAR)]
            }
            Self::WrittenFileStatistics => vec![
                LogicalType::VARCHAR,
                LogicalType::UBIGINT,
                LogicalType::UBIGINT,
                LogicalType::UBIGINT,
                LogicalType::map(
                    LogicalType::VARCHAR,
                    LogicalType::map(LogicalType::VARCHAR, LogicalType::VARCHAR),
                ),
                LogicalType::map(LogicalType::VARCHAR, LogicalType::VARCHAR),
            ],
        }
    }
}

/// One part of a [`FilenamePattern`] (DuckDB's `FileNameSegment`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileNameSegment {
    Literal(roastedduck_common::ByteString),
    Offset,
    UuidV4,
    UuidV7,
}

/// The names of written files: literals with `{i}`, `{uuid}` (`{uuidv4}`) and
/// `{uuidv7}` placeholders (DuckDB's `FilenamePattern`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilenamePattern {
    segments: Vec<FileNameSegment>,
}

impl Default for FilenamePattern {
    /// `data_{i}`.
    fn default() -> Self {
        Self {
            segments: vec![FileNameSegment::Literal("data_".into()), FileNameSegment::Offset],
        }
    }
}

/// A random version 4 UUID in its text form.
fn random_uuid(version: u8) -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let mut bytes = [0u8; 16];
    for chunk in bytes.as_chunks_mut::<8>().0 {
        let mut hasher = RandomState::new().build_hasher();
        hasher.write_u64(0);
        chunk.copy_from_slice(&hasher.finish().to_le_bytes());
    }
    bytes[6] = (bytes[6] & 0x0f) | (version << 4);
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let mut result = String::with_capacity(36);
    for (i, byte) in bytes.iter().enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) {
            result.push('-');
        }
        let _ = write!(result, "{byte:02x}");
    }
    result
}

impl roastedduck_serde::Serialize for FileNameSegment {
    fn serialize(
        &self,
        serializer: &mut dyn roastedduck_serde::Serializer,
    ) -> roastedduck_common::Result<()> {
        let (segment_type, data) = match self {
            Self::Literal(text) => (crate::FileNameSegmentType::Literal, text),
            Self::Offset => (
                crate::FileNameSegmentType::Offset,
                &roastedduck_common::ByteString::new(),
            ),
            Self::UuidV4 => (
                crate::FileNameSegmentType::UuidV4,
                &roastedduck_common::ByteString::new(),
            ),
            Self::UuidV7 => (
                crate::FileNameSegmentType::UuidV7,
                &roastedduck_common::ByteString::new(),
            ),
        };
        serializer.write_object(&crate::generated::logical_operator::FileNameSegment { segment_type, data })
    }
}

impl roastedduck_serde::Serialize for FilenamePattern {
    /// `FilenamePattern::Serialize`: patterns with a single placeholder other than
    /// `{uuidv7}` are written in the legacy form, `base` with the placeholder at `pos`.
    fn serialize(
        &self,
        serializer: &mut dyn roastedduck_serde::Serializer,
    ) -> roastedduck_common::Result<()> {
        let mut placeholders = 0;
        let mut legacy = true;
        let mut base = roastedduck_common::ByteString::new();
        let mut pos = 0;
        for segment in &self.segments {
            match segment {
                FileNameSegment::Literal(text) => base += text,
                FileNameSegment::UuidV7 => legacy = false,
                FileNameSegment::Offset | FileNameSegment::UuidV4 => {
                    placeholders += 1;
                    pos = base.len() as u64;
                }
            }
        }
        let legacy = legacy && placeholders == 1;
        let empty = roastedduck_common::ByteString::new();
        serializer.write_object(&crate::generated::logical_operator::FilenamePattern {
            base: if legacy { &base } else { &empty },
            pos: if legacy { pos } else { 0 },
            uuid: self.has_uuid(),
            segments: if legacy { &[] } else { &self.segments },
        })
    }
}

impl FilenamePattern {
    /// `SetFilenamePattern`.
    pub fn set_pattern(&mut self, pattern: impl AsRef<[u8]>) {
        const PATTERNS: [(&str, Option<FileNameSegment>); 4] = [
            ("{i}", Some(FileNameSegment::Offset)),
            ("{uuid}", Some(FileNameSegment::UuidV4)),
            ("{uuidv4}", Some(FileNameSegment::UuidV4)),
            ("{uuidv7}", Some(FileNameSegment::UuidV7)),
        ];
        let pattern = pattern.as_ref();
        self.segments.clear();
        let mut current_pos = 0;
        let mut i = 0;
        while i < pattern.len() {
            if pattern[i] != b'{' {
                i += 1;
                continue;
            }
            let found = PATTERNS
                .iter()
                .find(|(text, _)| pattern[i..].starts_with(text.as_bytes()));
            if let Some((text, Some(segment))) = found {
                if current_pos < i {
                    self.segments
                        .push(FileNameSegment::Literal(pattern[current_pos..i].into()));
                }
                self.segments.push(segment.clone());
                current_pos = i + text.len();
                i = current_pos;
                continue;
            }
            i += 1;
        }
        if current_pos < pattern.len() {
            self.segments
                .push(FileNameSegment::Literal(pattern[current_pos..].into()));
        }
        if let [FileNameSegment::Literal(_)] = self.segments.as_slice() {
            // Only a literal: the offset is appended.
            self.segments.push(FileNameSegment::Offset);
        }
    }

    /// `HasUUID`.
    pub fn has_uuid(&self) -> bool {
        self.segments
            .iter()
            .any(|segment| matches!(segment, FileNameSegment::UuidV4 | FileNameSegment::UuidV7))
    }

    /// `CreateFilename`: `<path>/<name>.<extension>`.
    pub fn create_filename(
        &self,
        path: &[u8],
        extension: &str,
        offset: u64,
    ) -> roastedduck_common::ByteString {
        let mut result = roastedduck_common::ByteString::new();
        for segment in &self.segments {
            match segment {
                FileNameSegment::Literal(text) => result += text,
                FileNameSegment::UuidV4 => result += &random_uuid(4),
                FileNameSegment::UuidV7 => result += &random_uuid(7),
                FileNameSegment::Offset => result += &offset.to_string(),
            }
        }
        let separator = if path.is_empty() { "" } else { "/" };
        roastedduck_common::byte_format!("{path}{separator}{result}.{extension}")
    }
}

/// `COPY (query) TO 'file'` (DuckDB's `LogicalCopyToFile`); its child produces the rows.
#[derive(Clone, Debug)]
#[allow(clippy::struct_excessive_bools)]
pub struct LogicalCopyToFile {
    pub function: CopyFunction,
    pub bind_data: Arc<dyn FunctionData>,
    pub file_path: roastedduck_common::ByteString,
    pub use_tmp_file: bool,
    pub overwrite_mode: CopyOverwriteMode,
    pub filename_pattern: FilenamePattern,
    pub file_extension: String,
    pub per_thread_output: bool,
    pub file_size_bytes: Option<u64>,
    pub rotate: bool,
    pub partition_output: bool,
    pub write_partition_columns: bool,
    pub partition_columns: Vec<usize>,
    pub write_empty_file: bool,
    pub hive_file_pattern: bool,
    /// The `PRESERVE_ORDER` option (`PreserveOrderType`); `None` when not given.
    pub preserve_order: Option<bool>,
    pub return_type: CopyFunctionReturnType,
    pub names: Vec<roastedduck_common::ByteString>,
    pub expected_types: Vec<LogicalType>,
    /// The statement, its query filled in (DuckDB's `copy_info`).
    pub copy_info: Box<roastedduck_ast::CopyInfo>,
}

impl LogicalCopyToFile {
    /// `GetNamesWithoutPartitions` / `GetTypesWithoutPartitions`: the written columns.
    pub fn without_partitions<T: Clone>(
        values: &[T],
        partition_columns: &[usize],
        write_partition_columns: bool,
    ) -> Vec<T> {
        if write_partition_columns || partition_columns.is_empty() {
            return values.to_vec();
        }
        values
            .iter()
            .enumerate()
            .filter(|(i, _)| !partition_columns.contains(i))
            .map(|(_, value)| value.clone())
            .collect()
    }
}

/// A table written by `EXPORT DATABASE` (DuckDB's `ExportedTableData`).
pub type ExportedTable = roastedduck_ast::ExportedTableData;

/// `EXPORT DATABASE` (DuckDB's `LogicalExport`): its child (if any) copies the tables;
/// the operator then writes `schema.sql` and `load.sql`.
#[derive(Clone, Debug)]
pub struct LogicalExport {
    /// The actual bound export options, including defaults/readable options.
    pub copy_info: roastedduck_ast::CopyInfo,
    pub function: CopyFunction,
    pub file_path: roastedduck_common::ByteString,
    pub format: roastedduck_common::ByteString,
    /// The options of the generated `COPY ... FROM` statements.
    pub options: Vec<(roastedduck_common::ByteString, Vec<roastedduck_types::Value>)>,
    pub catalog: roastedduck_common::ByteString,
    pub tables: Vec<ExportedTable>,
}
