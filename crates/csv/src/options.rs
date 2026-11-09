//! CSV reader and writer options (DuckDB's `csv_option.hpp`, `state_machine_options.hpp`,
//! `csv_reader_options.hpp/.cpp` and `ParseColumnList` of `bind_helpers.cpp`).

use std::collections::{BTreeMap, HashMap, HashSet};

use roastedduck_common::{CaseInsensitiveMap, Error, Result, STANDARD_VECTOR_SIZE};
use roastedduck_function::cast::ValueCast;
use roastedduck_functions::strtime::StrpTimeFormat;
use roastedduck_types::{LogicalType, LogicalTypeId, Value};

/// The newline of a CSV file (DuckDB's `NewLineIdentifier`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum NewLineIdentifier {
    /// `\n`
    SingleN = 1,
    /// `\r\n`
    CarryOn = 2,
    #[default]
    NotSet = 3,
    /// `\r`
    SingleR = 4,
}

/// How an option value is shown in error messages (`CSVOption::FormatValueInternal`).
pub trait FormatOption {
    fn format_option(&self) -> String;
}

impl FormatOption for String {
    fn format_option(&self) -> String {
        self.clone()
    }
}

impl FormatOption for u64 {
    fn format_option(&self) -> String {
        self.to_string()
    }
}

impl FormatOption for u8 {
    fn format_option(&self) -> String {
        if *self == 0 {
            return "(empty)".to_owned();
        }
        String::from_utf8_lossy(&[*self]).into_owned()
    }
}

impl FormatOption for bool {
    fn format_option(&self) -> String {
        if *self { "true" } else { "false" }.to_owned()
    }
}

impl FormatOption for NewLineIdentifier {
    fn format_option(&self) -> String {
        match self {
            Self::SingleN => "\\n",
            Self::SingleR => "\\r",
            Self::CarryOn => "\\r\\n",
            Self::NotSet => "Single-Line File",
        }
        .to_owned()
    }
}

impl FormatOption for StrpTimeFormat {
    fn format_option(&self) -> String {
        self.format_specifier().to_owned()
    }
}

/// An option that remembers whether the user set it (DuckDB's `CSVOption<T>`).
#[derive(Clone, Debug, Default)]
pub struct CsvOption<T> {
    set_by_user: bool,
    value: T,
}

impl<T> CsvOption<T> {
    pub const fn new(value: T) -> Self {
        Self {
            set_by_user: false,
            value,
        }
    }

    pub const fn with_user(value: T, set_by_user: bool) -> Self {
        Self { set_by_user, value }
    }

    /// `Set`: only changes a value the user did not set.
    pub fn set(&mut self, value: T, by_user: bool) {
        if !self.set_by_user {
            self.value = value;
            self.set_by_user = by_user;
        }
    }

    /// `Set(CSVOption)`.
    pub fn set_option(&mut self, other: Self, by_user: bool) {
        if !self.set_by_user {
            self.value = other.value;
            self.set_by_user = by_user;
        }
    }

    /// `ChangeSetByUserTrue`.
    pub fn change_set_by_user_true(&mut self) {
        self.set_by_user = true;
    }

    pub fn get(&self) -> &T {
        &self.value
    }

    pub fn is_set_by_user(&self) -> bool {
        self.set_by_user
    }

    /// `FormatSet`.
    pub fn format_set(&self) -> &'static str {
        if self.set_by_user {
            "(Set By User)"
        } else {
            "(Auto-Detected)"
        }
    }
}

impl<T: Copy> CsvOption<T> {
    pub fn value(&self) -> T {
        self.value
    }
}

impl<T: FormatOption> CsvOption<T> {
    /// `FormatValue`.
    pub fn format_value(&self) -> String {
        self.value.format_option()
    }
}

impl<T: PartialEq> PartialEq for CsvOption<T> {
    fn eq(&self, other: &Self) -> bool {
        self.value == other.value
    }
}

/// The options a state machine is built from (DuckDB's `CSVStateMachineOptions`).
#[derive(Clone, Debug)]
pub struct StateMachineOptions {
    pub delimiter: CsvOption<String>,
    pub quote: CsvOption<u8>,
    pub escape: CsvOption<u8>,
    pub comment: CsvOption<u8>,
    pub new_line: CsvOption<NewLineIdentifier>,
    pub strict_mode: CsvOption<bool>,
}

impl Default for StateMachineOptions {
    fn default() -> Self {
        Self {
            delimiter: CsvOption::new(",".to_owned()),
            quote: CsvOption::new(b'"'),
            escape: CsvOption::new(b'"'),
            comment: CsvOption::new(0),
            new_line: CsvOption::new(NewLineIdentifier::NotSet),
            strict_mode: CsvOption::new(true),
        }
    }
}

impl StateMachineOptions {
    pub fn new(
        delimiter: String,
        quote: u8,
        escape: u8,
        comment: u8,
        new_line: NewLineIdentifier,
        strict_mode: bool,
    ) -> Self {
        Self {
            delimiter: CsvOption::new(delimiter),
            quote: CsvOption::new(quote),
            escape: CsvOption::new(escape),
            comment: CsvOption::new(comment),
            new_line: CsvOption::new(new_line),
            strict_mode: CsvOption::new(strict_mode),
        }
    }

    /// The cache key of a state machine (the fields its transitions depend on).
    pub fn key(&self) -> (String, u8, u8, u8, NewLineIdentifier, bool) {
        (
            self.delimiter.get().clone(),
            self.quote.value(),
            self.escape.value(),
            self.comment.value(),
            self.new_line.value(),
            self.strict_mode.value(),
        )
    }
}

impl PartialEq for StateMachineOptions {
    fn eq(&self, other: &Self) -> bool {
        self.key() == other.key()
    }
}

/// The dialect of a file (DuckDB's `DialectOptions`).
#[derive(Clone, Debug)]
pub struct DialectOptions {
    pub state_machine_options: StateMachineOptions,
    /// Expected number of columns.
    pub num_cols: u64,
    pub header: CsvOption<bool>,
    /// The DATE and TIMESTAMP formats (if any).
    pub date_format: BTreeMap<FormatType, CsvOption<StrpTimeFormat>>,
    pub skip_rows: CsvOption<u64>,
    pub rows_until_header: u64,
}

/// The keys of [`DialectOptions::date_format`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FormatType {
    Date,
    Timestamp,
}

impl FormatType {
    pub fn of(id: LogicalTypeId) -> Option<Self> {
        match id {
            LogicalTypeId::Date => Some(Self::Date),
            LogicalTypeId::Timestamp => Some(Self::Timestamp),
            _ => None,
        }
    }

    pub fn type_id(self) -> LogicalTypeId {
        match self {
            Self::Date => LogicalTypeId::Date,
            Self::Timestamp => LogicalTypeId::Timestamp,
        }
    }
}

impl Default for DialectOptions {
    fn default() -> Self {
        let mut date_format = BTreeMap::new();
        date_format.insert(FormatType::Date, CsvOption::default());
        date_format.insert(FormatType::Timestamp, CsvOption::default());
        Self {
            state_machine_options: StateMachineOptions::default(),
            num_cols: 0,
            header: CsvOption::new(false),
            date_format,
            skip_rows: CsvOption::new(0),
            rows_until_header: 0,
        }
    }
}

impl DialectOptions {
    pub fn date_format(&self, ty: FormatType) -> &CsvOption<StrpTimeFormat> {
        &self.date_format[&ty]
    }

    pub fn date_format_mut(&mut self, ty: FormatType) -> &mut CsvOption<StrpTimeFormat> {
        self.date_format.entry(ty).or_default()
    }
}

/// How a file is compressed (DuckDB's `FileCompressionType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum FileCompressionType {
    #[default]
    AutoDetect,
    Uncompressed,
    Gzip,
    Zstd,
}

impl FileCompressionType {
    /// `FileCompressionTypeFromString`.
    pub fn from_string(input: &str) -> Result<Self> {
        Ok(match input.to_lowercase().as_str() {
            "infer" | "auto" => Self::AutoDetect,
            "gzip" => Self::Gzip,
            "zstd" => Self::Zstd,
            "uncompressed" | "none" | "" => Self::Uncompressed,
            _ => {
                return Err(Error::parser(format!(
                    "Unrecognized file compression type \"{input}\""
                )));
            }
        })
    }

    /// The compression of `path` under `self` (`VirtualFileSystem::OpenFileExtended`).
    #[must_use]
    pub fn resolve(self, path: impl AsRef<[u8]>) -> Self {
        if self != Self::AutoDetect {
            return self;
        }
        let path = path.as_ref();
        let path = path.split(|byte| *byte == b'?').next().unwrap_or(path);
        let extension = path
            .iter()
            .rposition(|byte| *byte == b'.')
            .map(|dot| &path[dot + 1..]);
        if extension.is_some_and(|extension| extension == b"gz") {
            Self::Gzip
        } else if extension.is_some_and(|extension| extension == b"zst") {
            Self::Zstd
        } else {
            Self::Uncompressed
        }
    }
}

/// `CSVBuffer::ROWS_PER_BUFFER`.
pub const ROWS_PER_BUFFER: u64 = 16;

/// All options of the CSV reader and writer (DuckDB's `CSVReaderOptions`).
#[derive(Clone, Debug)]
#[allow(clippy::struct_excessive_bools)]
pub struct CsvReaderOptions {
    pub dialect_options: DialectOptions,
    pub ignore_errors: CsvOption<bool>,
    pub store_rejects: CsvOption<bool>,
    pub rejects_table_name: CsvOption<String>,
    pub rejects_scan_name: CsvOption<String>,
    /// 0 = no limit.
    pub rejects_limit: u64,
    pub buffer_sample_size: u64,
    pub null_str: Vec<String>,
    pub compression: FileCompressionType,
    pub allow_quoted_nulls: bool,
    pub comment: u8,
    pub thousands_separator: u8,
    pub sql_types_per_column: CaseInsensitiveMap<usize, roastedduck_common::ByteString>,
    pub sql_type_list: Vec<LogicalType>,
    pub name_list: Vec<roastedduck_common::ByteString>,
    pub columns_set: bool,
    pub auto_type_candidates: Vec<LogicalType>,
    pub sniffer_user_mismatch_error: roastedduck_common::ByteString,
    pub was_type_manually_set: Vec<bool>,
    pub maximum_line_size: CsvOption<u64>,
    pub normalize_names: bool,
    pub force_not_null_names: HashSet<roastedduck_common::ByteString>,
    pub force_not_null: Vec<bool>,
    /// -1 means all.
    pub files_to_sniff: i64,
    pub sample_size_chunks: u64,
    pub all_varchar: bool,
    pub auto_detect: bool,
    pub file_path: roastedduck_common::ByteString,
    pub buffer_size_option: CsvOption<u64>,
    pub decimal_separator: String,
    pub null_padding: bool,
    pub parallel: bool,
    pub encoding: String,
    pub user_defined_parameters: BTreeMap<String, roastedduck_common::ByteString>,
    // Writer options.
    pub force_quote: Vec<bool>,
    pub prefix: String,
    pub suffix: String,
    pub write_newline: String,
    /// The DATE / TIMESTAMP / `TIMESTAMP_TZ` formats for writing.
    pub write_date_format: HashMap<LogicalTypeId, Value>,
    pub has_format: HashMap<LogicalTypeId, bool>,
    pub multi_file_reader: bool,
}

/// `max_line_size_default`.
pub const MAX_LINE_SIZE_DEFAULT: u64 = 2_000_000;
/// `CSVReaderOptions::sniff_size`.
pub const SNIFF_SIZE: u64 = 2048;

impl Default for CsvReaderOptions {
    fn default() -> Self {
        Self {
            dialect_options: DialectOptions::default(),
            ignore_errors: CsvOption::new(false),
            store_rejects: CsvOption::new(false),
            rejects_table_name: CsvOption::new("reject_errors".to_owned()),
            rejects_scan_name: CsvOption::new("reject_scans".to_owned()),
            rejects_limit: 0,
            buffer_sample_size: STANDARD_VECTOR_SIZE as u64 * 50,
            null_str: vec![String::new()],
            compression: FileCompressionType::AutoDetect,
            allow_quoted_nulls: true,
            comment: 0,
            thousands_separator: 0,
            sql_types_per_column: CaseInsensitiveMap::default(),
            sql_type_list: Vec::new(),
            name_list: Vec::new(),
            columns_set: false,
            auto_type_candidates: vec![
                LogicalType::VARCHAR,
                LogicalType::DOUBLE,
                LogicalType::BIGINT,
                LogicalType::TIMESTAMP_TZ,
                LogicalType::TIMESTAMP,
                LogicalType::DATE,
                LogicalType::TIME,
                LogicalType::BOOLEAN,
                LogicalType::SQLNULL,
            ],
            sniffer_user_mismatch_error: roastedduck_common::ByteString::new(),
            was_type_manually_set: Vec::new(),
            maximum_line_size: CsvOption::new(MAX_LINE_SIZE_DEFAULT),
            normalize_names: false,
            force_not_null_names: HashSet::new(),
            force_not_null: Vec::new(),
            files_to_sniff: 10,
            sample_size_chunks: 20480 / SNIFF_SIZE,
            all_varchar: false,
            auto_detect: true,
            file_path: roastedduck_common::ByteString::new(),
            buffer_size_option: CsvOption::new(ROWS_PER_BUFFER * MAX_LINE_SIZE_DEFAULT),
            decimal_separator: ".".to_owned(),
            null_padding: false,
            parallel: true,
            encoding: "utf-8".to_owned(),
            user_defined_parameters: BTreeMap::new(),
            force_quote: Vec::new(),
            prefix: String::new(),
            suffix: String::new(),
            write_newline: String::new(),
            write_date_format: HashMap::new(),
            has_format: HashMap::new(),
            multi_file_reader: false,
        }
    }
}

fn is_list(value: &Value) -> bool {
    value.logical_type().id() == LogicalTypeId::List
}

fn list_children(value: &Value) -> &[Value] {
    value.children().unwrap_or(&[])
}

/// `ParseBoolean(vector<Value>)`.
fn parse_boolean_list(set: &[Value], loption: &[u8]) -> Result<bool> {
    match set {
        [] => Ok(true),
        [value] => parse_boolean(value, loption),
        _ => Err(Error::binder(roastedduck_common::byte_format!(
            "\"{loption}\" expects a single argument as a boolean value (e.g. TRUE or 1)"
        ))),
    }
}

/// `ParseBoolean(Value)`.
pub(crate) fn parse_boolean(value: &Value, loption: &[u8]) -> Result<bool> {
    if value.is_null() {
        return Err(Error::binder(roastedduck_common::byte_format!(
            "\"{loption}\" expects a non-null boolean value (e.g. TRUE or 1)"
        )));
    }
    if is_list(value) {
        return parse_boolean_list(list_children(value), loption);
    }
    if matches!(
        value.logical_type().id(),
        LogicalTypeId::Float | LogicalTypeId::Double | LogicalTypeId::Decimal
    ) {
        return Err(Error::binder(roastedduck_common::byte_format!(
            "\"{loption}\" expects a boolean value (e.g. TRUE or 1)"
        )));
    }
    value
        .default_cast_as(&LogicalType::BOOLEAN)?
        .as_bool()
        .ok_or_else(|| {
            Error::binder(roastedduck_common::byte_format!(
                "\"{loption}\" expects a boolean value (e.g. TRUE or 1)"
            ))
        })
}

/// `ParseString`.
pub(crate) fn parse_string(value: &Value, loption: &[u8]) -> Result<String> {
    if value.is_null() {
        return Ok(String::new());
    }
    if is_list(value) {
        let children = list_children(value);
        if children.len() != 1 {
            return Err(Error::binder(roastedduck_common::byte_format!(
                "\"{loption}\" expects a single argument as a string value"
            )));
        }
        return parse_string(&children[0], loption);
    }
    if value.logical_type().id() != LogicalTypeId::Varchar {
        return Err(Error::binder(roastedduck_common::byte_format!(
            "\"{loption}\" expects a string argument!"
        )));
    }
    Ok(value.as_str().unwrap_or_default().to_owned())
}

/// `ParseInteger`.
pub(crate) fn parse_integer(value: &Value, loption: &[u8]) -> Result<i64> {
    if value.is_null() {
        return Err(Error::binder(roastedduck_common::byte_format!(
            "\"{loption}\" expects a non-null integer value"
        )));
    }
    if is_list(value) {
        let children = list_children(value);
        if children.len() != 1 {
            return Err(Error::binder(roastedduck_common::byte_format!(
                "\"{loption}\" expects a single argument as an integer value"
            )));
        }
        return parse_integer(&children[0], loption);
    }
    value
        .default_cast_as(&LogicalType::BIGINT)?
        .as_i64()
        .ok_or_else(|| Error::internal("integer option is not a BIGINT"))
}

/// `ParseColumnList(vector<Value>)`.
fn parse_column_list_values(
    set: &[Value],
    names: &[roastedduck_common::ByteString],
    loption: &[u8],
) -> Result<Vec<bool>> {
    if set.is_empty() {
        return Err(Error::binder(roastedduck_common::byte_format!(
            "\"{loption}\" expects a column list or * as parameter"
        )));
    }
    let mut option_map: Vec<(roastedduck_common::ByteString, bool)> = Vec::new();
    for value in set {
        let text = value.to_varchar_bytes()?;
        if !option_map
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case(&text))
        {
            option_map.push((text, false));
        }
    }
    let mut result = vec![false; names.len()];
    for (index, name) in names.iter().enumerate() {
        if let Some(entry) = option_map
            .iter_mut()
            .find(|(option, _)| option.eq_ignore_ascii_case(name))
        {
            result[index] = true;
            entry.1 = true;
        }
    }
    if let Some((missing, _)) = option_map.iter().find(|(_, found)| !found) {
        return Err(Error::binder(roastedduck_common::byte_format!(
            "\"{loption}\" expected to find {missing}, but it was not found in the table"
        )));
    }
    Ok(result)
}

/// `ParseColumnList(Value)`.
pub fn parse_column_list(
    value: &Value,
    names: &[roastedduck_common::ByteString],
    loption: &[u8],
) -> Result<Vec<bool>> {
    if !is_list(value) {
        if value.logical_type().id() == LogicalTypeId::Varchar && value.as_str() == Some("*") {
            return Ok(vec![true; names.len()]);
        }
        return Err(Error::binder(roastedduck_common::byte_format!(
            "\"{loption}\" expects a column list or * as parameter"
        )));
    }
    if value.is_null() {
        return Err(Error::binder(roastedduck_common::byte_format!(
            "\"{loption}\" expects a column list or * as parameter, it can't be a NULL value"
        )));
    }
    let children = list_children(value);
    if children.len() == 1
        && children[0].logical_type().id() == LogicalTypeId::Varchar
        && children[0].as_str() == Some("*")
    {
        return Ok(vec![true; names.len()]);
    }
    parse_column_list_values(children, names, loption)
}

/// The specificity of an auto-detection candidate (`GetCandidateSpecificity`).
fn candidate_specificity(ty: &LogicalType) -> Result<u8> {
    Ok(match ty.id() {
        LogicalTypeId::Varchar => 0,
        LogicalTypeId::Double => 1,
        LogicalTypeId::Float => 2,
        LogicalTypeId::Decimal => 3,
        LogicalTypeId::BigInt => 4,
        LogicalTypeId::Integer => 5,
        LogicalTypeId::SmallInt => 6,
        LogicalTypeId::TinyInt => 7,
        LogicalTypeId::TimestampTz => 8,
        LogicalTypeId::Timestamp => 9,
        LogicalTypeId::Date => 10,
        LogicalTypeId::Time => 11,
        LogicalTypeId::Boolean => 12,
        LogicalTypeId::SqlNull => 13,
        other => {
            return Err(Error::binder(roastedduck_common::byte_format!(
                "Auto Type Candidate of type {} is not accepted as a valid input",
                other.name()
            )));
        }
    })
}

/// `StoreUserDefinedParameter`.
fn store_user_defined_parameter(option: &str) -> bool {
    !matches!(
        option,
        "column_types" | "types" | "dtypes" | "auto_detect" | "auto_type_candidates" | "columns" | "names"
    )
}

/// The multi-file options that interact with CSV options (DuckDB's `MultiFileOptions`).
#[derive(Clone, Debug, Default)]
pub struct MultiFileOptionsView {
    pub union_by_name: bool,
    pub hive_partitioning: bool,
    /// Whether a `filename` column is added.
    pub filename: bool,
}

impl MultiFileOptionsView {
    /// `AnySet`.
    pub fn any_set(&self) -> bool {
        self.filename || self.hive_partitioning || self.union_by_name
    }
}

impl CsvReaderOptions {
    pub fn header(&self) -> bool {
        self.dialect_options.header.value()
    }

    pub fn set_header(&mut self, header: bool) {
        self.dialect_options.header.set(header, true);
    }

    pub fn set_compression(&mut self, compression: &str) -> Result<()> {
        self.compression = FileCompressionType::from_string(compression)?;
        Ok(())
    }

    pub fn escape(&self) -> String {
        byte_string(self.dialect_options.state_machine_options.escape.value())
    }

    pub fn set_escape(&mut self, input: &str) -> Result<()> {
        let byte = single_byte(input, "escape")?;
        self.dialect_options.state_machine_options.escape.set(byte, true);
        Ok(())
    }

    pub fn skip_rows(&self) -> u64 {
        self.dialect_options.skip_rows.value()
    }

    pub fn set_skip_rows(&mut self, rows: i64) -> Result<()> {
        if rows < 0 {
            return Err(Error::invalid_input(
                "skip_rows option from read_csv scanner, must be equal or higher than 0",
            ));
        }
        self.dialect_options.skip_rows.set(rows as u64, true);
        Ok(())
    }

    pub fn delimiter(&self) -> &str {
        self.dialect_options.state_machine_options.delimiter.get()
    }

    pub fn set_delimiter(&mut self, input: &str) -> Result<()> {
        let delimiter = input.replace("\\t", "\t");
        if delimiter.len() > 4 {
            return Err(Error::invalid_input(
                "The delimiter option cannot exceed a size of 4 bytes.",
            ));
        }
        if self
            .dialect_options
            .state_machine_options
            .delimiter
            .is_set_by_user()
        {
            return Err(Error::binder(
                "CSV Reader function option delim and sep are aliases, only one can be supplied",
            ));
        }
        self.dialect_options
            .state_machine_options
            .delimiter
            .set(delimiter, true);
        Ok(())
    }

    pub fn quote(&self) -> String {
        byte_string(self.dialect_options.state_machine_options.quote.value())
    }

    pub fn set_quote(&mut self, input: &str) -> Result<()> {
        let byte = single_byte(input, "quote")?;
        self.dialect_options.state_machine_options.quote.set(byte, true);
        Ok(())
    }

    pub fn comment_str(&self) -> String {
        byte_string(self.dialect_options.state_machine_options.comment.value())
    }

    pub fn set_comment(&mut self, input: &str) -> Result<()> {
        let byte = single_byte(input, "comment")?;
        self.dialect_options.state_machine_options.comment.set(byte, true);
        Ok(())
    }

    /// `GetNewline`.
    pub fn newline(&self) -> &'static str {
        match self.dialect_options.state_machine_options.new_line.value() {
            NewLineIdentifier::CarryOn => "\\r\\n",
            NewLineIdentifier::SingleR => "\\r",
            NewLineIdentifier::SingleN => "\\n",
            NewLineIdentifier::NotSet => "",
        }
    }

    pub fn set_newline(&mut self, input: &str) -> Result<()> {
        let new_line = match input {
            "\\n" => NewLineIdentifier::SingleN,
            "\\r" => NewLineIdentifier::SingleR,
            "\\r\\n" => NewLineIdentifier::CarryOn,
            _ => {
                return Err(Error::invalid_input(format!(
                    "This is not accepted as a newline: {input}"
                )));
            }
        };
        self.dialect_options
            .state_machine_options
            .new_line
            .set(new_line, true);
        Ok(())
    }

    pub fn rfc4180(&self) -> bool {
        self.dialect_options.state_machine_options.strict_mode.value()
    }

    pub fn set_rfc4180(&mut self, input: bool) {
        self.dialect_options
            .state_machine_options
            .strict_mode
            .set(input, true);
    }

    /// `IgnoreErrors`: errors are ignored and not stored in a rejects table.
    pub fn ignore_errors_only(&self) -> bool {
        self.ignore_errors.value() && !self.store_rejects.value()
    }

    /// `GetSingleByteDelimiter`.
    pub fn single_byte_delimiter(&self) -> CsvOption<u8> {
        let delimiter = &self.dialect_options.state_machine_options.delimiter;
        CsvOption::with_user(
            delimiter.get().as_bytes().first().copied().unwrap_or(0),
            delimiter.is_set_by_user(),
        )
    }

    /// `SetDateFormat`.
    pub fn set_date_format(&mut self, ty: LogicalTypeId, format: &str, read_format: bool) -> Result<()> {
        if format.eq_ignore_ascii_case("auto") {
            return Ok(());
        }
        if read_format {
            let parsed = StrpTimeFormat::parse(format)
                .map_err(|error| Error::invalid_input(format!("Could not parse DATEFORMAT: {error}")))?;
            if let Some(key) = FormatType::of(ty) {
                self.dialect_options.date_format_mut(key).set(parsed, true);
            }
        } else {
            self.write_date_format.insert(ty, Value::varchar(format));
        }
        Ok(())
    }

    /// `SetReadOption`.
    pub fn set_read_option(
        &mut self,
        loption: &[u8],
        value: &Value,
        expected_names: &[roastedduck_common::ByteString],
    ) -> Result<()> {
        if self.set_base_option(loption, value, false)? {
            return Ok(());
        }
        match loption {
            b"auto_detect" => self.auto_detect = parse_boolean(value, loption)?,
            b"sample_size" => {
                let sample_size = parse_integer(value, loption)?;
                if sample_size < 1 && sample_size != -1 {
                    return Err(Error::binder(
                        "Unsupported parameter for SAMPLE_SIZE: cannot be smaller than 1",
                    ));
                }
                if sample_size == -1 {
                    self.sample_size_chunks = u64::MAX;
                } else {
                    let vector = STANDARD_VECTOR_SIZE as i64;
                    self.sample_size_chunks = (sample_size / vector) as u64;
                    if sample_size % vector != 0 {
                        self.sample_size_chunks += 1;
                    }
                }
            }
            b"skip" => self.set_skip_rows(parse_integer(value, loption)?)?,
            b"max_line_size" | b"maximum_line_size" => {
                let line_size = parse_integer(value, loption)?;
                if line_size < 0 {
                    return Err(Error::binder(
                        "Invalid value for MAX_LINE_SIZE parameter: it cannot be smaller than 0",
                    ));
                }
                self.maximum_line_size.set(line_size as u64, true);
                if self.buffer_size_option.is_set_by_user()
                    && self.maximum_line_size.value() > self.buffer_size_option.value()
                {
                    return Err(Error::invalid_input(roastedduck_common::byte_format!(
                        "Buffer Size of {} must be a higher value than the maximum line size {}",
                        self.buffer_size_option.value(),
                        self.maximum_line_size.value()
                    )));
                }
            }
            b"date_format" | b"dateformat" => {
                let format = parse_string(value, loption)?;
                self.set_date_format(LogicalTypeId::Date, &format, true)?;
            }
            b"timestamp_format" | b"timestampformat" => {
                let format = parse_string(value, loption)?;
                self.set_date_format(LogicalTypeId::Timestamp, &format, true)?;
            }
            b"ignore_errors" => self.ignore_errors.set(parse_boolean(value, loption)?, true),
            b"buffer_size" => {
                self.buffer_size_option
                    .set(parse_integer(value, loption)? as u64, true);
                if self.buffer_size_option.value() == 0 {
                    return Err(Error::invalid_input("Buffer Size option must be higher than 0"));
                }
                if self.maximum_line_size.is_set_by_user()
                    && self.maximum_line_size.value() > self.buffer_size_option.value()
                {
                    return Err(Error::invalid_input(roastedduck_common::byte_format!(
                        "Buffer Size of {} must be a higher value than the maximum line size {}",
                        self.buffer_size_option.value(),
                        self.maximum_line_size.value()
                    )));
                }
                self.maximum_line_size.set(self.buffer_size_option.value(), false);
            }
            b"decimal_separator" => {
                self.decimal_separator = parse_string(value, loption)?;
                if self.decimal_separator != "." && self.decimal_separator != "," {
                    return Err(Error::binder(
                        "Unsupported parameter for DECIMAL_SEPARATOR: should be '.' or ','",
                    ));
                }
            }
            b"null_padding" => self.null_padding = parse_boolean(value, loption)?,
            b"parallel" => self.parallel = parse_boolean(value, loption)?,
            b"allow_quoted_nulls" => self.allow_quoted_nulls = parse_boolean(value, loption)?,
            b"store_rejects" => self.store_rejects.set(parse_boolean(value, loption)?, true),
            b"force_not_null" => {
                if expected_names.is_empty() {
                    if value.is_null() {
                        return Err(Error::binder("Invalid value for 'force_not_null' paramenter"));
                    }
                    for child in list_children(value) {
                        self.force_not_null_names.insert(child.to_varchar_bytes()?);
                    }
                } else {
                    self.force_not_null = parse_column_list(value, expected_names, loption)?;
                }
            }
            b"rejects_table" => {
                let table_name = parse_string(value, loption)?;
                if table_name.is_empty() {
                    return Err(Error::binder("REJECTS_TABLE option cannot be empty"));
                }
                if roastedduck_ast::keyword::requires_quotes(
                    &table_name,
                    true,
                    roastedduck_ast::keyword::KeywordCategory::None,
                ) {
                    return Err(Error::binder(roastedduck_common::byte_format!(
                        "rejects_scan option: {table_name} requires quotes to be used as an identifier"
                    )));
                }
                self.rejects_table_name.set(table_name, true);
            }
            b"rejects_scan" => {
                let table_name = parse_string(value, loption)?;
                if table_name.is_empty() {
                    return Err(Error::binder("rejects_scan option cannot be empty"));
                }
                if roastedduck_ast::keyword::requires_quotes(
                    &table_name,
                    true,
                    roastedduck_ast::keyword::KeywordCategory::None,
                ) {
                    return Err(Error::binder(roastedduck_common::byte_format!(
                        "rejects_scan option: {table_name} requires quotes to be used as an identifier"
                    )));
                }
                self.rejects_scan_name.set(table_name, true);
            }
            b"rejects_limit" => {
                let limit = parse_integer(value, loption)?;
                if limit < 0 {
                    return Err(Error::binder(
                        "Unsupported parameter for REJECTS_LIMIT: cannot be negative",
                    ));
                }
                self.rejects_limit = limit as u64;
            }
            b"encoding" => self.encoding = parse_string(value, loption)?,
            b"thousands" => {
                if !value.is_null() {
                    let separator = parse_string(value, loption)?;
                    if separator.len() > 1 {
                        return Err(Error::binder(
                            "Unsupported parameter for THOUSANDS: should be max one character",
                        ));
                    }
                    if let Some(&byte) = separator.as_bytes().first() {
                        self.thousands_separator = byte;
                    }
                }
            }
            _ => {
                return Err(Error::binder(roastedduck_common::byte_format!(
                    "Unrecognized option for CSV reader \"{loption}\""
                )));
            }
        }
        Ok(())
    }

    /// `SetWriteOption`.
    pub fn set_write_option(&mut self, loption: &[u8], value: &Value) -> Result<()> {
        if loption == b"new_line" {
            self.write_newline = parse_string(value, loption)?;
            return Ok(());
        }
        if self.set_base_option(loption, value, true)? {
            return Ok(());
        }
        match loption {
            b"force_quote" => {
                let names = self.name_list.clone();
                self.force_quote = parse_column_list(value, &names, loption)?;
            }
            b"date_format" | b"dateformat" => {
                let format = parse_string(value, loption)?;
                self.set_date_format(LogicalTypeId::Date, &format, false)?;
            }
            b"timestamp_format" | b"timestampformat" => {
                let mut format = parse_string(value, loption)?;
                if format.eq_ignore_ascii_case("iso") {
                    "%Y-%m-%dT%H:%M:%S.%fZ".clone_into(&mut format);
                }
                self.set_date_format(LogicalTypeId::Timestamp, &format, false)?;
                self.set_date_format(LogicalTypeId::TimestampTz, &format, false)?;
            }
            b"prefix" => self.prefix = parse_string(value, loption)?,
            b"suffix" => self.suffix = parse_string(value, loption)?,
            _ => {
                return Err(Error::binder(roastedduck_common::byte_format!(
                    "Unrecognized option CSV writer \"{loption}\""
                )));
            }
        }
        Ok(())
    }

    /// `SetBaseOption`: options of both the reader and the writer.
    pub fn set_base_option(&mut self, loption: &[u8], value: &Value, write_option: bool) -> Result<bool> {
        if loption.starts_with(b"delim") || loption.starts_with(b"sep") {
            self.set_delimiter(&parse_string(value, loption)?)?;
        } else if loption == b"quote" {
            self.set_quote(&parse_string(value, loption)?)?;
        } else if loption == b"comment" {
            self.set_comment(&parse_string(value, loption)?)?;
        } else if loption == b"new_line" {
            self.set_newline(&parse_string(value, loption)?)?;
        } else if loption == b"escape" {
            self.set_escape(&parse_string(value, loption)?)?;
        } else if loption == b"header" {
            self.set_header(parse_boolean(value, loption)?);
        } else if loption == b"nullstr" || loption == b"null" {
            self.null_str.clear();
            let ty = value.logical_type();
            if ty.id() != LogicalTypeId::List && ty.id() != LogicalTypeId::Varchar {
                return Err(Error::binder(roastedduck_common::byte_format!(
                    "CSV Reader function option {loption} requires a string or a list as input"
                )));
            }
            if ty.id() == LogicalTypeId::List {
                let list_child = ty.child_type().cloned().unwrap_or_default();
                let mut children = list_children(value);
                if list_child.id() == LogicalTypeId::List {
                    let grandchild = list_child.child_type().cloned().unwrap_or_default();
                    let outer = list_children(value);
                    if grandchild.id() != LogicalTypeId::Varchar || outer.len() != 1 {
                        return Err(Error::binder(roastedduck_common::byte_format!(
                            "CSV Reader function option {loption} requires a non-empty list of possible null strings (varchar) as input"
                        )));
                    }
                    children = list_children(&outer[outer.len() - 1]);
                } else if list_child.id() != LogicalTypeId::Varchar {
                    return Err(Error::binder(roastedduck_common::byte_format!(
                        "CSV Reader function option {loption} requires a non-empty list of possible null strings (varchar) as input"
                    )));
                }
                for child in children {
                    if child.is_null() {
                        return Err(Error::binder(roastedduck_common::byte_format!(
                            "CSV Reader function option {loption} does not accept NULL values as a valid nullstr option"
                        )));
                    }
                    self.null_str.push(child.as_str().unwrap_or_default().to_owned());
                }
            } else {
                self.null_str.push(parse_string(value, loption)?);
            }
            if self.null_str.len() > 1 && write_option {
                return Err(Error::binder(roastedduck_common::byte_format!(
                    "CSV Writer function option {loption} only accepts one nullstr value."
                )));
            }
        } else if loption == b"compression" {
            self.set_compression(&parse_string(value, loption)?)?;
        } else if loption == b"strict_mode" {
            self.set_rfc4180(parse_boolean(value, loption)?);
        } else {
            return Ok(false);
        }
        Ok(true)
    }

    /// `WasTypeManuallySet`.
    pub fn was_type_manually_set(&self, index: usize) -> bool {
        self.was_type_manually_set.get(index).copied().unwrap_or(false)
    }

    /// `ToString`: the options shown in error messages.
    pub fn to_display_string(&self, current_file_path: &[u8]) -> roastedduck_common::ByteString {
        fn line<T: FormatOption>(name: &str, option: &CsvOption<T>) -> String {
            format!("{name} = {} {}\n  ", option.format_value(), option.format_set())
        }
        let sm = &self.dialect_options.state_machine_options;
        let mut error = roastedduck_common::byte_format!("  file = {current_file_path}\n  ");
        error.push_bytes(line("delimiter", &sm.delimiter));
        error.push_bytes(line("quote", &sm.quote));
        error.push_bytes(line("escape", &sm.escape));
        error.push_bytes(line("new_line", &sm.new_line));
        error.push_bytes(line("header", &self.dialect_options.header));
        error.push_bytes(line("skip_rows", &self.dialect_options.skip_rows));
        error.push_bytes(line("comment", &sm.comment));
        error.push_bytes(line("strict_mode", &sm.strict_mode));
        error.push_bytes(line(
            "date_format",
            self.dialect_options.date_format(FormatType::Date),
        ));
        error.push_bytes(line(
            "timestamp_format",
            self.dialect_options.date_format(FormatType::Timestamp),
        ));
        let _ = roastedduck_common::byte_write!(error, "null_padding = {}\n  ", u8::from(self.null_padding));
        let _ = roastedduck_common::byte_write!(
            error,
            "sample_size = {}\n  ",
            self.sample_size_chunks.wrapping_mul(STANDARD_VECTOR_SIZE as u64)
        );
        let _ = roastedduck_common::byte_write!(
            error,
            "ignore_errors = {}\n  ",
            self.ignore_errors.format_value()
        );
        let _ = roastedduck_common::byte_write!(error, "all_varchar = {}\n", u8::from(self.all_varchar));
        error.push_bytes(&self.sniffer_user_mismatch_error);
        error
    }

    /// `Verify`.
    pub fn verify(&mut self, file_options: &MultiFileOptionsView) -> Result<()> {
        if self.rejects_table_name.is_set_by_user()
            && !self.store_rejects.value()
            && self.store_rejects.is_set_by_user()
        {
            return Err(Error::binder(
                "REJECTS_TABLE option is only supported when store_rejects is not manually set to false",
            ));
        }
        if self.rejects_scan_name.is_set_by_user()
            && !self.store_rejects.value()
            && self.store_rejects.is_set_by_user()
        {
            return Err(Error::binder(
                "REJECTS_SCAN option is only supported when store_rejects is not manually set to false",
            ));
        }
        if self.rejects_scan_name.is_set_by_user() || self.rejects_table_name.is_set_by_user() {
            self.store_rejects.set(true, false);
        }
        if self.store_rejects.value() {
            if !self.ignore_errors.value() && self.ignore_errors.is_set_by_user() {
                return Err(Error::binder(
                    "STORE_REJECTS option is only supported when IGNORE_ERRORS is not manually set to false",
                ));
            }
            self.ignore_errors.set(true, false);
            if file_options.union_by_name {
                return Err(Error::binder(
                    "REJECTS_TABLE option is not supported when UNION_BY_NAME is set to true",
                ));
            }
        }
        if self.rejects_limit != 0 && !self.store_rejects.value() {
            return Err(Error::binder(
                "REJECTS_LIMIT option is only supported when REJECTS_TABLE is set to a table name",
            ));
        }
        if self.buffer_size_option.is_set_by_user() && self.maximum_line_size.is_set_by_user() {
            if self.buffer_size_option.value() < self.maximum_line_size.value() {
                return Err(Error::binder(format!(
                    "BUFFER_SIZE option was set to {}, while MAX_LINE_SIZE was set to {}. BUFFER_SIZE must have always be set to value bigger than MAX_LINE_SIZE",
                    self.buffer_size_option.value(),
                    self.maximum_line_size.value()
                )));
            }
        } else if self.maximum_line_size.is_set_by_user()
            && self.maximum_line_size.value() > MAX_LINE_SIZE_DEFAULT
        {
            self.buffer_size_option
                .set(ROWS_PER_BUFFER * self.maximum_line_size.value(), false);
        }
        Ok(())
    }

    /// `BaseCSVData::Finalize`: rejects option combinations that cannot be parsed.
    pub fn finalize(&self) -> Result<()> {
        fn clash(name_1: &str, name_2: &str) -> Error {
            Error::binder(format!(
                "{name_1} must not appear in the {name_2} specification and vice versa"
            ))
        }
        fn options_equal(a: u8, b: u8, name_1: &str, name_2: &str) -> Result<()> {
            if a != 0 && b != 0 && a == b {
                return Err(clash(name_1, name_2));
            }
            Ok(())
        }
        fn substring(a: u8, b: &str, name_1: &str, name_2: &str) -> Result<()> {
            if a != 0 && !b.is_empty() && b.as_bytes().contains(&a) {
                return Err(clash(name_1, name_2));
            }
            Ok(())
        }
        let sm = &self.dialect_options.state_machine_options;
        let delimiter = sm.delimiter.get();
        substring(sm.quote.value(), delimiter, "QUOTE", "DELIMITER")?;
        substring(sm.escape.value(), delimiter, "ESCAPE", "DELIMITER")?;
        if sm.quote.value() != sm.escape.value() {
            options_equal(sm.quote.value(), sm.escape.value(), "QUOTE", "ESCAPE")?;
        }
        options_equal(sm.comment.value(), sm.quote.value(), "COMMENT", "QUOTE")?;
        substring(sm.comment.value(), delimiter, "COMMENT", "DELIMITER")?;
        substring(
            self.thousands_separator,
            &self.decimal_separator,
            "THOUSANDS",
            "DECIMAL_SEPARATOR",
        )?;
        for null_str in &self.null_str {
            if null_str.is_empty() {
                continue;
            }
            if !delimiter.is_empty() && null_str.contains(delimiter.as_str()) {
                return Err(clash("DELIMITER", "NULL"));
            }
            substring(sm.quote.value(), null_str, "QUOTE", "NULL")?;
            let escape = sm.escape.value();
            let bytes = null_str.as_bytes();
            if !sm.strict_mode.value() && bytes.len() == 2 && bytes[0] == escape && bytes[1] != 0 {
                continue;
            }
            substring(escape, null_str, "ESCAPE", "NULL")?;
        }
        if !self.prefix.is_empty() || !self.suffix.is_empty() {
            if self.prefix.is_empty() || self.suffix.is_empty() {
                return Err(Error::binder(
                    "COPY ... (FORMAT CSV) must have both PREFIX and SUFFIX, or none at all",
                ));
            }
            if self.dialect_options.header.value() {
                return Err(Error::binder(
                    "COPY ... (FORMAT CSV)'s HEADER cannot be combined with PREFIX/SUFFIX",
                ));
            }
        }
        Ok(())
    }

    /// `GetUserDefinedParameters`.
    pub fn user_defined_parameters_string(&self) -> roastedduck_common::ByteString {
        roastedduck_common::ByteString::join(
            &self
                .user_defined_parameters
                .iter()
                .map(|(key, value)| roastedduck_common::byte_format!("{key}={value}"))
                .collect::<Vec<_>>(),
            b", ",
        )
    }

    /// `ParseOption`: one `read_csv` named parameter (multi-file options are handled
    /// before). `parse_type` turns a type name into a type (`TransformStringToLogicalType`).
    pub fn parse_option(
        &mut self,
        key: impl AsRef<[u8]>,
        value: &Value,
        parse_type: &dyn Fn(&str) -> Result<LogicalType>,
    ) -> Result<()> {
        let key = key.as_ref();
        let key = std::str::from_utf8(key)
            .ok()
            .filter(|key| key.is_ascii())
            .ok_or_else(|| {
                Error::binder(roastedduck_common::byte_format!(
                    "Unrecognized option for CSV reader \"{}\"",
                    key.to_ascii_lowercase()
                ))
            })?;
        let loption = key.to_ascii_lowercase();
        if store_user_defined_parameter(&loption) {
            self.user_defined_parameters
                .insert(loption.clone(), roastedduck_ast::value_to_sql_string(value));
        }
        match loption.as_str() {
            "columns" => {
                if !self.name_list.is_empty() {
                    return Err(Error::binder(
                        "read_csv column_names/names can only be supplied once",
                    ));
                }
                self.columns_set = true;
                let ty = value.logical_type().clone();
                if ty.id() != LogicalTypeId::Struct {
                    return Err(Error::binder("read_csv columns requires a struct as input"));
                }
                let fields = ty.struct_children().unwrap_or_default();
                let children = value.children().unwrap_or(&[]);
                for (index, ((name, _), child)) in fields.iter().zip(children).enumerate() {
                    self.name_list.push(name.clone());
                    if child.logical_type().id() != LogicalTypeId::Varchar {
                        return Err(Error::binder("read_csv requires a type specification as string"));
                    }
                    self.sql_types_per_column.insert(name.clone(), index);
                    self.sql_type_list
                        .push(parse_type(child.as_str().unwrap_or_default())?);
                }
                if self.name_list.is_empty() {
                    return Err(Error::binder(
                        "read_csv requires at least a single column as input!",
                    ));
                }
            }
            "auto_type_candidates" => {
                self.auto_type_candidates.clear();
                let mut candidates: BTreeMap<u8, LogicalType> = BTreeMap::new();
                candidates.insert(
                    candidate_specificity(&LogicalType::VARCHAR)?,
                    LogicalType::VARCHAR,
                );
                candidates.insert(
                    candidate_specificity(&LogicalType::SQLNULL)?,
                    LogicalType::SQLNULL,
                );
                if value.logical_type().id() != LogicalTypeId::List {
                    return Err(Error::binder("read_csv auto_types requires a list as input"));
                }
                let children = list_children(value);
                if children.is_empty() {
                    return Err(Error::binder("auto_type_candidates requires at least one type"));
                }
                for child in children {
                    if child.logical_type().id() != LogicalTypeId::Varchar {
                        return Err(Error::binder(
                            "auto_type_candidates requires a type specification as string",
                        ));
                    }
                    let candidate = parse_type(child.as_str().unwrap_or_default())?;
                    candidates.insert(candidate_specificity(&candidate)?, candidate);
                }
                self.auto_type_candidates = candidates.into_values().collect();
            }
            "column_names" | "names" => {
                if !self.name_list.is_empty() {
                    return Err(Error::binder(
                        "read_csv column_names/names can only be supplied once",
                    ));
                }
                if value.is_null() {
                    return Err(Error::binder(format!("read_csv {key} cannot be NULL")));
                }
                for child in list_children(value) {
                    if child.is_null() {
                        return Err(Error::binder(format!(
                            "read_csv {key} parameter cannot have a NULL value"
                        )));
                    }
                    self.name_list.push(child.to_varchar_bytes()?);
                }
                let mut seen = HashSet::new();
                for name in &self.name_list {
                    if name.iter().all(u8::is_ascii_whitespace) {
                        return Err(Error::binder(format!(
                            "read_csv {key} cannot have empty (or all whitespace) value"
                        )));
                    }
                    if !seen.insert(name.clone()) {
                        return Err(Error::binder(roastedduck_common::byte_format!(
                            "read_csv {key} must have unique values. \"{name}\" is repeated."
                        )));
                    }
                }
            }
            "column_types" | "types" | "dtypes" => {
                let ty = value.logical_type().clone();
                if ty.id() != LogicalTypeId::Struct && ty.id() != LogicalTypeId::List {
                    return Err(Error::binder(format!(
                        "read_csv {key} requires a struct or list as input"
                    )));
                }
                if !self.sql_type_list.is_empty() {
                    return Err(Error::binder(
                        "read_csv column_types/types/dtypes can only be supplied once",
                    ));
                }
                let mut names = Vec::new();
                if ty.id() == LogicalTypeId::Struct {
                    let fields = ty.struct_children().unwrap_or_default();
                    let children = value.children().unwrap_or(&[]);
                    for (index, ((name, _), child)) in fields.iter().zip(children).enumerate() {
                        if child.logical_type().id() != LogicalTypeId::Varchar {
                            return Err(Error::binder(format!(
                                "read_csv {key} requires a type specification as string"
                            )));
                        }
                        names.push(child.as_str().unwrap_or_default().to_owned());
                        self.sql_types_per_column.insert(name.clone(), index);
                    }
                } else {
                    let child = ty.child_type().cloned().unwrap_or_default();
                    if child.id() != LogicalTypeId::Varchar {
                        return Err(Error::binder(format!(
                            "read_csv {key} requires a list of types (varchar) as input"
                        )));
                    }
                    for child in list_children(value) {
                        names.push(child.as_str().unwrap_or_default().to_owned());
                    }
                }
                for name in names {
                    let ty = parse_type(&name)?;
                    if ty.id() == LogicalTypeId::Unbound {
                        return Err(Error::binder(format!(
                            "Unrecognized type \"{name}\" for read_csv {key} definition"
                        )));
                    }
                    self.sql_type_list.push(ty);
                }
            }
            "all_varchar" => self.all_varchar = boolean_value(loption.as_bytes(), value)?,
            "files_to_sniff" => {
                self.files_to_sniff = parse_integer(value, loption.as_bytes())?;
                if self.files_to_sniff < 1 && self.files_to_sniff != -1 {
                    return Err(Error::binder(
                        "Unsupported parameter for files_to_sniff: value must be -1 for all files or higher than one.",
                    ));
                }
            }
            "normalize_names" => self.normalize_names = boolean_value(loption.as_bytes(), value)?,
            _ => {
                let names = self.name_list.clone();
                self.set_read_option(loption.as_bytes(), value, &names)?;
            }
        }
        Ok(())
    }

    /// `ToNamedParameters`: the options the sniffer settled on, as `read_csv` parameters.
    pub fn to_named_parameters(&self) -> Result<Vec<(String, Value)>> {
        let sm = &self.dialect_options.state_machine_options;
        let mut params: Vec<(String, Value)> = Vec::new();
        let mut set = |name: &str, value: Value| {
            if let Some(entry) = params.iter_mut().find(|(key, _)| key == name) {
                entry.1 = value;
            } else {
                params.push((name.to_owned(), value));
            }
        };
        if sm.delimiter.is_set_by_user() {
            set("delim", Value::varchar(self.delimiter()));
        }
        if sm.new_line.is_set_by_user() {
            set("new_line", Value::varchar(self.newline()));
        }
        if sm.quote.is_set_by_user() {
            set("quote", Value::varchar(self.quote()));
        }
        if sm.escape.is_set_by_user() {
            set("escape", Value::varchar(self.escape()));
        }
        if sm.comment.is_set_by_user() {
            set("comment", Value::varchar(self.comment_str()));
        }
        if self.dialect_options.header.is_set_by_user() {
            set("header", Value::boolean(self.header()));
        }
        if sm.strict_mode.is_set_by_user() {
            set("strict_mode", Value::boolean(self.rfc4180()));
        }
        set(
            "max_line_size",
            Value::bigint(self.maximum_line_size.value() as i64),
        );
        if self.dialect_options.skip_rows.is_set_by_user() {
            set("skip", Value::ubigint(self.skip_rows()));
        }
        set("null_padding", Value::boolean(self.null_padding));
        set("parallel", Value::boolean(self.parallel));
        let date = self.dialect_options.date_format(FormatType::Date).get();
        if !date.is_empty() {
            set("dateformat", Value::varchar(date.format_specifier()));
        }
        let timestamp = self.dialect_options.date_format(FormatType::Timestamp).get();
        if !timestamp.is_empty() {
            set("timestampformat", Value::varchar(timestamp.format_specifier()));
        }
        set("normalize_names", Value::boolean(self.normalize_names));
        if !self.name_list.is_empty() {
            set(
                "column_names",
                Value::list(
                    LogicalType::VARCHAR,
                    self.name_list
                        .iter()
                        .map(|name| Value::try_varchar(name))
                        .collect::<Result<_>>()?,
                ),
            );
        }
        set("all_varchar", Value::boolean(self.all_varchar));
        Ok(params)
    }
}

/// `GetBooleanValue`.
fn boolean_value(loption: &[u8], value: &Value) -> Result<bool> {
    if value.is_null() {
        return Err(Error::binder(roastedduck_common::byte_format!(
            "read_csv {loption} cannot be NULL"
        )));
    }
    value.as_bool().ok_or_else(|| {
        Error::binder(roastedduck_common::byte_format!(
            "read_csv {loption} expects a boolean"
        ))
    })
}

/// A single-byte option (`SetQuote`, `SetEscape`, `SetComment`): empty means none.
fn single_byte(input: &str, what: &str) -> Result<u8> {
    if input.len() > 1 {
        return Err(Error::invalid_input(format!(
            "The {what} option cannot exceed a size of 1 byte."
        )));
    }
    Ok(input.as_bytes().first().copied().unwrap_or(0))
}

/// `std::string(1, c)`.
pub(crate) fn byte_string(byte: u8) -> String {
    String::from_utf8_lossy(&[byte]).into_owned()
}
