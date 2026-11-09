//! DuckDB data chunks and types to Arrow (DuckDB's `common/arrow/arrow_converter.cpp`):
//! [`to_arrow_schema`] describes columns as an Arrow struct schema, [`to_arrow_array`]
//! converts a chunk into an Arrow struct array.
//!
//! DuckDB fills the C structs of a schema in place while walking the types. Here the
//! walk builds a tree of [`SchemaField`]s, owned by the schema's private data, and the C
//! structs are made from it once it no longer moves.

use std::ffi::{CStr, c_char, c_void};

use roastedduck_common::{Error, Result};
use roastedduck_types::{DataChunk, LogicalType, LogicalTypeId, PhysicalType};

use crate::appender::ArrowAppender;
use crate::client_properties::{ArrowFormatVersion, ArrowOffsetSize, ClientProperties};
use crate::ffi::{ARROW_FLAG_NULLABLE, ArrowArray, ArrowSchema};
use crate::type_extension::arrow_type_extensions;

/// Converts the rows of `input` into an Arrow struct array of its columns
/// (`ArrowConverter::ToArrowArray`).
pub fn to_arrow_array(input: &DataChunk, options: &ClientProperties) -> Result<ArrowArray> {
    let types: Vec<LogicalType> = input
        .columns()
        .iter()
        .map(|column| column.logical_type().clone())
        .collect();
    let mut appender = ArrowAppender::new(types, input.len(), options.clone())?;
    appender.append(input, 0, input.len(), input.len())?;
    appender.finalize()
}

/// The format string of a field: a literal, or text made for the type.
#[derive(Debug)]
enum Format {
    Static(&'static CStr),
    /// NUL-terminated.
    Owned(Vec<u8>),
}

/// A field of an exported schema, before it is laid out as C structs.
#[derive(Debug)]
pub struct SchemaField {
    format: Format,
    /// NUL-terminated.
    name: Vec<u8>,
    /// The binary field metadata, if any.
    metadata: Option<Box<[u8]>>,
    flags: i64,
    children: Vec<SchemaField>,
    dictionary: Option<Box<SchemaField>>,
}

impl SchemaField {
    /// A nullable field without a format yet (`InitializeChild`).
    fn new(name: impl AsRef<[u8]>) -> Self {
        let mut name = name.as_ref().to_vec();
        name.push(0);
        Self {
            format: Format::Static(c""),
            name,
            metadata: None,
            flags: ARROW_FLAG_NULLABLE,
            children: Vec::new(),
            dictionary: None,
        }
    }

    /// Sets a literal format.
    pub(crate) fn set_format(&mut self, format: &'static CStr) {
        self.format = Format::Static(format);
    }

    /// Sets a format made at runtime (`owned_type_names` / `extension_format`).
    pub(crate) fn set_owned_format(&mut self, format: &str) {
        let mut bytes = format.as_bytes().to_vec();
        bytes.push(0);
        self.format = Format::Owned(bytes);
    }

    /// Sets the binary field metadata (`metadata_info`).
    pub(crate) fn set_metadata(&mut self, metadata: Box<[u8]>) {
        self.metadata = Some(metadata);
    }

    fn format_ptr(&self) -> *const c_char {
        match &self.format {
            Format::Static(format) => format.as_ptr(),
            Format::Owned(format) => format.as_ptr().cast(),
        }
    }
}

/// What an exported schema's private data holds (DuckDB's `DuckDBArrowSchemaHolder`):
/// the fields and the C structs of every nesting level, which point into them.
#[derive(Debug)]
struct DuckDbArrowSchemaHolder {
    columns: Vec<SchemaField>,
    exported: ExportedSchemas,
}

/// The C structs of the nested fields of an exported schema.
#[derive(Debug, Default)]
struct ExportedSchemas {
    /// The child schemas of each schema with children, as C reads them.
    child_schemas: Vec<Vec<ArrowSchema>>,
    /// The pointers to those child schemas (`children` of each schema).
    child_pointers: Vec<Vec<*mut ArrowSchema>>,
}

impl ExportedSchemas {
    /// The C struct of `field`, which points into it; its children are laid out here.
    /// Children are only released through the root (`InitializeChild`).
    fn export(&mut self, field: &SchemaField) -> ArrowSchema {
        let mut schema = ArrowSchema::empty();
        schema.format = field.format_ptr();
        schema.name = field.name.as_ptr().cast();
        schema.metadata = field
            .metadata
            .as_ref()
            .map_or(std::ptr::null(), |metadata| metadata.as_ptr().cast());
        schema.flags = field.flags;
        schema.release = Some(release_duckdb_arrow_schema);
        if !field.children.is_empty() {
            let (children, count) = self.export_children(&field.children);
            schema.children = children;
            schema.n_children = count;
        }
        if let Some(dictionary) = &field.dictionary {
            let (children, _) = self.export_children(std::slice::from_ref(&**dictionary));
            // SAFETY: `children` points to one pointer, to the dictionary's schema.
            schema.dictionary = unsafe { *children };
        }
        schema
    }

    /// Lays out the C structs of `fields`: the pointer to their pointers, and their count.
    fn export_children(&mut self, fields: &[SchemaField]) -> (*mut *mut ArrowSchema, i64) {
        let mut schemas: Vec<ArrowSchema> = fields.iter().map(|field| self.export(field)).collect();
        let mut pointers: Vec<*mut ArrowSchema> = schemas.iter_mut().map(std::ptr::from_mut).collect();
        let children = pointers.as_mut_ptr();
        // Moving the vectors keeps their heap buffers, which the pointers refer to.
        self.child_schemas.push(schemas);
        self.child_pointers.push(pointers);
        (children, fields.len() as i64)
    }
}

/// The release callback of exported schemas (`ReleaseDuckDBArrowSchema`): the root owns
/// the holder; children only mark themselves released.
unsafe extern "C" fn release_duckdb_arrow_schema(schema: *mut ArrowSchema) {
    // SAFETY: called by the consumer with a schema this module exported.
    let Some(schema) = (unsafe { schema.as_mut() }) else {
        return;
    };
    if schema.release.is_none() {
        return;
    }
    schema.release = None;
    let holder = std::mem::replace(&mut schema.private_data, std::ptr::null_mut());
    if !holder.is_null() {
        // SAFETY: the root's private data is the holder `to_arrow_schema` leaked.
        drop(unsafe { Box::from_raw(holder.cast::<DuckDbArrowSchemaHolder>()) });
    }
}

/// Describes columns of `types` named `names` as an Arrow struct schema named
/// `duckdb_query_result` (`ArrowConverter::ToArrowSchema`).
pub fn to_arrow_schema(
    out_schema: &mut ArrowSchema,
    types: &[LogicalType],
    names: &[roastedduck_common::ByteString],
    options: &ClientProperties,
) -> Result<()> {
    let mut columns = Vec::with_capacity(types.len());
    for (ty, name) in types.iter().zip(names) {
        let mut child = SchemaField::new(name);
        set_arrow_format(&mut child, ty, options)?;
        columns.push(child);
    }
    let holder = Box::into_raw(Box::new(DuckDbArrowSchemaHolder {
        columns,
        exported: ExportedSchemas::default(),
    }));
    // SAFETY: `holder` was just leaked; it is owned by the schema from here on, and the
    // fields the C structs point into no longer move.
    let holder_ref = unsafe { &mut *holder };
    let (children, count) = holder_ref.exported.export_children(&holder_ref.columns);
    *out_schema = ArrowSchema {
        format: c"+s".as_ptr(),
        name: c"duckdb_query_result".as_ptr(),
        metadata: std::ptr::null(),
        flags: 0,
        n_children: count,
        children,
        dictionary: std::ptr::null_mut(),
        release: Some(release_duckdb_arrow_schema),
        private_data: holder.cast::<c_void>(),
    };
    Ok(())
}

/// The fields of a STRUCT (`SetArrowStructFormat`); a MAP's keys are not nullable.
fn set_arrow_struct_format(
    child: &mut SchemaField,
    ty: &LogicalType,
    options: &ClientProperties,
    map_is_parent: bool,
) -> Result<()> {
    child.set_format(c"+s");
    for (name, child_type) in ty.struct_children().unwrap_or_default() {
        let mut field = SchemaField::new(name);
        set_arrow_format(&mut field, child_type, options)?;
        child.children.push(field);
    }
    if map_is_parent && let Some(keys) = child.children.first_mut() {
        keys.flags = 0;
    }
    Ok(())
}

/// A MAP: a list of non-nullable `entries` structs (`SetArrowMapFormat`).
fn set_arrow_map_format(child: &mut SchemaField, ty: &LogicalType, options: &ClientProperties) -> Result<()> {
    child.set_format(c"+m");
    let mut entries = SchemaField::new("entries");
    entries.flags = 0;
    let entry_type = ty
        .child_type()
        .ok_or_else(|| Error::internal("MAP type without an entry type"))?;
    set_arrow_struct_format(&mut entries, entry_type, options, true)?;
    child.children.push(entries);
    Ok(())
}

/// Exports `ty` as its registered extension type, if it has one (`SetArrowExtension`).
fn set_arrow_extension(
    child: &mut SchemaField,
    ty: &LogicalType,
    options: &ClientProperties,
) -> Result<bool> {
    match arrow_type_extensions().for_type(ty) {
        Some(extension) => {
            extension.populate_arrow_schema(child, ty, options)?;
            Ok(true)
        }
        None => Ok(false),
    }
}

/// A variable-size binary format: views from version 1.4 (the layout the appender
/// writes then), else regular or large binary.
pub(crate) fn binary_format(options: &ClientProperties) -> &'static CStr {
    if options.arrow_output_version >= ArrowFormatVersion::V1_4 {
        c"vz"
    } else if options.arrow_offset_size == ArrowOffsetSize::Large {
        c"Z"
    } else {
        c"z"
    }
}

/// The Arrow format of a column of type `ty` (`SetArrowFormat`).
fn set_arrow_format(child: &mut SchemaField, ty: &LogicalType, options: &ClientProperties) -> Result<()> {
    let is_json =
        ty.id() == LogicalTypeId::Varchar && ty.alias().is_some_and(|alias| alias.as_bytes() == b"JSON");
    // An aliased type may be an extension type; JSON only when exported losslessly.
    if ty.alias().is_some_and(|alias| !alias.is_empty())
        && (!is_json || options.arrow_lossless_conversion)
        && set_arrow_extension(child, ty, options)?
    {
        return Ok(());
    }
    let lossless = options.arrow_lossless_conversion;
    let large = options.arrow_offset_size == ArrowOffsetSize::Large;
    match ty.id() {
        // Types without an exact Arrow counterpart are extension types when lossless.
        LogicalTypeId::Boolean
        | LogicalTypeId::HugeInt
        | LogicalTypeId::UHugeInt
        | LogicalTypeId::Uuid
        | LogicalTypeId::TimeTz
        | LogicalTypeId::Bit
            if lossless =>
        {
            set_arrow_extension(child, ty, options)?;
        }
        LogicalTypeId::Boolean => child.set_format(c"b"),
        LogicalTypeId::TinyInt => child.set_format(c"c"),
        LogicalTypeId::SmallInt => child.set_format(c"s"),
        LogicalTypeId::Integer => child.set_format(c"i"),
        LogicalTypeId::BigInt => child.set_format(c"l"),
        LogicalTypeId::UTinyInt => child.set_format(c"C"),
        LogicalTypeId::USmallInt => child.set_format(c"S"),
        LogicalTypeId::UInteger => child.set_format(c"I"),
        LogicalTypeId::UBigInt => child.set_format(c"L"),
        LogicalTypeId::Float => child.set_format(c"f"),
        LogicalTypeId::HugeInt | LogicalTypeId::UHugeInt => child.set_format(c"d:38,0"),
        LogicalTypeId::Double => child.set_format(c"g"),
        // The appender writes UUIDs as (non-view) strings.
        LogicalTypeId::Uuid => child.set_format(if large { c"U" } else { c"u" }),
        LogicalTypeId::Varchar => {
            // String views were introduced in Arrow format 1.4.
            child.set_format(
                if options.produce_arrow_string_view
                    && options.arrow_output_version >= ArrowFormatVersion::V1_4
                {
                    c"vu"
                } else if large {
                    c"U"
                } else {
                    c"u"
                },
            );
        }
        LogicalTypeId::Date => child.set_format(c"tdD"),
        LogicalTypeId::TimeTz | LogicalTypeId::Time => child.set_format(c"ttu"),
        LogicalTypeId::TimeNs => child.set_format(c"ttn"),
        LogicalTypeId::Timestamp => child.set_format(c"tsu:"),
        LogicalTypeId::TimestampTz => child.set_owned_format(&format!("tsu:{}", options.time_zone)),
        LogicalTypeId::TimestampSec => child.set_format(c"tss:"),
        LogicalTypeId::TimestampNs => child.set_format(c"tsn:"),
        LogicalTypeId::TimestampMs => child.set_format(c"tsm:"),
        LogicalTypeId::Interval => child.set_format(c"tin"),
        LogicalTypeId::Decimal => {
            let (width, scale) = ty
                .decimal_properties()
                .ok_or_else(|| Error::internal("DECIMAL type without width and scale"))?;
            // Before version 1.5 all decimals are 128-bit.
            let bit_width = if options.arrow_output_version <= ArrowFormatVersion::V1_4 {
                128
            } else {
                match ty.physical_type() {
                    PhysicalType::Int16 | PhysicalType::Int32 => 32,
                    PhysicalType::Int64 => 64,
                    PhysicalType::Int128 => 128,
                    _ => {
                        return Err(Error::not_implemented(
                            "Unsupported internal type For DUCKDB Decimal -> Arrow ",
                        ));
                    }
                }
            };
            child.set_owned_format(&format!("d:{width},{scale},{bit_width}"));
        }
        LogicalTypeId::SqlNull => child.set_format(c"n"),
        LogicalTypeId::Blob | LogicalTypeId::Bit => child.set_format(binary_format(options)),
        LogicalTypeId::List => {
            // List views were introduced in Arrow format 1.4.
            child.set_format(
                match (
                    options.arrow_use_list_view && options.arrow_output_version >= ArrowFormatVersion::V1_4,
                    large,
                ) {
                    (true, true) => c"+vL",
                    (true, false) => c"+vl",
                    (false, true) => c"+L",
                    (false, false) => c"+l",
                },
            );
            let mut element = SchemaField::new("l");
            let child_type = ty
                .child_type()
                .ok_or_else(|| Error::internal("LIST type without a child type"))?;
            set_arrow_format(&mut element, child_type, options)?;
            child.children.push(element);
        }
        LogicalTypeId::Struct => set_arrow_struct_format(child, ty, options, false)?,
        LogicalTypeId::Array => {
            let (child_type, size) = ty
                .array_child()
                .ok_or_else(|| Error::internal("ARRAY type without a child type"))?;
            child.set_owned_format(&format!("+w:{size}"));
            let mut element = SchemaField::new("");
            set_arrow_format(&mut element, child_type, options)?;
            child.children.push(element);
        }
        LogicalTypeId::Map => set_arrow_map_format(child, ty, options)?,
        LogicalTypeId::Union => {
            let members = ty.union_members().unwrap_or_default();
            let mut format = String::from("+us:");
            for (index, (name, member_type)) in members.iter().enumerate() {
                let mut member = SchemaField::new(name);
                set_arrow_format(&mut member, member_type, options)?;
                child.children.push(member);
                if index > 0 {
                    format.push(',');
                }
                format.push_str(&index.to_string());
            }
            child.set_owned_format(&format);
        }
        LogicalTypeId::Enum => {
            child.set_format(match ty.physical_type() {
                PhysicalType::UInt8 => c"C",
                PhysicalType::UInt16 => c"S",
                PhysicalType::UInt32 => c"I",
                _ => return Err(Error::internal("Unsupported Enum Internal Type")),
            });
            // The dictionary values are always written as regular (int32 offset) strings.
            let mut dictionary = SchemaField::new("");
            dictionary.set_format(c"u");
            child.dictionary = Some(Box::new(dictionary));
        }
        // Other types may be exported as a registered extension.
        _ => {
            if !set_arrow_extension(child, ty, options)? {
                return Err(Error::not_implemented(format!("Unsupported Arrow type {ty}")));
            }
        }
    }
    Ok(())
}
