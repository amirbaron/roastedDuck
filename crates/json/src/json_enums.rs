//! The enums of the JSON reader (DuckDB's `json_enums.hpp`).

/// What a scan produces (`JSONScanType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JsonScanType {
    /// JSON converted to columns (`read_json`).
    ReadJson,
    /// Each JSON value as text (`read_json_objects`).
    ReadJsonObjects,
}

/// Whether the values are objects whose keys are columns (`JSONRecordType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JsonRecordType {
    AutoDetect,
    /// Objects, unpacked into columns.
    Records,
    /// Any value, in one column.
    Values,
}

/// How values are laid out in a file (`JSONFormat`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JsonFormat {
    AutoDetect,
    /// One value after another; newlines anywhere.
    Unstructured,
    /// One value per line (NDJSON).
    NewlineDelimited,
    /// One top-level array of values.
    Array,
}
