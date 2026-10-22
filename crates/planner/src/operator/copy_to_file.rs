//! `COPY ... TO` (DuckDB's `LogicalCopyToFile`, `FilenamePattern` and the copy
//! function return types).

use std::fmt::Write as _;
use std::sync::Arc;

use rustdb_function::{CopyFunction, FunctionData};
use rustdb_types::LogicalType;

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
    pub fn names(self) -> Vec<String> {
        match self {
            Self::ChangedRows => vec!["Count".to_owned()],
            Self::ChangedRowsAndFileList => vec!["Count".to_owned(), "Files".to_owned()],
            Self::WrittenFileStatistics => [
                "filename",
                "count",
                "file_size_bytes",
                "footer_size_bytes",
                "column_statistics",
                "partition_keys",
            ]
            .into_iter()
            .map(str::to_owned)
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
    Literal(String),
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
            segments: vec![
                FileNameSegment::Literal("data_".to_owned()),
                FileNameSegment::Offset,
            ],
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

impl rustdb_serde::Serialize for FileNameSegment {
    fn serialize(&self, serializer: &mut dyn rustdb_serde::Serializer) -> rustdb_common::Result<()> {
        let (segment_type, data) = match self {
            Self::Literal(text) => (crate::FileNameSegmentType::Literal, text.as_str()),
            Self::Offset => (crate::FileNameSegmentType::Offset, ""),
            Self::UuidV4 => (crate::FileNameSegmentType::UuidV4, ""),
            Self::UuidV7 => (crate::FileNameSegmentType::UuidV7, ""),
        };
        serializer.write_object(&crate::generated::logical_operator::FileNameSegment { segment_type, data })
    }
}

impl rustdb_serde::Serialize for FilenamePattern {
    /// `FilenamePattern::Serialize`: patterns with a single placeholder other than
    /// `{uuidv7}` are written in the legacy form, `base` with the placeholder at `pos`.
    fn serialize(&self, serializer: &mut dyn rustdb_serde::Serializer) -> rustdb_common::Result<()> {
        let mut placeholders = 0;
        let mut legacy = true;
        let mut base = String::new();
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
        serializer.write_object(&crate::generated::logical_operator::FilenamePattern {
            base: if legacy { &base } else { "" },
            pos: if legacy { pos } else { 0 },
            uuid: self.has_uuid(),
            segments: if legacy { &[] } else { &self.segments },
        })
    }
}

impl FilenamePattern {
    /// `SetFilenamePattern`.
    pub fn set_pattern(&mut self, pattern: &str) {
        const PATTERNS: [(&str, Option<FileNameSegment>); 4] = [
            ("{i}", Some(FileNameSegment::Offset)),
            ("{uuid}", Some(FileNameSegment::UuidV4)),
            ("{uuidv4}", Some(FileNameSegment::UuidV4)),
            ("{uuidv7}", Some(FileNameSegment::UuidV7)),
        ];
        self.segments.clear();
        let mut current_pos = 0;
        let mut i = 0;
        while i < pattern.len() {
            if pattern.as_bytes()[i] != b'{' {
                i += 1;
                continue;
            }
            let found = PATTERNS
                .iter()
                .find(|(text, _)| pattern.as_bytes()[i..].starts_with(text.as_bytes()));
            if let Some((text, Some(segment))) = found {
                if current_pos < i {
                    self.segments
                        .push(FileNameSegment::Literal(pattern[current_pos..i].to_owned()));
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
                .push(FileNameSegment::Literal(pattern[current_pos..].to_owned()));
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
    pub fn create_filename(&self, path: &str, extension: &str, offset: u64) -> String {
        let mut result = String::new();
        for segment in &self.segments {
            match segment {
                FileNameSegment::Literal(text) => result += text,
                FileNameSegment::UuidV4 => result += &random_uuid(4),
                FileNameSegment::UuidV7 => result += &random_uuid(7),
                FileNameSegment::Offset => result += &offset.to_string(),
            }
        }
        let separator = if path.ends_with('/') { "" } else { "/" };
        format!("{path}{separator}{result}.{extension}")
    }
}

/// `COPY (query) TO 'file'` (DuckDB's `LogicalCopyToFile`); its child produces the rows.
#[derive(Clone, Debug)]
#[allow(clippy::struct_excessive_bools)]
pub struct LogicalCopyToFile {
    pub function: CopyFunction,
    pub bind_data: Arc<dyn FunctionData>,
    pub file_path: String,
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
    pub names: Vec<String>,
    pub expected_types: Vec<LogicalType>,
    /// The statement, its query filled in (DuckDB's `copy_info`).
    pub copy_info: Box<rustdb_ast::CopyInfo>,
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
#[derive(Clone, Debug)]
pub struct ExportedTable {
    pub schema_name: String,
    pub table_name: String,
    pub file_path: String,
    /// Columns with a NOT NULL constraint (read back with `force_not_null`).
    pub not_null_columns: Vec<String>,
}

/// `EXPORT DATABASE` (DuckDB's `LogicalExport`): its child (if any) copies the tables;
/// the operator then writes `schema.sql` and `load.sql`.
#[derive(Clone, Debug)]
pub struct LogicalExport {
    pub function: CopyFunction,
    pub file_path: String,
    pub format: String,
    /// The options of the generated `COPY ... FROM` statements.
    pub options: Vec<(String, Vec<rustdb_types::Value>)>,
    pub catalog: String,
    pub tables: Vec<ExportedTable>,
}
