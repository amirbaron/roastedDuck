use std::any::Any;
use std::sync::{Arc, Mutex, PoisonError};

use apache_avro::Schema;
use apache_avro::types::Value as AvroValue;
use apache_avro::writer::datum::GenericDatumWriter;
use roastedduck_common::{ByteString, Error, Result};
use roastedduck_function::cast::ValueCast;
use roastedduck_function::copy::{CopyFunctionBindInput, CopyGlobalState};
use roastedduck_function::{CopyFunction, FunctionData};
use roastedduck_types::{LogicalType, LogicalTypeId, Value};
use serde_json::{Value as Json, json};

use super::container::{Writer, avro_error, parse_schema};

const EMPTY_STRUCT_MARKER: &[u8] = b"__duckdb_empty_struct_marker";
const BLOCK_TARGET: usize = 1024 * 1024;

#[derive(Debug, Default)]
struct FieldOptions {
    id: Option<i64>,
    required: bool,
    children: roastedduck_common::CaseInsensitiveMap<Self, ByteString>,
}

impl FieldOptions {
    fn parse(value: &Value) -> Result<Self> {
        if let Some(id) = value.as_i128() {
            return Ok(Self {
                id: Some(field_id(id)?),
                ..Self::default()
            });
        }
        let mut options = Self::default();
        for (name, child) in struct_fields(value, "FIELD_IDS")? {
            if name.eq_ignore_ascii_case(b"__duckdb_field_id") {
                options.id =
                    Some(field_id(child.as_i128().ok_or_else(|| {
                        Error::invalid_input("Avro field ID must be an integer")
                    })?)?);
            } else if name.eq_ignore_ascii_case(b"__duckdb_nullable") {
                let nullable = child.default_cast_as(&LogicalType::BOOLEAN)?;
                options.required = nullable.as_bool() == Some(false);
            } else {
                options.children.insert(name.clone(), Self::parse(child)?);
            }
        }
        Ok(options)
    }
}

fn field_id(value: i128) -> Result<i64> {
    let id = i32::try_from(value)
        .ok()
        .filter(|id| *id >= 0)
        .ok_or_else(|| Error::invalid_input("Avro field ID must be a nonnegative 32-bit integer"))?;
    Ok(i64::from(id))
}

fn struct_fields<'a>(
    value: &'a Value,
    option: &str,
) -> Result<impl Iterator<Item = (&'a ByteString, &'a Value)>> {
    let error = || Error::invalid_input(format!("'{option}' is expected to be provided as a STRUCT"));
    let fields = value.logical_type().struct_children().ok_or_else(error)?;
    let values = value.children().ok_or_else(error)?;
    Ok(fields.iter().zip(values).map(|((name, _), value)| (name, value)))
}

fn identifier(name: &[u8]) -> Result<&str> {
    let valid = name.iter().enumerate().all(|(index, byte)| {
        byte.is_ascii_alphabetic() || *byte == b'_' || index > 0 && byte.is_ascii_digit()
    });
    if valid && !name.is_empty() {
        return std::str::from_utf8(name).map_err(|error| Error::internal(error.to_string()));
    }
    Err(Error::invalid_input(roastedduck_common::byte_format!(
        "Invalid Avro identifier: '{name}'. The identifier has to match [A-Za-z_][A-Za-z0-9_]*"
    )))
}

fn nullable(schema: Json, options: &FieldOptions) -> Json {
    if options.required || schema == "null" {
        schema
    } else {
        json!(["null", schema])
    }
}

#[derive(Default)]
struct SchemaBuilder {
    next_name: usize,
}

impl SchemaBuilder {
    fn record<'a>(
        &mut self,
        schema_name: &str,
        fields: impl Iterator<Item = (&'a ByteString, &'a LogicalType)>,
        options: &FieldOptions,
    ) -> Result<Json> {
        let mut output = Vec::new();
        let defaults = FieldOptions::default();
        let mut seen = roastedduck_common::CaseInsensitiveSet::<ByteString>::default();
        for (name, ty) in fields {
            let field = options.children.get(name).unwrap_or(&defaults);
            let name = identifier(name)?;
            seen.insert(ByteString::from(name));
            let mut schema = json!({"name": name, "type": nullable(self.field(ty, field)?, field)});
            if let Some(id) = field.id {
                schema["field-id"] = json!(id);
            }
            output.push(schema);
        }
        for (name, _) in options.children.iter() {
            if !seen.contains(name) {
                return Err(Error::binder(roastedduck_common::byte_format!(
                    "Avro FIELD_IDS column \"{name}\" not found"
                )));
            }
        }
        Ok(json!({"type": "record", "name": schema_name, "fields": output}))
    }

    fn field(&mut self, ty: &LogicalType, options: &FieldOptions) -> Result<Json> {
        use LogicalTypeId as Id;

        self.next_name += 1;
        let name = format!("roastedduck.type_{}", self.next_name);
        let defaults = FieldOptions::default();
        Ok(match ty.id() {
            Id::SqlNull => json!("null"),
            Id::Boolean => json!("boolean"),
            Id::TinyInt | Id::SmallInt | Id::Integer | Id::UTinyInt | Id::USmallInt => json!("int"),
            Id::BigInt | Id::UInteger => json!("long"),
            Id::Float => json!("float"),
            Id::Double => json!("double"),
            Id::Varchar => json!("string"),
            Id::Blob => json!("bytes"),
            Id::Date => json!({"type": "int", "logicalType": "date"}),
            Id::Time => json!({"type": "long", "logicalType": "time-micros"}),
            Id::Timestamp | Id::TimestampMs | Id::TimestampSec => {
                json!({"type": "long", "logicalType": "timestamp-micros"})
            }
            Id::TimestampTz => {
                json!({"type": "long", "logicalType": "timestamp-micros", "adjust-to-utc": true})
            }
            Id::TimestampNs => json!({"type": "long", "logicalType": "timestamp-nanos"}),
            Id::Uuid => json!({"type": "fixed", "name": name, "size": 16, "logicalType": "uuid"}),
            Id::Decimal => {
                let (precision, scale) = ty
                    .decimal_properties()
                    .ok_or_else(|| Error::internal("Missing DECIMAL properties"))?;
                let digits = 10u128.pow(u32::from(precision));
                let bytes = (1..=16)
                    .find(|size| digits < 1u128 << (8 * size - 1))
                    .ok_or_else(|| Error::internal("Avro decimal precision is out of range"))?;
                json!({"type": "fixed", "name": name, "logicalType": "decimal", "precision": precision, "scale": scale, "size": bytes})
            }
            Id::Struct => self.record(
                &name,
                ty.struct_children()
                    .unwrap_or_default()
                    .iter()
                    .filter(|(name, _)| !name.eq_ignore_ascii_case(EMPTY_STRUCT_MARKER))
                    .map(|(name, ty)| (name, ty)),
                options,
            )?,
            Id::List => {
                let list = options.children.get("list").unwrap_or(&defaults);
                let item_type = ty
                    .child_type()
                    .ok_or_else(|| Error::internal("Missing LIST child type"))?;
                let item = nullable(self.field(item_type, list)?, list);
                let mut schema = json!({"type": "array", "items": item});
                if let Some(id) = list.id {
                    schema["element-id"] = json!(id);
                }
                schema
            }
            Id::Map => {
                let entry = ty
                    .child_type()
                    .ok_or_else(|| Error::internal("Missing MAP entry type"))?;
                let entry = self.field(entry, options)?;
                json!({"type": "array", "logicalType": "map", "items": entry})
            }
            _ => {
                return Err(Error::not_implemented(format!(
                    "Can't convert logical type '{ty}' to Avro type"
                )));
            }
        })
    }
}

#[derive(Debug)]
struct BindData {
    schema: Schema,
    json: Json,
    metadata: Vec<(String, String)>,
}
impl FunctionData for BindData {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn bind(
    input: &CopyFunctionBindInput<'_>,
    names: &[ByteString],
    types: &[LogicalType],
) -> Result<Arc<dyn FunctionData>> {
    let mut name = "root";
    let mut options = FieldOptions::default();
    let mut metadata = Vec::new();
    for (key, values) in input.options.iter() {
        let value = || {
            values.first().ok_or_else(|| {
                Error::invalid_input(roastedduck_common::byte_format!(
                    "{key} can not be provided without a value"
                ))
            })
        };
        if key.eq_ignore_ascii_case(b"root_name") {
            name =
                identifier(value()?.as_bytes().ok_or_else(|| {
                    Error::invalid_input("'ROOT_NAME' is expected to be provided as VARCHAR")
                })?)?;
        } else if key.eq_ignore_ascii_case(b"field_ids") {
            options = FieldOptions::parse(value()?)?;
        } else if key.eq_ignore_ascii_case(b"metadata") {
            for (key, value) in struct_fields(value()?, "METADATA")? {
                let key = std::str::from_utf8(key)
                    .map_err(|_| Error::invalid_input("Invalid Avro metadata key"))?;
                if key.starts_with("avro.") {
                    return Err(Error::invalid_input(format!("Reserved Avro metadata key: {key}")));
                }
                metadata.push((key.to_owned(), value.to_varchar()?));
            }
        } else {
            let value = values
                .first()
                .map(Value::to_varchar)
                .transpose()?
                .map(|value| format!(" with value: '{value}'"))
                .unwrap_or_default();
            return Err(Error::new(
                roastedduck_common::ErrorKind::InvalidConfiguration,
                format!(
                    "The following option(s) are not recognized: key: '{}'{value}",
                    key.to_ascii_uppercase().escape_ascii()
                ),
            ));
        }
    }
    let mut builder = SchemaBuilder::default();
    // Give nested generated names their own namespace, avoiding a collision
    // with any valid unqualified ROOT_NAME.
    let json = builder.record(name, names.iter().zip(types), &options)?;
    let schema = parse_schema(&json)?;
    Ok(Arc::new(BindData {
        schema,
        json,
        metadata,
    }))
}

fn encode(value: &Value, schema: &Schema) -> Result<AvroValue> {
    let mismatch =
        || Error::invalid_input(format!("Cannot encode {} as Avro {schema}", value.logical_type()));
    if let Schema::Union(union) = schema {
        let branch = usize::from(!value.is_null());
        let value = encode(value, &union.variants()[branch])?;
        return Ok(AvroValue::Union(branch as u32, Box::new(value)));
    }
    if value.is_null() {
        return if matches!(schema, Schema::Null) {
            Ok(AvroValue::Null)
        } else {
            Err(Error::invalid_input(format!(
                "Cannot insert NULL to non-nullable field of type {}",
                value.logical_type()
            )))
        };
    }
    Ok(match schema {
        Schema::Boolean => AvroValue::Boolean(value.as_bool().ok_or_else(mismatch)?),
        Schema::Int => {
            AvroValue::Int(i32::try_from(value.as_i128().ok_or_else(mismatch)?).map_err(|_| mismatch())?)
        }
        Schema::Long => {
            AvroValue::Long(i64::try_from(value.as_i128().ok_or_else(mismatch)?).map_err(|_| mismatch())?)
        }
        Schema::Float => AvroValue::Float(value.as_f32().ok_or_else(mismatch)?),
        Schema::Double => AvroValue::Double(value.as_f64().ok_or_else(mismatch)?),
        Schema::String => AvroValue::String(value.as_str().ok_or_else(mismatch)?.to_owned()),
        Schema::Bytes => AvroValue::Bytes(value.as_bytes().ok_or_else(mismatch)?.to_vec()),
        Schema::Date => AvroValue::Date(value.as_date().ok_or_else(mismatch)?.0),
        Schema::TimeMicros => AvroValue::TimeMicros(value.as_time().ok_or_else(mismatch)?.0),
        Schema::TimestampMicros => AvroValue::TimestampMicros(value.as_timestamp().ok_or_else(mismatch)?.0),
        Schema::LocalTimestampMicros => {
            let value_in_units = value.as_i64().ok_or_else(mismatch)?;
            let multiplier = match value.logical_type().id() {
                LogicalTypeId::TimestampMs => 1000,
                LogicalTypeId::TimestampSec => 1_000_000,
                _ => 1,
            };
            AvroValue::LocalTimestampMicros(value_in_units.checked_mul(multiplier).ok_or_else(mismatch)?)
        }
        Schema::TimestampNanos => AvroValue::TimestampNanos(value.as_i64().ok_or_else(mismatch)?),
        Schema::Uuid(_) => {
            let uuid = value
                .as_uuid()
                .ok_or_else(mismatch)?
                .to_string()
                .parse()
                .map_err(|_| mismatch())?;
            AvroValue::Uuid(uuid)
        }
        Schema::Decimal(_) => {
            let bytes = value.as_i128().ok_or_else(mismatch)?.to_be_bytes();
            AvroValue::Decimal(apache_avro::Decimal::new(bytes).map_err(avro_error)?)
        }
        Schema::Array(array) => {
            let values = value.children().ok_or_else(mismatch)?;
            AvroValue::Array(
                values
                    .iter()
                    .map(|value| encode(value, &array.items))
                    .collect::<Result<_>>()?,
            )
        }
        Schema::Record(record) => {
            let entries = value.children().ok_or_else(mismatch)?;
            let types = value.logical_type().struct_children().ok_or_else(mismatch)?;
            let entries = entries
                .iter()
                .zip(types)
                .filter(|(_, (name, _))| !name.eq_ignore_ascii_case(EMPTY_STRUCT_MARKER));
            encode_record(record, entries.map(|(value, _)| value))?
        }
        _ => return Err(mismatch()),
    })
}

fn encode_record<'a>(
    schema: &apache_avro::schema::RecordSchema,
    values: impl Iterator<Item = &'a Value>,
) -> Result<AvroValue> {
    Ok(AvroValue::Record(
        schema
            .fields
            .iter()
            .zip(values)
            .map(|(field, value)| Ok((field.name.clone(), encode(value, &field.schema)?)))
            .collect::<Result<_>>()?,
    ))
}

fn bind_data(data: &dyn FunctionData) -> Result<&BindData> {
    data.as_any()
        .downcast_ref::<BindData>()
        .ok_or_else(|| Error::internal("Missing Avro COPY bind data"))
}
fn writer(state: &CopyGlobalState) -> Result<&Mutex<Option<Writer>>> {
    state
        .downcast_ref::<Mutex<Option<Writer>>>()
        .ok_or_else(|| Error::internal("Missing Avro COPY writer"))
}

pub(super) fn copy_function() -> CopyFunction {
    let mut function = CopyFunction::new("avro");
    function.extension = "avro".into();
    function.copy_to_bind = Some(Arc::new(|input, names, types| bind(input, names, types)));
    function.copy_to_initialize_global = Some(Arc::new(|data, path| {
        let data = bind_data(data)?;
        Ok(Box::new(Mutex::new(Some(Writer::new(
            path,
            &data.json,
            &data.metadata,
        )?))))
    }));
    function.copy_to_sink = Some(Arc::new(|data, state, chunk| {
        let data = bind_data(data)?;
        let Schema::Record(record) = &data.schema else {
            return Err(Error::internal("Invalid root Avro COPY schema"));
        };
        let encoder = GenericDatumWriter::builder(&data.schema)
            .build()
            .map_err(avro_error)?;
        let mut guard = writer(state)?.lock().unwrap_or_else(PoisonError::into_inner);
        let output = guard
            .as_mut()
            .ok_or_else(|| Error::internal("Avro COPY writer is closed"))?;

        let mut block = Vec::new();
        let mut count = 0;
        for row in 0..chunk.len() {
            let values = chunk
                .columns()
                .iter()
                .map(|column| column.get_value(row))
                .collect::<Vec<_>>();
            let datum = encode_record(record, values.iter())?;
            encoder.write_value(&mut block, datum).map_err(avro_error)?;
            count += 1;
            if block.len() >= BLOCK_TARGET {
                output.write_block(&block, count)?;
                block.clear();
                count = 0;
            }
        }
        if count != 0 {
            output.write_block(&block, count)?;
        }
        Ok(())
    }));
    function.copy_to_finalize = Some(Arc::new(|_, state| {
        if let Some(output) = writer(state)?
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            output.close()?;
        }
        Ok(())
    }));
    function
}
