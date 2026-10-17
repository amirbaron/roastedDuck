//! The options of the JSON reader (DuckDB's `json_reader_options.hpp` and the option
//! parsing of `json_multi_file_info.cpp`).

use rustdb_common::{Error, Result};
use rustdb_csv::options::FileCompressionType;
use rustdb_functions::strtime::StrpTimeFormat;
use rustdb_planner::ClientContext;
use rustdb_types::{LogicalType, LogicalTypeId, Value};

use crate::json_enums::{JsonFormat, JsonRecordType, JsonScanType};
use crate::json_functions::json_transform::string_to_logical_type;

/// `STANDARD_VECTOR_SIZE`.
pub(crate) const STANDARD_VECTOR_SIZE: usize = 2048;

/// `JSONReaderOptions`.
#[derive(Clone, Debug)]
#[allow(clippy::struct_excessive_bools, reason = "DuckDB's reader options")]
pub struct JsonReaderOptions {
    pub scan_type: JsonScanType,
    pub format: JsonFormat,
    pub record_type: JsonRecordType,
    pub compression: FileCompressionType,
    /// Malformed JSON becomes NULL (newline-delimited only) and conversion errors NULL.
    pub ignore_errors: bool,
    /// The largest JSON value (at least 16 MB).
    pub maximum_object_size: u64,
    pub auto_detect: bool,
    /// Values sampled per file for the schema.
    pub sample_size: u64,
    /// Nesting depth of the detected types (deeper is JSON).
    pub max_depth: u64,
    /// Objects whose fields appear less often (on average) become MAPs.
    pub field_appearance_threshold: f64,
    pub maximum_sample_files: u64,
    pub convert_strings_to_integers: bool,
    /// Objects with at least this many similar fields become MAPs.
    pub map_inference_threshold: u64,
    /// `columns`: the names and types.
    pub name_list: Vec<String>,
    pub sql_type_list: Vec<LogicalType>,
    pub date_format: String,
    pub timestamp_format: String,
}

impl Default for JsonReaderOptions {
    fn default() -> Self {
        Self {
            scan_type: JsonScanType::ReadJson,
            format: JsonFormat::AutoDetect,
            record_type: JsonRecordType::AutoDetect,
            compression: FileCompressionType::AutoDetect,
            ignore_errors: false,
            maximum_object_size: 16_777_216,
            auto_detect: false,
            sample_size: STANDARD_VECTOR_SIZE as u64 * 10,
            max_depth: u64::MAX,
            field_appearance_threshold: 0.1,
            maximum_sample_files: 32,
            convert_strings_to_integers: false,
            map_inference_threshold: 200,
            name_list: Vec::new(),
            sql_type_list: Vec::new(),
            date_format: String::new(),
            timestamp_format: String::new(),
        }
    }
}

/// `EnumUtil::FromString<FileCompressionType>` of an upper-cased name.
pub(crate) fn parse_compression(text: &str) -> Result<FileCompressionType> {
    let upper = text.to_uppercase();
    Ok(match upper.as_str() {
        "AUTO_DETECT" => FileCompressionType::AutoDetect,
        "UNCOMPRESSED" => FileCompressionType::Uncompressed,
        "GZIP" => FileCompressionType::Gzip,
        "ZSTD" => FileCompressionType::Zstd,
        _ => {
            return Err(rustdb_function::logging::enum_error(
                &upper,
                "FileCompressionType",
                &["AUTO_DETECT", "UNCOMPRESSED", "GZIP", "ZSTD"],
            ));
        }
    })
}

fn as_bool(value: &Value) -> bool {
    value.as_bool().unwrap_or(false)
}

fn as_i64(value: &Value) -> i64 {
    value.as_i128().map_or(0, |v| v as i64)
}

fn as_text(value: &Value) -> Result<String> {
    value.to_varchar()
}

impl JsonReaderOptions {
    /// `JSONMultiFileInfo::ParseOption`: whether `key` is a JSON reader option.
    pub fn parse_option(
        &mut self,
        context: Option<&ClientContext>,
        key: &str,
        value: &Value,
    ) -> Result<bool> {
        if value.is_null() {
            return Err(Error::binder(format!("Cannot use NULL as argument to key {key}")));
        }
        match key.to_lowercase().as_str() {
            "ignore_errors" => self.ignore_errors = as_bool(value),
            "maximum_object_size" => {
                let size = value.as_i128().map_or(0, |v| v as u64);
                self.maximum_object_size = size.max(self.maximum_object_size);
            }
            "format" => {
                let arg = as_text(value)?.to_lowercase();
                self.format = match arg.as_str() {
                    "auto" => JsonFormat::AutoDetect,
                    "unstructured" => JsonFormat::Unstructured,
                    "newline_delimited" | "nd" => JsonFormat::NewlineDelimited,
                    "array" => JsonFormat::Array,
                    _ => {
                        return Err(Error::binder(format!(
                            "format must be one of ['array', 'newline_delimited', 'unstructured', 'nd', 'auto'], not '{arg}'"
                        )));
                    }
                };
            }
            "compression" => self.compression = parse_compression(&as_text(value)?)?,
            "columns" => {
                let ty = value.logical_type();
                if ty.id() != LogicalTypeId::Struct {
                    return Err(Error::binder(
                        "read_json \"columns\" parameter requires a struct as input.",
                    ));
                }
                let names: Vec<String> = ty
                    .struct_children()
                    .unwrap_or_default()
                    .iter()
                    .map(|(name, _)| name.clone())
                    .collect();
                for (name, child) in names.iter().zip(value.children().unwrap_or_default()) {
                    if child.is_null() {
                        return Err(Error::binder(
                            "read_json \"columns\" parameter type specification cannot be NULL.",
                        ));
                    }
                    self.name_list.push(name.clone());
                    if child.logical_type().id() != LogicalTypeId::Varchar {
                        return Err(Error::binder(
                            "read_json \"columns\" parameter type specification must be VARCHAR.",
                        ));
                    }
                    self.sql_type_list
                        .push(string_to_logical_type(context, &as_text(child)?)?);
                }
                if self.name_list.is_empty() {
                    return Err(Error::binder(
                        "read_json \"columns\" parameter needs at least one column.",
                    ));
                }
            }
            "auto_detect" => self.auto_detect = as_bool(value),
            "sample_size" => {
                let arg = as_i64(value);
                if arg == -1 {
                    self.sample_size = u64::MAX;
                } else if arg > 0 {
                    self.sample_size = arg as u64;
                } else {
                    return Err(Error::binder(
                        "read_json \"sample_size\" parameter must be positive, or -1 to sample all input files entirely, up to \"maximum_sample_files\" files.",
                    ));
                }
            }
            "maximum_depth" => {
                let arg = as_i64(value);
                self.max_depth = if arg == -1 { u64::MAX } else { arg as u64 };
            }
            "field_appearance_threshold" => {
                let arg = value.as_f64().unwrap_or(0.0);
                if !(0.0..=1.0).contains(&arg) {
                    return Err(Error::binder(
                        "read_json_auto \"field_appearance_threshold\" parameter must be between 0 and 1",
                    ));
                }
                self.field_appearance_threshold = arg;
            }
            "map_inference_threshold" => {
                let arg = as_i64(value);
                if arg == -1 {
                    self.map_inference_threshold = u64::MAX;
                } else if arg >= 0 {
                    self.map_inference_threshold = arg as u64;
                } else {
                    return Err(Error::binder(
                        "read_json_auto \"map_inference_threshold\" parameter must be 0 or positive, or -1 to disable map inference for consistent objects.",
                    ));
                }
            }
            "dateformat" | "date_format" => {
                let mut format = as_text(value)?;
                if format.eq_ignore_ascii_case("iso") {
                    "%Y-%m-%d".clone_into(&mut format);
                }
                if let Err(error) = StrpTimeFormat::parse(&format) {
                    return Err(Error::binder(format!(
                        "read_json could not parse \"dateformat\": '{error}'."
                    )));
                }
                self.date_format = format;
            }
            "timestampformat" | "timestamp_format" => {
                let mut format = as_text(value)?;
                if format.eq_ignore_ascii_case("iso") {
                    "%Y-%m-%dT%H:%M:%S.%fZ".clone_into(&mut format);
                }
                if let Err(error) = StrpTimeFormat::parse(&format) {
                    return Err(Error::binder(format!(
                        "read_json could not parse \"timestampformat\": '{error}'."
                    )));
                }
                self.timestamp_format = format;
            }
            "records" => {
                self.record_type = match as_text(value)?.as_str() {
                    "auto" => JsonRecordType::AutoDetect,
                    "true" => JsonRecordType::Records,
                    "false" => JsonRecordType::Values,
                    _ => {
                        return Err(Error::binder(
                            "read_json requires \"records\" to be one of ['auto', 'true', 'false'].",
                        ));
                    }
                };
            }
            "maximum_sample_files" => {
                let arg = as_i64(value);
                if arg == -1 {
                    self.maximum_sample_files = u64::MAX;
                } else if arg > 0 {
                    self.maximum_sample_files = arg as u64;
                } else {
                    return Err(Error::binder(
                        "read_json \"maximum_sample_files\" parameter must be positive, or -1 to remove the limit on the number of files used to sample \"sample_size\" rows.",
                    ));
                }
            }
            "convert_strings_to_integers" => self.convert_strings_to_integers = as_bool(value),
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// `JSONMultiFileInfo::ParseCopyOption`: whether `key` is a `COPY ... FROM` option.
    pub fn parse_copy_option(&mut self, key: &str, values: &[Value]) -> Result<bool> {
        let single = |values: &[Value]| -> Result<Value> {
            match values {
                [value] => Ok(value.clone()),
                _ => Err(Error::binder(format!(
                    "COPY (FORMAT JSON) parameter {key} expects a single argument."
                ))),
            }
        };
        let to_bool = |value: &Value| -> Result<bool> {
            use rustdb_function::cast::ValueCast;
            Ok(value
                .default_cast_as(&LogicalType::BOOLEAN)?
                .as_bool()
                .unwrap_or(false))
        };
        match key.to_lowercase().as_str() {
            "dateformat" | "date_format" => self.date_format = as_text(&single(values)?)?,
            "timestampformat" | "timestamp_format" => self.timestamp_format = as_text(&single(values)?)?,
            "auto_detect" => {
                if values.is_empty() {
                    self.auto_detect = true;
                } else {
                    self.auto_detect = to_bool(&single(values)?)?;
                    if self.format == JsonFormat::AutoDetect {
                        self.format = JsonFormat::NewlineDelimited;
                    }
                }
            }
            "compression" => self.compression = parse_compression(&as_text(&single(values)?)?)?,
            "array" => {
                if values.is_empty() || to_bool(&single(values)?)? {
                    self.format = JsonFormat::Array;
                } else {
                    self.format = JsonFormat::NewlineDelimited;
                }
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
}
