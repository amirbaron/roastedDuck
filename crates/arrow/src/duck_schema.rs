//! The DuckDB types of imported Arrow fields (DuckDB's
//! `function/table/arrow/arrow_duck_schema.cpp` and `arrow_type_info.cpp`): an Arrow
//! format string, with its children, dictionary and extension metadata, becomes a
//! [`LogicalType`] plus the layout details the conversion of the data needs.

use std::sync::Arc;

use rustdb_common::{Error, Result};
use rustdb_types::LogicalType;

use crate::ffi::ArrowSchema;
use crate::schema_metadata::ArrowSchemaMetadata;
use crate::type_extension::{ArrowTypeExtensionData, arrow_type_extensions};

/// The unit of an Arrow date, time, timestamp or duration (DuckDB's
/// `ArrowDateTimeType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArrowDateTimeType {
    Milliseconds,
    Microseconds,
    Nanoseconds,
    Seconds,
    Days,
    Months,
    MonthDayNano,
}

/// The layout of a variable-size Arrow array (DuckDB's `ArrowVariableSizeType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArrowVariableSizeType {
    /// 32-bit offsets.
    Normal,
    /// Fixed-size binary: no offsets.
    FixedSize,
    /// 64-bit offsets.
    SuperSize,
    /// Binary/string views.
    View,
}

/// The bit width of an Arrow decimal (DuckDB's `DecimalBitWidth`; 256-bit decimals are
/// rejected when the format is read).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecimalBitWidth {
    Decimal32,
    Decimal64,
    Decimal128,
}

/// How an Arrow array is physically encoded (DuckDB's `ArrowArrayPhysicalType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArrowArrayPhysicalType {
    DictionaryEncoded,
    RunEndEncoded,
    Default,
}

/// The layout details of an Arrow type (DuckDB's `ArrowTypeInfo` subclasses).
#[derive(Clone, Debug)]
pub enum ArrowTypeInfo {
    /// The children of a struct, union or run-end encoded array (`ArrowStructInfo`).
    Struct(Vec<ArrowType>),
    /// `ArrowDateTimeInfo`
    DateTime(ArrowDateTimeType),
    /// `ArrowDecimalInfo`
    Decimal(DecimalBitWidth),
    /// `ArrowStringInfo`: `fixed_size` is set for `FixedSize`.
    String {
        size_type: ArrowVariableSizeType,
        fixed_size: usize,
    },
    /// `ArrowListInfo` of a list, list view or map.
    List {
        size_type: ArrowVariableSizeType,
        is_view: bool,
        child: Box<ArrowType>,
    },
    /// `ArrowArrayInfo` of a fixed-size list.
    Array {
        child: Box<ArrowType>,
        fixed_size: usize,
    },
}

/// The DuckDB type of an Arrow field (DuckDB's `ArrowType`).
#[derive(Clone, Debug)]
pub struct ArrowType {
    ty: LogicalType,
    /// The type of the dictionary, for a dictionary-encoded field.
    dictionary_type: Option<Box<ArrowType>>,
    run_end_encoded: bool,
    type_info: Option<ArrowTypeInfo>,
    /// The extension type the field's metadata names, if registered.
    pub(crate) extension_data: Option<Arc<ArrowTypeExtensionData>>,
}

impl ArrowType {
    pub fn new(ty: LogicalType, type_info: Option<ArrowTypeInfo>) -> Self {
        Self {
            ty,
            dictionary_type: None,
            run_end_encoded: false,
            type_info,
            extension_data: None,
        }
    }

    /// The children of a struct, union or run-end encoded type (`ArrowStructInfo`).
    pub fn struct_children(&self) -> Result<&[ArrowType]> {
        match &self.type_info {
            Some(ArrowTypeInfo::Struct(children)) => Ok(children),
            _ => Err(type_info_mismatch("STRUCT")),
        }
    }

    /// The child and layout of a list, list view or map (`ArrowListInfo`).
    pub fn list_info(&self) -> Result<(&ArrowType, ArrowVariableSizeType, bool)> {
        match &self.type_info {
            Some(ArrowTypeInfo::List {
                size_type,
                is_view,
                child,
            }) => Ok((child, *size_type, *is_view)),
            _ => Err(type_info_mismatch("LIST")),
        }
    }

    /// The child and size of a fixed-size list (`ArrowArrayInfo`).
    pub fn array_info(&self) -> Result<(&ArrowType, usize)> {
        match &self.type_info {
            Some(ArrowTypeInfo::Array { child, fixed_size }) => Ok((child, *fixed_size)),
            _ => Err(type_info_mismatch("ARRAY")),
        }
    }

    /// The layout of a string or binary type (`ArrowStringInfo`).
    pub fn string_info(&self) -> Result<(ArrowVariableSizeType, usize)> {
        match &self.type_info {
            Some(ArrowTypeInfo::String {
                size_type,
                fixed_size,
            }) => Ok((*size_type, *fixed_size)),
            _ => Err(type_info_mismatch("STRING")),
        }
    }

    /// The unit of a temporal type (`ArrowDateTimeInfo`).
    pub fn date_time_info(&self) -> Result<ArrowDateTimeType> {
        match &self.type_info {
            Some(ArrowTypeInfo::DateTime(unit)) => Ok(*unit),
            _ => Err(type_info_mismatch("DATE_TIME")),
        }
    }

    /// The width of a decimal (`ArrowDecimalInfo`).
    pub fn decimal_info(&self) -> Result<DecimalBitWidth> {
        match &self.type_info {
            Some(ArrowTypeInfo::Decimal(width)) => Ok(*width),
            _ => Err(type_info_mismatch("DECIMAL")),
        }
    }

    /// The DuckDB type; with `use_dictionary`, the type of the values of dictionary
    /// encoded fields, at any nesting depth (`GetDuckType`).
    pub fn duck_type(&self, use_dictionary: bool) -> LogicalType {
        if use_dictionary && let Some(dictionary) = &self.dictionary_type {
            return dictionary.duck_type(true);
        }
        if !use_dictionary {
            return self.ty.clone();
        }
        // Dictionaries can exist in arbitrarily nested schemas: rebuild the type.
        match (&self.type_info, self.ty.id()) {
            (Some(ArrowTypeInfo::Struct(children)), rustdb_types::LogicalTypeId::Struct) => {
                let names = self.ty.struct_children().unwrap_or_default();
                LogicalType::struct_type(
                    children
                        .iter()
                        .zip(names)
                        .map(|(child, (name, _))| (name.clone(), child.duck_type(true)))
                        .collect(),
                )
            }
            (Some(ArrowTypeInfo::Struct(children)), rustdb_types::LogicalTypeId::Union) => {
                let names = self.ty.union_members().unwrap_or_default();
                LogicalType::union_type(
                    children
                        .iter()
                        .zip(names)
                        .map(|(child, (name, _))| (name.clone(), child.duck_type(true)))
                        .collect(),
                )
            }
            (Some(ArrowTypeInfo::List { child, .. }), rustdb_types::LogicalTypeId::List) => {
                LogicalType::list(child.duck_type(true))
            }
            (Some(ArrowTypeInfo::List { child, .. }), rustdb_types::LogicalTypeId::Map) => {
                let entry = child.duck_type(true);
                let fields = entry.struct_children().unwrap_or_default();
                match fields {
                    [(_, key), (_, value)] => LogicalType::map(key.clone(), value.clone()),
                    _ => self.ty.clone(),
                }
            }
            _ => self.ty.clone(),
        }
    }

    fn set_dictionary(&mut self, dictionary: ArrowType) {
        self.dictionary_type = Some(Box::new(dictionary));
    }

    /// The type of the dictionary of a dictionary-encoded field (`GetDictionary`).
    pub fn dictionary(&self) -> Option<&ArrowType> {
        self.dictionary_type.as_deref()
    }

    /// Makes a run-end encoded type (a struct of run ends and values) the type of its
    /// values (`SetRunEndEncoded`).
    fn set_run_end_encoded(&mut self) -> Result<()> {
        let values = self
            .struct_children()?
            .get(1)
            .ok_or_else(|| Error::internal("run-end encoded type without values"))?;
        self.ty = values.duck_type(false);
        self.run_end_encoded = true;
        Ok(())
    }

    /// `GetPhysicalType`
    pub fn physical_type(&self) -> ArrowArrayPhysicalType {
        if self.dictionary_type.is_some() {
            ArrowArrayPhysicalType::DictionaryEncoded
        } else if self.run_end_encoded {
            ArrowArrayPhysicalType::RunEndEncoded
        } else {
            ArrowArrayPhysicalType::Default
        }
    }

    /// The type of a format without children (`GetTypeFromFormat(string &)`); `None` for
    /// nested and unknown formats.
    pub fn from_primitive_format(format: &str) -> Result<Option<Self>> {
        use ArrowDateTimeType as T;
        let date_time = |ty: LogicalType, unit: T| Self::new(ty, Some(ArrowTypeInfo::DateTime(unit)));
        let string = |ty: LogicalType, size_type: ArrowVariableSizeType| {
            Self::new(
                ty,
                Some(ArrowTypeInfo::String {
                    size_type,
                    fixed_size: 0,
                }),
            )
        };
        let simple = |ty: LogicalType| Self::new(ty, None);
        let bytes = format.as_bytes();
        Ok(Some(match format {
            "n" => simple(LogicalType::SQLNULL),
            "b" => simple(LogicalType::BOOLEAN),
            "c" => simple(LogicalType::TINYINT),
            "s" => simple(LogicalType::SMALLINT),
            "i" => simple(LogicalType::INTEGER),
            "l" => simple(LogicalType::BIGINT),
            "C" => simple(LogicalType::UTINYINT),
            "S" => simple(LogicalType::USMALLINT),
            "I" => simple(LogicalType::UINTEGER),
            "L" => simple(LogicalType::UBIGINT),
            "f" => simple(LogicalType::FLOAT),
            "g" => simple(LogicalType::DOUBLE),
            _ if bytes.first() == Some(&b'd') => return decimal_type(format).map(Some),
            "u" => string(LogicalType::VARCHAR, ArrowVariableSizeType::Normal),
            "U" => string(LogicalType::VARCHAR, ArrowVariableSizeType::SuperSize),
            "vu" => string(LogicalType::VARCHAR, ArrowVariableSizeType::View),
            "tsn:" => simple(LogicalType::TIMESTAMP_NS),
            "tsu:" => simple(LogicalType::TIMESTAMP),
            "tsm:" => simple(LogicalType::TIMESTAMP_MS),
            "tss:" => simple(LogicalType::TIMESTAMP_S),
            "tdD" => date_time(LogicalType::DATE, T::Days),
            "tdm" => date_time(LogicalType::DATE, T::Milliseconds),
            "tts" => date_time(LogicalType::TIME, T::Seconds),
            "ttm" => date_time(LogicalType::TIME, T::Milliseconds),
            "ttu" => date_time(LogicalType::TIME, T::Microseconds),
            "ttn" => date_time(LogicalType::TIME_NS, T::Nanoseconds),
            "tDs" => date_time(LogicalType::INTERVAL, T::Seconds),
            "tDm" => date_time(LogicalType::INTERVAL, T::Milliseconds),
            "tDu" => date_time(LogicalType::INTERVAL, T::Microseconds),
            "tDn" => date_time(LogicalType::INTERVAL, T::Nanoseconds),
            "tiD" => date_time(LogicalType::INTERVAL, T::Days),
            "tiM" => date_time(LogicalType::INTERVAL, T::Months),
            "tin" => date_time(LogicalType::INTERVAL, T::MonthDayNano),
            "z" => string(LogicalType::BLOB, ArrowVariableSizeType::Normal),
            "Z" => string(LogicalType::BLOB, ArrowVariableSizeType::SuperSize),
            "vz" => string(LogicalType::BLOB, ArrowVariableSizeType::View),
            _ if bytes.first() == Some(&b'w') => {
                // Fixed-size binary is `w:NN`, the colon always at position 1.
                if bytes.len() <= 2 || bytes[1] != b':' {
                    return Err(Error::invalid_input(format!(
                        "Invalid Arrow fixed-size binary format string: \"{format}\""
                    )));
                }
                Self::new(
                    LogicalType::BLOB,
                    Some(ArrowTypeInfo::String {
                        size_type: ArrowVariableSizeType::FixedSize,
                        fixed_size: parse_size(&format[2..])?,
                    }),
                )
            }
            _ if bytes.starts_with(b"ts") => {
                // A timestamp with a time zone; the values are read as UTC.
                let unit = match bytes.get(2) {
                    Some(b'n') => T::Nanoseconds,
                    Some(b'u') => T::Microseconds,
                    Some(b'm') => T::Milliseconds,
                    Some(b's') => T::Seconds,
                    _ => return Err(Error::not_implemented(" Timestamptz precision of not accepted")),
                };
                date_time(LogicalType::TIMESTAMP_TZ, unit)
            }
            _ => return Ok(None),
        }))
    }

    /// The type of a field from its format and children
    /// (`GetTypeFromFormat(ClientContext &, ArrowSchema &, string &)`).
    ///
    /// # Safety
    /// `schema` must follow the C data interface.
    unsafe fn from_format(schema: &ArrowSchema, format: &str) -> Result<Self> {
        if let Some(arrow_type) = Self::from_primitive_format(format)? {
            return Ok(arrow_type);
        }
        // SAFETY: guaranteed by the caller.
        let children: Vec<&ArrowSchema> = unsafe { schema.child_schemas() }.collect();
        let first_child = || {
            children
                .first()
                .copied()
                .ok_or_else(|| Error::invalid_input(format!("Arrow type \"{format}\" without children")))
        };
        let bytes = format.as_bytes();
        match format {
            "+l" | "+L" | "+vl" | "+vL" => {
                let size_type = if format.ends_with('l') {
                    ArrowVariableSizeType::Normal
                } else {
                    ArrowVariableSizeType::SuperSize
                };
                // SAFETY: the child follows the C data interface.
                unsafe { Self::create_list_type(first_child()?, size_type, format.starts_with("+v")) }
            }
            _ if bytes.starts_with(b"+w") => {
                // A fixed-size list is `+w:NN`, the colon always at position 2.
                if bytes.len() <= 3 || bytes[2] != b':' {
                    return Err(Error::invalid_input(format!(
                        "Invalid Arrow fixed-size list format string: \"{format}\""
                    )));
                }
                let fixed_size = parse_size(&format[3..])?;
                // SAFETY: the child follows the C data interface.
                let child_type = unsafe { Self::from_schema_with_dictionary(first_child()?) }?;
                let array_type = LogicalType::array(child_type.duck_type(false), fixed_size as u32);
                Ok(Self::new(
                    array_type,
                    Some(ArrowTypeInfo::Array {
                        child: Box::new(child_type),
                        fixed_size,
                    }),
                ))
            }
            "+s" => {
                if children.is_empty() {
                    return Err(Error::invalid_input(
                        "Attempted to convert a STRUCT with no fields to DuckDB which is not supported",
                    ));
                }
                // SAFETY: the children follow the C data interface.
                let (child_types, fields) = unsafe { Self::member_types(&children) }?;
                Ok(Self::new(
                    LogicalType::struct_type(fields),
                    Some(ArrowTypeInfo::Struct(child_types)),
                ))
            }
            _ if bytes.starts_with(b"+u") => {
                if bytes.get(2) != Some(&b's') {
                    return Err(Error::not_implemented(format!(
                        "Unsupported Internal Arrow Type: \"{}\" Union",
                        bytes
                            .get(2)
                            .map_or(String::new(), |&kind| char::from(kind).to_string())
                    )));
                }
                if children.is_empty() {
                    return Err(Error::invalid_input(
                        "Attempted to convert a UNION with no fields to DuckDB which is not supported",
                    ));
                }
                // SAFETY: the children follow the C data interface.
                let (child_types, members) = unsafe { Self::member_types(&children) }?;
                Ok(Self::new(
                    LogicalType::union_type(members),
                    Some(ArrowTypeInfo::Struct(child_types)),
                ))
            }
            "+r" => {
                // SAFETY: the children follow the C data interface.
                let (child_types, members) = unsafe { Self::member_types(&children) }?;
                let mut struct_type = Self::new(
                    LogicalType::struct_type(members),
                    Some(ArrowTypeInfo::Struct(child_types)),
                );
                struct_type.set_run_end_encoded()?;
                Ok(struct_type)
            }
            "+m" => {
                // SAFETY: the child follows the C data interface.
                let entries: Vec<&ArrowSchema> = unsafe { first_child()?.child_schemas() }.collect();
                let [key, value] = entries[..] else {
                    return Err(Error::invalid_input("Arrow map entries must have two fields"));
                };
                // SAFETY: the entries follow the C data interface.
                let (key_type, value_type) = unsafe {
                    (
                        Self::from_schema_with_dictionary(key)?,
                        Self::from_schema_with_dictionary(value)?,
                    )
                };
                let map_type = LogicalType::map(key_type.duck_type(false), value_type.duck_type(false));
                let entry_type = LogicalType::struct_type(vec![
                    ("key".to_owned(), key_type.duck_type(false)),
                    ("value".to_owned(), value_type.duck_type(false)),
                ]);
                let inner_struct = Self::new(
                    entry_type,
                    Some(ArrowTypeInfo::Struct(vec![key_type, value_type])),
                );
                Ok(Self::new(
                    map_type,
                    Some(ArrowTypeInfo::List {
                        size_type: ArrowVariableSizeType::Normal,
                        is_view: false,
                        child: Box::new(inner_struct),
                    }),
                ))
            }
            _ => Err(Error::not_implemented(format!(
                "Unsupported Internal Arrow Type {format}"
            ))),
        }
    }

    /// The types and named DuckDB types of the children of a struct or union.
    ///
    /// # Safety
    /// The children must follow the C data interface.
    unsafe fn member_types(children: &[&ArrowSchema]) -> Result<MemberTypes> {
        let mut child_types = Vec::with_capacity(children.len());
        let mut members = Vec::with_capacity(children.len());
        for child in children {
            // SAFETY: guaranteed by the caller.
            let child_type = unsafe { Self::from_schema_with_dictionary(child) }?;
            // SAFETY: as above.
            members.push((unsafe { child.name_string() }, child_type.duck_type(false)));
            child_types.push(child_type);
        }
        Ok((child_types, members))
    }

    /// `CreateListType`
    ///
    /// # Safety
    /// `child` must follow the C data interface.
    unsafe fn create_list_type(
        child: &ArrowSchema,
        size_type: ArrowVariableSizeType,
        view: bool,
    ) -> Result<Self> {
        // SAFETY: guaranteed by the caller.
        let child_type = unsafe { Self::from_schema_with_dictionary(child) }?;
        Ok(Self::new(
            LogicalType::list(child_type.duck_type(false)),
            Some(ArrowTypeInfo::List {
                size_type,
                is_view: view,
                child: Box::new(child_type),
            }),
        ))
    }

    /// The type of a field, an extension type if its metadata names a registered one
    /// (`GetTypeFromSchema`).
    ///
    /// # Safety
    /// `schema` must follow the C data interface.
    unsafe fn from_schema(schema: &ArrowSchema) -> Result<Self> {
        // SAFETY: guaranteed by the caller.
        let format = unsafe { schema.format_str() };
        // SAFETY: as above.
        let metadata = unsafe { ArrowSchemaMetadata::from_c(schema.metadata) }?;
        // SAFETY: as above.
        let mut arrow_type = unsafe { Self::from_format(schema, format) }?;
        if metadata.has_extension()
            && let Some(extension) = arrow_type_extensions().for_metadata(&metadata.extension_info(format))
        {
            arrow_type = extension.get_type(format, &metadata)?;
            arrow_type.extension_data = Some(Arc::clone(extension.type_extension()));
        }
        Ok(arrow_type)
    }

    /// The type of a field and of its dictionary, if any (`GetArrowLogicalType`).
    ///
    /// # Safety
    /// `schema` must follow the C data interface.
    pub unsafe fn from_schema_with_dictionary(schema: &ArrowSchema) -> Result<Self> {
        // SAFETY: guaranteed by the caller.
        let mut arrow_type = unsafe { Self::from_schema(schema) }?;
        // SAFETY: as above; a dictionary is a schema of the same interface.
        if let Some(dictionary) = unsafe { schema.dictionary.as_ref() } {
            // SAFETY: as above.
            arrow_type.set_dictionary(unsafe { Self::from_schema_with_dictionary(dictionary) }?);
        }
        Ok(arrow_type)
    }
}

/// The Arrow types of the children of a struct or union, and their names and DuckDB types.
type MemberTypes = (Vec<ArrowType>, Vec<(String, LogicalType)>);

/// The error of reading layout details a type does not have (`ArrowTypeInfo::Cast`).
fn type_info_mismatch(expected: &str) -> Error {
    Error::internal(format!(
        "Failed to cast ArrowTypeInfo, type mismatch (expected: {expected})"
    ))
}

/// The leading decimal digits of `text` (`std::stoull` / `std::stoi` of DuckDB's format
/// parsing, which ignore what follows the number).
fn parse_size(text: &str) -> Result<usize> {
    let digits = text.trim_start();
    let end = digits
        .bytes()
        .position(|c| !c.is_ascii_digit())
        .unwrap_or(digits.len());
    digits[..end]
        .parse()
        .map_err(|_| Error::invalid_input(format!("Invalid number in Arrow format: \"{text}\"")))
}

/// A decimal format `d:width,scale[,bitwidth]` (part of `GetTypeFromFormat`).
fn decimal_type(format: &str) -> Result<ArrowType> {
    let extra_info: Vec<&str> = format.split(':').collect();
    if extra_info.len() != 2 {
        return Err(Error::invalid_input(format!(
            "Decimal format of Arrow object is incomplete, it is missing the scale and width. Current format: {format}"
        )));
    }
    let parameters: Vec<&str> = extra_info[1].split(',').collect();
    // Parameters must always be 2 or 3 values (width, scale and an optional bit width).
    if parameters.len() != 2 && parameters.len() != 3 {
        return Err(Error::invalid_input(format!(
            "Decimal format of Arrow object is incomplete, it is missing the scale or width. Current format: {format}"
        )));
    }
    let width = parse_size(parameters[0])?;
    let scale = parse_size(parameters[1])?;
    let bit_width = match parameters.get(2) {
        Some(bit_width) => parse_size(bit_width)?,
        None => 128,
    };
    if width > 38 || bit_width > 128 {
        return Err(Error::not_implemented(format!(
            "Unsupported Internal Arrow Type for Decimal {format}"
        )));
    }
    let bit_width = match bit_width {
        32 => DecimalBitWidth::Decimal32,
        64 => DecimalBitWidth::Decimal64,
        128 => DecimalBitWidth::Decimal128,
        other => {
            return Err(Error::not_implemented(format!(
                "Unsupported bit-width value of {other}  for Arrow Decimal type"
            )));
        }
    };
    Ok(ArrowType::new(
        LogicalType::decimal(width as u8, scale as u8),
        Some(ArrowTypeInfo::Decimal(bit_width)),
    ))
}

/// The columns of an imported Arrow table (DuckDB's `ArrowTableSchema`).
#[derive(Clone, Debug, Default)]
pub struct ArrowTableSchema {
    columns: Vec<ArrowType>,
    types: Vec<LogicalType>,
    names: Vec<String>,
}

impl ArrowTableSchema {
    /// Adds the next column (`AddColumn`); its DuckDB type is that of dictionary values.
    pub fn add_column(&mut self, arrow_type: ArrowType, name: String) {
        self.types.push(arrow_type.duck_type(true));
        self.names.push(name);
        self.columns.push(arrow_type);
    }

    /// The Arrow types of the columns (`GetColumns`).
    pub fn columns(&self) -> &[ArrowType] {
        &self.columns
    }

    /// `GetTypes`
    pub fn types(&self) -> &[LogicalType] {
        &self.types
    }

    /// `GetNames`
    pub fn names(&self) -> &[String] {
        &self.names
    }
}
