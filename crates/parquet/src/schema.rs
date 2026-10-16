//! The DuckDB view of a Parquet schema (DuckDB's `ParquetColumnSchema`,
//! `ParquetReader::DeriveLogicalType` and `ParseSchemaRecursive`): the flattened
//! `SchemaElement` list becomes a tree of columns with DuckDB types and the definition
//! and repetition levels of every node.

use rustdb_common::{CaseInsensitiveMap, Error, ErrorKind, Result};
use rustdb_types::{LogicalType, LogicalTypeId};

use crate::thrift::{ConvertedType, FieldRepetitionType, FileMetaData, SchemaElement, Type};

/// How a schema node is read (`ParquetColumnSchemaType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SchemaKind {
    /// A column stored in the file.
    Column,
    /// The `file_row_number` virtual column.
    FileRowNumber,
    /// A VARIANT group (`metadata` + `value` binaries).
    Variant,
    /// A WKB geometry stored as BLOB (its only child is the BLOB column).
    Geometry,
}

/// What a leaf's physical values mean beyond its DuckDB type (`ParquetExtraTypeInfo`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ExtraTypeInfo {
    #[default]
    None,
    ImpalaTimestamp,
    UnitNs,
    UnitMs,
    UnitMicros,
    DecimalByteArray,
    DecimalInt32,
    DecimalInt64,
    Float16,
}

/// The reader options that affect the schema and the scan (`ParquetOptions`).
#[derive(Clone, Debug, Default)]
pub struct ParquetOptions {
    pub binary_as_string: bool,
    pub variant_legacy_encoding: bool,
    pub file_row_number: bool,
    /// Whether floats may be NaN, which min/max statistics do not cover.
    pub can_have_nan: bool,
    /// The `schema` parameter: (identifier, name, type, default value) per column.
    pub schema: Vec<ParquetColumnDefinition>,
    pub explicit_cardinality: u64,
}

/// One column of the `schema` parameter (`ParquetColumnDefinition`).
#[derive(Clone, Debug)]
pub struct ParquetColumnDefinition {
    /// The field id (INTEGER) or name (VARCHAR) the column is matched by.
    pub identifier: rustdb_types::Value,
    pub name: String,
    pub logical_type: LogicalType,
    pub default_value: rustdb_types::Value,
}

/// A node of the column tree (`ParquetColumnSchema`).
#[derive(Clone, Debug)]
pub struct ColumnSchema {
    pub name: String,
    pub max_define: u8,
    pub max_repeat: u8,
    /// The index of the node's `SchemaElement`.
    pub schema_index: Option<usize>,
    /// For leaves: the index of the column chunk in each row group.
    pub column_index: usize,
    pub kind: SchemaKind,
    pub logical_type: LogicalType,
    /// For repeated groups: the index of the `SchemaElement` that holds the field id.
    pub parent_schema_index: Option<usize>,
    pub type_length: u32,
    pub type_scale: u32,
    pub parquet_type: Type,
    pub type_info: ExtraTypeInfo,
    pub children: Vec<ColumnSchema>,
}

impl ColumnSchema {
    /// `ParquetColumnSchema::FromChildSchemas`.
    fn from_children(
        name: &str,
        logical_type: LogicalType,
        max_define: u8,
        max_repeat: u8,
        schema_index: usize,
        column_index: usize,
        children: Vec<ColumnSchema>,
        kind: SchemaKind,
    ) -> Self {
        Self {
            name: name.to_owned(),
            max_define,
            max_repeat,
            schema_index: Some(schema_index),
            column_index,
            kind,
            logical_type,
            parent_schema_index: None,
            type_length: 0,
            type_scale: 0,
            parquet_type: Type::INT32,
            type_info: ExtraTypeInfo::None,
            children,
        }
    }

    /// `ParquetColumnSchema::FileRowNumber`.
    pub fn file_row_number() -> Self {
        Self {
            name: "file_row_number".to_owned(),
            max_define: 0,
            max_repeat: 0,
            schema_index: Some(0),
            column_index: 0,
            kind: SchemaKind::FileRowNumber,
            logical_type: LogicalType::BIGINT,
            parent_schema_index: None,
            type_length: 0,
            type_scale: 0,
            parquet_type: Type::INT32,
            type_info: ExtraTypeInfo::None,
            children: Vec::new(),
        }
    }
}

fn io_error(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::Io, message.into())
}

/// `ParquetReader::DeriveLogicalType`: the DuckDB type of a leaf, recording how its
/// physical values are converted in `schema`.
pub fn derive_logical_type(
    element: &SchemaElement,
    options: &ParquetOptions,
    schema: &mut ColumnSchema,
) -> Result<LogicalType> {
    let physical = element.type_.unwrap_or_default();
    if physical == Type::FIXED_LEN_BYTE_ARRAY && element.type_length.is_none() {
        return Err(io_error("FIXED_LEN_BYTE_ARRAY requires length to be set"));
    }
    if let Some(length) = element.type_length {
        schema.type_length = u32::try_from(length).map_err(|_| {
            Error::conversion(format!(
                "Type INT32 with value {length} can't be cast because the value is out of range for the destination type UINT32"
            ))
        })?;
    }
    schema.parquet_type = physical;
    if let Some(logical) = &element.logical_type {
        if logical.unknown.is_some() {
            return Ok(LogicalType::SQLNULL);
        } else if logical.uuid.is_some() {
            if physical == Type::FIXED_LEN_BYTE_ARRAY {
                return Ok(LogicalType::UUID);
            }
        } else if logical.float16.is_some() {
            if physical == Type::FIXED_LEN_BYTE_ARRAY && element.type_length == Some(2) {
                schema.type_info = ExtraTypeInfo::Float16;
                return Ok(LogicalType::FLOAT);
            }
        } else if let Some(timestamp) = &logical.timestamp {
            schema.type_info = time_unit(&timestamp.unit, "TIMESTAMP")?;
            if timestamp.is_adjusted_to_utc {
                return Ok(LogicalType::TIMESTAMP_TZ);
            } else if timestamp.unit.nanos.is_some() {
                return Ok(LogicalType::TIMESTAMP_NS);
            }
            return Ok(LogicalType::TIMESTAMP);
        } else if let Some(time) = &logical.time {
            schema.type_info = time_unit(&time.unit, "TIME")?;
            if time.is_adjusted_to_utc {
                return Ok(LogicalType::TIME_TZ);
            } else if time.unit.nanos.is_some() {
                return Ok(LogicalType::TIME_NS);
            }
            return Ok(LogicalType::TIME);
        }
    }
    if let Some(converted) = element.converted_type {
        if converted == ConvertedType::PARQUET_NULL {
            return Ok(LogicalType::SQLNULL);
        }
        let require = |expected: Type, result: LogicalType, message: &str| {
            if physical == expected {
                Ok(result)
            } else {
                Err(io_error(message.to_owned()))
            }
        };
        return match converted {
            ConvertedType::INT_8 => require(
                Type::INT32,
                LogicalType::TINYINT,
                "INT8 converted type can only be set for value of Type::INT32",
            ),
            ConvertedType::INT_16 => require(
                Type::INT32,
                LogicalType::SMALLINT,
                "INT16 converted type can only be set for value of Type::INT32",
            ),
            ConvertedType::INT_32 => require(
                Type::INT32,
                LogicalType::INTEGER,
                "INT32 converted type can only be set for value of Type::INT32",
            ),
            ConvertedType::INT_64 => require(
                Type::INT64,
                LogicalType::BIGINT,
                "INT64 converted type can only be set for value of Type::INT32",
            ),
            ConvertedType::UINT_8 => require(
                Type::INT32,
                LogicalType::UTINYINT,
                "UINT8 converted type can only be set for value of Type::INT32",
            ),
            ConvertedType::UINT_16 => require(
                Type::INT32,
                LogicalType::USMALLINT,
                "UINT16 converted type can only be set for value of Type::INT32",
            ),
            ConvertedType::UINT_32 => require(
                Type::INT32,
                LogicalType::UINTEGER,
                "UINT32 converted type can only be set for value of Type::INT32",
            ),
            ConvertedType::UINT_64 => require(
                Type::INT64,
                LogicalType::UBIGINT,
                "UINT64 converted type can only be set for value of Type::INT64",
            ),
            ConvertedType::DATE => require(
                Type::INT32,
                LogicalType::DATE,
                "DATE converted type can only be set for value of Type::INT32",
            ),
            ConvertedType::TIMESTAMP_MICROS => {
                schema.type_info = ExtraTypeInfo::UnitMicros;
                require(
                    Type::INT64,
                    LogicalType::TIMESTAMP,
                    "TIMESTAMP converted type can only be set for value of Type::INT64",
                )
            }
            ConvertedType::TIMESTAMP_MILLIS => {
                schema.type_info = ExtraTypeInfo::UnitMs;
                require(
                    Type::INT64,
                    LogicalType::TIMESTAMP,
                    "TIMESTAMP converted type can only be set for value of Type::INT64",
                )
            }
            ConvertedType::DECIMAL => {
                let (Some(precision), Some(scale)) = (element.precision, element.scale) else {
                    return Err(io_error("DECIMAL requires a length and scale specifier!"));
                };
                schema.type_scale = u32::try_from(scale).map_err(|_| {
                    Error::conversion(format!(
                        "Type INT32 with value {scale} can't be cast because the value is out of range for the destination type UINT32"
                    ))
                })?;
                if precision > 38 {
                    schema.type_info = ExtraTypeInfo::DecimalByteArray;
                    return Ok(LogicalType::DOUBLE);
                }
                schema.type_info = match physical {
                    Type::BYTE_ARRAY | Type::FIXED_LEN_BYTE_ARRAY => ExtraTypeInfo::DecimalByteArray,
                    Type::INT32 => ExtraTypeInfo::DecimalInt32,
                    Type::INT64 => ExtraTypeInfo::DecimalInt64,
                    _ => {
                        return Err(io_error(
                            "DECIMAL converted type can only be set for value of Type::(FIXED_LEN_)BYTE_ARRAY/INT32/INT64",
                        ));
                    }
                };
                // `LogicalType::DECIMAL(width, scale)` validates its arguments.
                if precision < 1 || scale < 0 || scale > precision {
                    return Err(Error::invalid_input(format!(
                        "Invalid DECIMAL({precision}, {scale}) type in Parquet file"
                    )));
                }
                Ok(LogicalType::decimal(precision as u8, scale as u8))
            }
            ConvertedType::UTF8 | ConvertedType::ENUM => match physical {
                Type::BYTE_ARRAY | Type::FIXED_LEN_BYTE_ARRAY => Ok(LogicalType::VARCHAR),
                _ => Err(io_error(
                    "UTF8 converted type can only be set for Type::(FIXED_LEN_)BYTE_ARRAY",
                )),
            },
            ConvertedType::TIME_MILLIS => {
                schema.type_info = ExtraTypeInfo::UnitMs;
                require(
                    Type::INT32,
                    LogicalType::TIME,
                    "TIME_MILLIS converted type can only be set for value of Type::INT32",
                )
            }
            ConvertedType::TIME_MICROS => {
                schema.type_info = ExtraTypeInfo::UnitMicros;
                require(
                    Type::INT64,
                    LogicalType::TIME,
                    "TIME_MICROS converted type can only be set for value of Type::INT64",
                )
            }
            ConvertedType::INTERVAL => Ok(LogicalType::INTERVAL),
            ConvertedType::JSON => Ok(LogicalType::VARCHAR.with_alias("JSON")),
            _ => Err(io_error(format!(
                "Unsupported converted type ({})",
                converted.0
            ))),
        };
    }
    Ok(match physical {
        Type::BOOLEAN => LogicalType::BOOLEAN,
        Type::INT32 => LogicalType::INTEGER,
        Type::INT64 => LogicalType::BIGINT,
        Type::INT96 => {
            schema.type_info = ExtraTypeInfo::ImpalaTimestamp;
            LogicalType::TIMESTAMP
        }
        Type::FLOAT => LogicalType::FLOAT,
        Type::DOUBLE => LogicalType::DOUBLE,
        Type::BYTE_ARRAY | Type::FIXED_LEN_BYTE_ARRAY => {
            if options.binary_as_string {
                LogicalType::VARCHAR
            } else {
                LogicalType::BLOB
            }
        }
        _ => LogicalType::INVALID,
    })
}

fn time_unit(unit: &crate::thrift::TimeUnit, what: &str) -> Result<ExtraTypeInfo> {
    if unit.millis.is_some() {
        Ok(ExtraTypeInfo::UnitMs)
    } else if unit.micros.is_some() {
        Ok(ExtraTypeInfo::UnitMicros)
    } else if unit.nanos.is_some() {
        Ok(ExtraTypeInfo::UnitNs)
    } else {
        Err(Error::not_implemented(format!(
            "Unimplemented {what} encoding - missing UNIT"
        )))
    }
}

/// `ParquetColumnSchema::FromSchemaElement`: a leaf column.
fn leaf_schema(
    element: &SchemaElement,
    max_define: u8,
    max_repeat: u8,
    schema_index: usize,
    column_index: usize,
    options: &ParquetOptions,
) -> Result<ColumnSchema> {
    let mut result = ColumnSchema::from_children(
        &element.name,
        LogicalType::INVALID,
        max_define,
        max_repeat,
        schema_index,
        column_index,
        Vec::new(),
        SchemaKind::Column,
    );
    result.logical_type = derive_logical_type(element, options, &mut result)?;
    Ok(result)
}

/// Builds the column tree of a file (`ParquetReader::ParseSchema`).
pub struct SchemaParser<'a> {
    metadata: &'a FileMetaData,
    options: &'a ParquetOptions,
    file_path: &'a str,
    next_schema_index: usize,
    next_file_index: usize,
}

impl<'a> SchemaParser<'a> {
    pub fn new(metadata: &'a FileMetaData, options: &'a ParquetOptions, file_path: &'a str) -> Self {
        Self {
            metadata,
            options,
            file_path,
            next_schema_index: 0,
            next_file_index: 0,
        }
    }

    /// `ParquetReader::ParseSchema`: the root STRUCT whose children are the columns.
    pub fn parse(mut self) -> Result<ColumnSchema> {
        let path = self.file_path;
        if self.metadata.schema.is_empty() {
            return Err(io_error(format!(
                "Failed to read Parquet file \"{path}\": no schema elements found"
            )));
        }
        if self.metadata.schema[0].num_children.unwrap_or(0) == 0 {
            return Err(io_error(format!(
                "Failed to read Parquet file \"{path}\": root schema element has no children"
            )));
        }
        let mut root = self.parse_recursive(0, 0, 0)?;
        if root.logical_type.id() != LogicalTypeId::Struct {
            return Err(Error::invalid_input(format!(
                "Failed to read Parquet file \"{path}\": Root element of Parquet file must be a struct"
            )));
        }
        if let Some(first) = self.metadata.row_groups.first()
            && self.next_file_index != first.columns.len()
        {
            return Err(Error::invalid_input(format!(
                "Failed to read Parquet file \"{path}\": row group does not have enough columns"
            )));
        }
        if self.options.file_row_number {
            if root
                .children
                .iter()
                .any(|column| column.name.eq_ignore_ascii_case("file_row_number"))
            {
                return Err(Error::binder(format!(
                    "Failed to read Parquet file \"{path}\": Using file_row_number option on file with column named file_row_number is not supported"
                )));
            }
            root.children.push(ColumnSchema::file_row_number());
        }
        Ok(root)
    }

    /// `ParquetReader::ParseSchemaRecursive`.
    fn parse_recursive(&mut self, depth: usize, mut max_define: u8, mut max_repeat: u8) -> Result<ColumnSchema> {
        let metadata = self.metadata;
        let this_index = self.next_schema_index;
        let Some(element) = metadata.schema.get(this_index) else {
            return Err(Error::invalid_input(format!(
                "Malformed Parquet schema in file \"{}\": invalid schema index {this_index}",
                self.file_path
            )));
        };
        let mut repetition = FieldRepetitionType::REQUIRED;
        if let Some(repetition_type) = element.repetition_type
            && this_index > 0
        {
            repetition = repetition_type;
        }
        if repetition != FieldRepetitionType::REQUIRED {
            max_define = max_define.saturating_add(1);
        }
        if repetition == FieldRepetitionType::REPEATED {
            max_repeat = max_repeat.saturating_add(1);
        }
        if is_geometry_type(element) {
            let child = leaf_schema(
                element,
                max_define,
                max_repeat,
                this_index,
                self.next_file_index,
                self.options,
            )?;
            let column_index = self.next_file_index;
            self.next_file_index += 1;
            return Ok(ColumnSchema::from_children(
                &element.name,
                LogicalType::GEOMETRY,
                max_define,
                max_repeat,
                this_index,
                column_index,
                vec![child],
                SchemaKind::Geometry,
            ));
        }
        let num_children = element.num_children.filter(|&count| count > 0);
        let is_inner = if element.type_.is_some() {
            num_children.is_some()
        } else {
            true
        };
        if is_inner {
            let count = match element.num_children {
                Some(count) => usize::try_from(count).map_err(|_| {
                    Error::invalid_input(format!(
                        "Failed to read Parquet file \"{}\": schema element has negative number of children",
                        self.file_path
                    ))
                })?,
                None => 0,
            };
            let mut children = Vec::with_capacity(count.min(1024));
            for _ in 0..count {
                self.next_schema_index += 1;
                children.push(self.parse_recursive(depth + 1, max_define, max_repeat)?);
            }
            // Rename case-insensitive duplicates by appending _1, _2, ...
            let mut collisions: CaseInsensitiveMap<usize> = CaseInsensitiveMap::default();
            for child in &mut children {
                let mut name = child.name.clone();
                while let Some(count) = collisions.get_mut(&name) {
                    *count += 1;
                    name = format!("{name}_{count}");
                }
                collisions.insert(name.clone(), 0);
                child.name = name;
            }
            let is_repeated = repetition == FieldRepetitionType::REPEATED;
            let converted = element.converted_type;
            let is_list = converted == Some(ConvertedType::LIST);
            let is_map = converted == Some(ConvertedType::MAP);
            let mut is_map_kv = converted == Some(ConvertedType::MAP_KEY_VALUE);
            let mut is_variant = element
                .logical_type
                .as_ref()
                .is_some_and(|logical| logical.variant.is_some());
            if !is_variant && self.options.variant_legacy_encoding && is_variant_type(&children)? {
                is_variant = true;
            }
            if !is_map_kv && this_index > 0 {
                let parent = &metadata.schema[this_index - 1];
                is_map_kv =
                    parent.converted_type == Some(ConvertedType::MAP) && parent.num_children == Some(1);
            }
            let file_index = self.next_file_index;
            if is_map_kv {
                if children.len() != 2 {
                    return Err(io_error("MAP_KEY_VALUE requires two children"));
                }
                if !is_repeated {
                    return Err(io_error("MAP_KEY_VALUE needs to be repeated"));
                }
                let map_type = LogicalType::map(
                    children[0].logical_type.clone(),
                    children[1].logical_type.clone(),
                );
                let entry_type = map_type
                    .child_type()
                    .cloned()
                    .ok_or_else(|| Error::internal("MAP without an entry type"))?;
                let entry = ColumnSchema::from_children(
                    &element.name,
                    entry_type,
                    max_define - 1,
                    max_repeat - 1,
                    this_index,
                    file_index,
                    children,
                    SchemaKind::Column,
                );
                return Ok(ColumnSchema::from_children(
                    &element.name,
                    map_type,
                    max_define,
                    max_repeat,
                    this_index,
                    file_index,
                    vec![entry],
                    SchemaKind::Column,
                ));
            }
            let mut result = if children.len() > 1 || (!is_list && !is_map && !is_repeated) {
                let (logical_type, kind) = if is_variant {
                    (LogicalType::variant(), SchemaKind::Variant)
                } else {
                    let fields = children
                        .iter()
                        .map(|child| (child.name.clone(), child.logical_type.clone()))
                        .collect();
                    (LogicalType::struct_type(fields), SchemaKind::Column)
                };
                ColumnSchema::from_children(
                    &element.name,
                    logical_type,
                    max_define,
                    max_repeat,
                    this_index,
                    file_index,
                    children,
                    kind,
                )
            } else if children.is_empty() {
                return Err(Error::invalid_input(format!(
                    "Failed to read Parquet file \"{}\": schema element has no children",
                    self.file_path
                )));
            } else {
                // A group with a single child is pulled up.
                let mut only = children.remove(0);
                only.name.clone_from(&element.name);
                only
            };
            if is_repeated {
                let list_type = LogicalType::list(result.logical_type.clone());
                result = ColumnSchema::from_children(
                    &element.name,
                    list_type,
                    max_define,
                    max_repeat,
                    this_index,
                    self.next_file_index,
                    vec![result],
                    SchemaKind::Column,
                );
            }
            result.parent_schema_index = Some(this_index);
            return Ok(result);
        }
        if element.type_.is_none() {
            return Err(Error::invalid_input(format!(
                "Node '{}' has neither num_children nor type set - this violates the Parquet spec (corrupted file)",
                element.name
            )));
        }
        let column_index = self.next_file_index;
        self.next_file_index += 1;
        let result = leaf_schema(
            element,
            max_define,
            max_repeat,
            this_index,
            column_index,
            self.options,
        )?;
        if element.repetition_type == Some(FieldRepetitionType::REPEATED) {
            let list_type = LogicalType::list(result.logical_type.clone());
            return Ok(ColumnSchema::from_children(
                &element.name,
                list_type,
                max_define,
                max_repeat,
                this_index,
                self.next_file_index,
                vec![result],
                SchemaKind::Column,
            ));
        }
        Ok(result)
    }
}

/// `IsGeometryType` for the native GEOMETRY/GEOGRAPHY annotations.
fn is_geometry_type(element: &SchemaElement) -> bool {
    element.type_ == Some(Type::BYTE_ARRAY)
        && element
            .logical_type
            .as_ref()
            .is_some_and(|logical| logical.geometry.is_some() || logical.geography.is_some())
}

/// `IsVariantType`: the legacy (unannotated) VARIANT layout.
fn is_variant_type(children: &[ColumnSchema]) -> Result<bool> {
    if children.len() < 2 {
        return Ok(false);
    }
    if children[0].name != "metadata" && children[1].name != "metadata" {
        return Ok(false);
    }
    if children[0].name != "value" && children[1].name != "value" {
        return Ok(false);
    }
    if children[0].parquet_type != Type::BYTE_ARRAY || children[1].parquet_type != Type::BYTE_ARRAY {
        return Ok(false);
    }
    if children.len() == 3 {
        if children[2].name != "typed_value" {
            return Ok(false);
        }
        return Err(Error::not_implemented("Shredded Variants are not supported yet"));
    }
    Ok(children.len() == 2)
}
