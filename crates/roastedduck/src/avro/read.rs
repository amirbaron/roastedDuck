use std::any::Any;
use std::sync::Arc;

use apache_avro::Schema;
use apache_avro::schema::{Name, ResolvedSchema};
use apache_avro::types::Value as AvroValue;
use roastedduck_common::{ByteString, Error, Result, STANDARD_VECTOR_SIZE};
use roastedduck_csv::multi_file::{
    COLUMN_IDENTIFIER_FILE_INDEX, COLUMN_IDENTIFIER_FILENAME, MultiFileOptions, bind_options,
    create_file_list, hive_parse, multi_file_parameters, multi_file_virtual_columns, parse_paths,
};
use roastedduck_csv::read_csv::{client_context, transform_string_to_logical_type};
use roastedduck_function::cast::{CastFunctionSet, CastParameters, ValueCast};
use roastedduck_function::{
    FunctionData, TableFunction, TableFunctionBindInput, TableFunctionBindOutput, TableFunctionSet,
    TableScanState,
};
use roastedduck_planner::operator::COLUMN_IDENTIFIER_EMPTY;
use roastedduck_types::{DataChunk, Date, LogicalType, LogicalTypeId, Time, Timestamp, Uuid, Value, Vector};

use super::container::{Reader, avro_error};

fn logical_type(
    schema: &Schema,
    names: &ResolvedSchema<'_>,
    parents: &mut Vec<Name>,
    depth: usize,
) -> Result<LogicalType> {
    if depth > 128 {
        return Err(Error::invalid_input("Avro schema nesting exceeds 128 levels"));
    }
    let mut child = |schema| logical_type(schema, names, parents, depth + 1);
    Ok(match schema {
        Schema::Null => LogicalType::SQLNULL,
        Schema::Boolean => LogicalType::BOOLEAN,
        Schema::Int => LogicalType::INTEGER,
        Schema::Long => LogicalType::BIGINT,
        Schema::Float => LogicalType::FLOAT,
        Schema::Double => LogicalType::DOUBLE,
        Schema::String => LogicalType::VARCHAR,
        Schema::Bytes | Schema::Fixed(_) | Schema::Duration(_) => LogicalType::BLOB,
        Schema::Date => LogicalType::DATE,
        Schema::TimeMillis | Schema::TimeMicros => LogicalType::TIME,
        Schema::TimestampMillis | Schema::TimestampMicros => LogicalType::TIMESTAMP_TZ,
        Schema::LocalTimestampMillis | Schema::LocalTimestampMicros => LogicalType::TIMESTAMP,
        Schema::TimestampNanos | Schema::LocalTimestampNanos => LogicalType::TIMESTAMP_NS,
        Schema::Uuid(_) => LogicalType::UUID,
        Schema::Decimal(decimal) => {
            let width = decimal.precision.get();
            if width > 38 || decimal.scale > width {
                return Err(Error::invalid_input(
                    "Avro decimal exceeds DuckDB's decimal precision",
                ));
            }
            LogicalType::decimal(width as u8, decimal.scale as u8)
        }
        Schema::BigDecimal => return Err(Error::not_implemented("Avro big-decimal is not supported")),
        Schema::Enum(enumeration) => LogicalType::enum_type(enumeration.symbols.clone()),
        Schema::Ref { name } => {
            if parents.contains(name) {
                return Err(Error::invalid_input(format!(
                    "Recursive Avro types not supported: {}",
                    name.name()
                )));
            }
            let target = names
                .get_names()
                .get(name)
                .ok_or_else(|| Error::invalid_input(format!("Unresolved Avro schema: {}", name.name())))?;
            logical_type(target, names, parents, depth + 1)?
        }
        Schema::Record(record) => {
            if record.fields.is_empty() {
                return Ok(LogicalType::SQLNULL);
            }
            parents.push(record.name.clone());
            let fields = record
                .fields
                .iter()
                .map(|field| {
                    Ok((
                        field.name.as_str().into(),
                        logical_type(&field.schema, names, parents, depth + 1)?,
                    ))
                })
                .collect::<Result<Vec<_>>>()?;
            parents.pop();
            LogicalType::struct_type(fields)
        }
        Schema::Map(map) => LogicalType::map(LogicalType::VARCHAR, child(&map.types)?),
        Schema::Array(array) => {
            let element = child(&array.items)?;
            if array
                .attributes
                .get("logicalType")
                .and_then(serde_json::Value::as_str)
                == Some("map")
            {
                let Some([(_, key), (_, value)]) = element.struct_children() else {
                    return Err(Error::invalid_input(
                        "Avro map array must contain key/value records",
                    ));
                };
                LogicalType::map(key.clone(), value.clone())
            } else {
                LogicalType::list(element)
            }
        }
        Schema::Union(union) => {
            let mut fields = Vec::new();
            for (index, variant) in union.variants().iter().enumerate() {
                let ty = child(variant)?;
                if ty != LogicalType::SQLNULL {
                    fields.push((format!("u{index}").into(), ty));
                }
            }
            match fields.len() {
                0 => LogicalType::SQLNULL,
                1 => fields.remove(0).1,
                2..=255 => LogicalType::union_type(fields),
                _ => return Err(Error::invalid_input("Avro union exceeds 255 non-null branches")),
            }
        }
    })
}

fn signed_decimal(bytes: &[u8]) -> Result<i128> {
    let Some(first) = bytes.first() else {
        return Err(Error::invalid_input("Empty Avro decimal"));
    };
    let sign_byte = if first & 0x80 == 0 { 0 } else { 0xff };
    let mut padded = [sign_byte; 16];
    let extra = bytes.len().saturating_sub(padded.len());
    let data = &bytes[extra..];
    if bytes[..extra].iter().any(|&byte| byte != sign_byte) || data[0] & 0x80 != sign_byte & 0x80 {
        return Err(Error::invalid_input("Avro decimal is out of range"));
    }
    let start = padded.len() - data.len();
    padded[start..].copy_from_slice(data);
    Ok(i128::from_be_bytes(padded))
}

fn convert(value: AvroValue, ty: &LogicalType) -> Result<Value> {
    let mismatch = || Error::invalid_input(format!("Avro datum does not match {ty}"));
    let convert_timestamp = |micros| {
        if ty == &LogicalType::TIMESTAMP_TZ {
            Value::timestamp_tz(Timestamp(micros))
        } else {
            Value::timestamp(Timestamp(micros))
        }
    };
    if ty == &LogicalType::SQLNULL {
        return Ok(Value::null(ty.clone()));
    }
    Ok(match value {
        AvroValue::Null => Value::null(ty.clone()),
        AvroValue::Boolean(value) => Value::boolean(value),
        AvroValue::Int(value) => Value::integer(value),
        AvroValue::Long(value) => Value::bigint(value),
        AvroValue::Float(value) => Value::float(value),
        AvroValue::Double(value) => Value::double(value),
        AvroValue::String(value) => Value::try_varchar(value.as_bytes())?,
        AvroValue::Bytes(value) | AvroValue::Fixed(_, value) => Value::blob(&value),
        AvroValue::Duration(value) => Value::blob(&<[u8; 12]>::from(value)),
        AvroValue::Date(days) => Value::date(Date(days)),
        AvroValue::TimeMillis(millis) => Value::time(Time(i64::from(millis) * 1000)),
        AvroValue::TimeMicros(micros) => Value::time(Time(micros)),
        AvroValue::TimestampMillis(millis) | AvroValue::LocalTimestampMillis(millis) => convert_timestamp(
            millis
                .checked_mul(1000)
                .ok_or_else(|| Error::invalid_input("Avro timestamp is out of range"))?,
        ),
        AvroValue::TimestampMicros(micros) | AvroValue::LocalTimestampMicros(micros) => {
            convert_timestamp(micros)
        }
        AvroValue::TimestampNanos(nanos) | AvroValue::LocalTimestampNanos(nanos) => {
            Value::timestamp_ns(nanos)
        }
        AvroValue::Uuid(uuid) => Value::uuid(Uuid::from_u128(uuid.as_u128())),
        AvroValue::Decimal(decimal) => {
            let (width, scale) = ty.decimal_properties().ok_or_else(mismatch)?;
            let bytes = Vec::<u8>::try_from(decimal).map_err(avro_error)?;
            let number = signed_decimal(&bytes)?;
            if number.unsigned_abs() >= 10u128.pow(u32::from(width)) {
                return Err(Error::invalid_input(
                    "Avro decimal exceeds its declared precision",
                ));
            }
            Value::decimal(number, width, scale)
        }
        AvroValue::Enum(index, _) => Value::enum_value(ty.clone(), index),
        AvroValue::Array(values) => {
            if let Some((key_type, value_type)) = ty.map_key_value() {
                let entries = values
                    .into_iter()
                    .map(|entry| {
                        let AvroValue::Record(fields) = entry else {
                            return Err(mismatch());
                        };
                        let [(_, key), (_, value)]: [(String, AvroValue); 2] =
                            fields.try_into().map_err(|_| mismatch())?;
                        Ok((convert(key, key_type)?, convert(value, value_type)?))
                    })
                    .collect::<Result<Vec<_>>>()?;
                return Ok(Value::map(key_type.clone(), value_type.clone(), entries));
            }
            let child = ty.child_type().ok_or_else(mismatch)?;
            let values = values
                .into_iter()
                .map(|value| convert(value, child))
                .collect::<Result<_>>()?;
            Value::from_children(ty.clone(), values)
        }
        AvroValue::Map(values) => {
            let (key, child) = ty.map_key_value().ok_or_else(mismatch)?;
            let mut values = values.into_iter().collect::<Vec<_>>();
            values.sort_unstable_by(|(left, _), (right, _)| left.cmp(right));
            let values = values
                .into_iter()
                .map(|(key, value)| Ok((Value::try_varchar(key.as_bytes())?, convert(value, child)?)))
                .collect::<Result<_>>()?;
            Value::map(key.clone(), child.clone(), values)
        }
        AvroValue::Record(fields) => {
            let children = ty.struct_children().ok_or_else(mismatch)?;
            let values = fields
                .into_iter()
                .zip(children)
                .map(|((_, value), (_, ty))| convert(value, ty))
                .collect::<Result<_>>()?;
            Value::from_children(ty.clone(), values)
        }
        AvroValue::Union(index, value) => {
            if matches!(*value, AvroValue::Null)
                || matches!(&*value, AvroValue::Record(fields) if fields.is_empty())
            {
                return Ok(Value::null(ty.clone()));
            }
            let Some(members) = ty.union_members() else {
                return convert(*value, ty);
            };
            let name = format!("u{index}");
            let (tag, (_, child)) = members
                .iter()
                .enumerate()
                .find(|(_, (member, _))| member.as_bytes() == name.as_bytes())
                .ok_or_else(mismatch)?;
            Value::union_value(ty.clone(), tag as u8, convert(*value, child)?)
        }
        AvroValue::BigDecimal(_) => return Err(mismatch()),
    })
}

#[derive(Debug, PartialEq, Eq)]
struct Layout {
    root: LogicalType,
    flatten_record: bool,
    names: Vec<ByteString>,
    types: Vec<LogicalType>,
}

impl Layout {
    fn bind(schema: &Schema) -> Result<Self> {
        let resolved = ResolvedSchema::try_from(schema).map_err(avro_error)?;
        let root = logical_type(schema, &resolved, &mut Vec::new(), 0)?;
        let flatten_record = matches!(schema, Schema::Record(_)) && root.id() == LogicalTypeId::Struct;
        let (names, types) = if flatten_record {
            root.struct_children().unwrap_or_default().iter().cloned().unzip()
        } else {
            (
                vec![schema.name().map_or("avro_schema", Name::name).into()],
                vec![root.clone()],
            )
        };
        Ok(Self {
            root,
            flatten_record,
            names,
            types,
        })
    }
}

#[derive(Debug)]
struct AvroScan {
    paths: Vec<ByteString>,
    layout: Layout,
    types: Vec<LogicalType>,
    constants: Vec<Vec<(usize, Value)>>,
    virtual_filename: bool,
    casts: Arc<CastFunctionSet>,
}
impl FunctionData for AvroScan {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn equals(&self, other: &dyn FunctionData) -> bool {
        other.as_any().downcast_ref::<Self>().is_some_and(|other| {
            self.paths == other.paths
                && self.layout == other.layout
                && self.types == other.types
                && self.constants == other.constants
                && self.virtual_filename == other.virtual_filename
        })
    }
}

fn bind(input: &TableFunctionBindInput<'_>) -> Result<TableFunctionBindOutput> {
    let context = client_context(input)?;
    let mut options = MultiFileOptions::default();
    for (name, value) in input.named_parameters {
        options.parse_option(name, value, &|name| {
            transform_string_to_logical_type(context, name)
        })?;
    }
    if options.union_by_name {
        return Err(Error::not_implemented(
            "'union_by_name' not implemented for Avro reader yet",
        ));
    }

    let paths = parse_paths("read_avro", &input.inputs[0])?;
    let files = create_file_list("read_avro", &paths, "avro")?;
    let layout = Layout::bind(&Reader::open(&files.files[0])?.schema)?;
    let mut names = layout.names.clone();
    let mut types = layout.types.clone();
    let casts = context.casts.clone();
    options.auto_detect_hive_partitioning(&files, &casts)?;
    let reader_bind = bind_options(&options, &files, &mut types, &mut names)?;
    let constants = files
        .files
        .iter()
        .map(|path| {
            let mut columns = Vec::new();
            if let Some(index) = reader_bind.filename_idx {
                columns.push((index, Value::try_varchar(path)?));
            }
            if !reader_bind.hive_partitioning_indexes.is_empty() {
                let partitions = hive_parse(path);
                for (key, index) in &reader_bind.hive_partitioning_indexes {
                    let value = partitions.get(key).cloned().unwrap_or_default();
                    columns.push((*index, options.hive_partition_value(&value, key, &casts)?));
                }
            }
            Ok(columns)
        })
        .collect::<Result<_>>()?;

    Ok(TableFunctionBindOutput {
        names,
        return_types: types.clone(),
        bind_data: Some(Arc::new(AvroScan {
            paths: files.files,
            layout,
            types,
            constants,
            virtual_filename: !options.filename,
            casts,
        })),
    })
}

#[derive(Default)]
struct ScanState {
    next_file: usize,
    current: Option<ScanFile>,
}

struct ScanFile {
    reader: Reader,
    layout: Layout,
    mapping: Vec<Option<usize>>,
}
impl ScanFile {
    fn open(path: &[u8], target: &Layout) -> Result<Self> {
        let reader = Reader::open(path)?;
        let layout = Layout::bind(&reader.schema)?;
        let names = layout
            .names
            .iter()
            .enumerate()
            .map(|(index, name)| (name.clone(), index))
            .collect::<roastedduck_common::CaseInsensitiveMap<usize, ByteString>>();
        let mapping = target.names.iter().map(|name| names.get(name).copied()).collect();
        Ok(Self {
            reader,
            layout,
            mapping,
        })
    }
}

fn scan(
    data: Option<&dyn FunctionData>,
    columns: &[u64],
    state: &mut TableScanState,
    chunk: &mut DataChunk,
) -> Result<bool> {
    let data = data
        .and_then(|data| data.as_any().downcast_ref::<AvroScan>())
        .ok_or_else(|| Error::internal("Missing Avro scan bind data"))?;
    let state = state
        .local
        .get_or_insert_with(|| Box::<ScanState>::default())
        .downcast_mut::<ScanState>()
        .ok_or_else(|| Error::internal("Invalid Avro scan state"))?;
    chunk.reset();
    loop {
        if state.current.is_none() {
            let Some(path) = data.paths.get(state.next_file) else {
                return Ok(false);
            };
            state.current = Some(ScanFile::open(path, &data.layout)?);
            state.next_file += 1;
        }
        let file_index = state.next_file - 1;
        let path = &data.paths[file_index];
        let file_constants = &data.constants[file_index];
        let file = state
            .current
            .as_mut()
            .ok_or_else(|| Error::internal("Missing Avro file reader"))?;
        let mut vectors = Vec::with_capacity(columns.len());
        let mut source_columns = Vec::new();
        for &column in columns {
            let index = column as usize;
            let constant = match column {
                COLUMN_IDENTIFIER_FILENAME => Some(Value::try_varchar(path)?),
                COLUMN_IDENTIFIER_FILE_INDEX => Some(Value::ubigint(file_index as u64)),
                COLUMN_IDENTIFIER_EMPTY => Some(Value::boolean(true)),
                _ => file_constants
                    .iter()
                    .find(|(position, _)| *position == index)
                    .map(|(_, value)| value.clone()),
            };
            if let Some(value) = constant {
                vectors.push(Vector::constant(&value)?);
            } else if let Some(ty) = data.types.get(index) {
                let source_index = file.mapping.get(index).and_then(|entry| *entry).ok_or_else(|| {
                    let name = &data.layout.names[index];
                    Error::invalid_input(roastedduck_common::byte_format!(
                        "Avro schema mismatch in glob: {path} is missing column {name}"
                    ))
                })?;
                source_columns.push((vectors.len(), source_index, Value::null(ty.clone())));
                vectors.push(Vector::flat(ty.clone(), STANDARD_VECTOR_SIZE));
            } else {
                vectors.push(Vector::constant_null(LogicalType::BIGINT));
            }
        }
        let mut row = 0;
        let count = file.reader.read(STANDARD_VECTOR_SIZE, |datum| {
            if !source_columns.is_empty() {
                let value = convert(datum, &file.layout.root)?;
                let fields = value.children();
                for (output, index, null) in &source_columns {
                    let field = if file.layout.flatten_record {
                        fields.and_then(|fields| fields.get(*index)).unwrap_or(null)
                    } else {
                        &value
                    };
                    let ty = null.logical_type();
                    if field.logical_type() == ty || field.is_null() {
                        vectors[*output].set_value(row, field)?;
                    } else {
                        let field = field.cast_with(&data.casts, ty, &mut CastParameters::new())?;
                        vectors[*output].set_value(row, &field)?;
                    }
                }
            }
            row += 1;
            Ok(())
        })?;
        if count != 0 {
            chunk.set_columns(vectors, count);
            return Ok(true);
        }
        state.current = None;
    }
}

pub(super) fn functions() -> TableFunctionSet {
    let functions = [LogicalType::VARCHAR, LogicalType::list(LogicalType::VARCHAR)]
        .into_iter()
        .map(|argument| {
            let mut function = TableFunction::new("read_avro", vec![argument])
                .with_bind(bind)
                .with_scan(scan);
            function.named_parameters = multi_file_parameters();
            function.projection_pushdown = true;
            function.projected_output = true;
            function.empty_column = true;
            function.get_virtual_columns = Some(Arc::new(|data| {
                let filename_is_virtual = data
                    .and_then(|data| data.as_any().downcast_ref::<AvroScan>())
                    .is_some_and(|data| data.virtual_filename);
                multi_file_virtual_columns(!filename_is_virtual)
            }));
            function
        })
        .collect();
    TableFunctionSet {
        name: "read_avro".into(),
        functions,
    }
}
