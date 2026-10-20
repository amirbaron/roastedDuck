//! Building JSON from SQL values (DuckDB's `json_create.cpp`): `to_json` / `json_quote`,
//! `json_array`, `json_object`, `array_to_json`, `row_to_json` and the casts of any type
//! to JSON.
//!
//! DuckDB builds a mutable yyjson document per chunk and writes it out; the text only
//! depends on the values, so this writes each row's JSON directly: the argument vectors are
//! first converted to the types JSON can hold (casting dates, decimals, ... like
//! `CreateValues`), then every row is serialized by walking the converted vectors.

use std::any::Any;
use std::sync::Arc;

use rustdb_common::{Error, Result};
use rustdb_function::cast::{BoundCastInfo, CastFunctionSet, CastParameters, cast_vector};
use rustdb_function::{
    BindInput, FunctionData, FunctionState, NullHandling, ScalarFunction, ScalarFunctionSet,
};
use rustdb_types::{
    DataChunk, ListEntry, LogicalType, LogicalTypeId, Sel, StringT, Unified, ValidityMask, Vector, VectorKind,
};

use crate::json_executors::{ColumnBuilder, StrBuilder, all_constant, local_arena};
use crate::json_functions::is_json_type;
use crate::json_type;
use crate::yyjson::{JsonArena, write_f64, write_raw, write_string, write_val};

/// The bind data of the create functions: the casts of the calling context (used for
/// TIMESTAMP WITH TIME ZONE, which converts in the session time zone).
#[derive(Debug)]
pub(crate) struct JsonCreateFunctionData {
    casts: Arc<CastFunctionSet>,
}

impl FunctionData for JsonCreateFunctionData {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn equals(&self, other: &dyn FunctionData) -> bool {
        other.as_any().is::<Self>()
    }
}

/// `GetJSONType`: the type a value is converted to before it is put into JSON.
pub(crate) fn get_json_type(ty: &LogicalType) -> LogicalType {
    use LogicalTypeId as L;
    if is_json_type(ty) {
        return ty.clone();
    }
    match ty.id() {
        L::SqlNull
        | L::Boolean
        | L::TinyInt
        | L::SmallInt
        | L::Integer
        | L::BigInt
        | L::HugeInt
        | L::UHugeInt
        | L::UTinyInt
        | L::USmallInt
        | L::UInteger
        | L::UBigInt
        | L::Float
        | L::Double
        | L::Bit
        | L::Blob
        | L::Varchar
        | L::AggregateState
        | L::Enum
        | L::Date
        | L::Interval
        | L::Time
        | L::TimeTz
        | L::Timestamp
        | L::TimestampTz
        | L::TimestampNs
        | L::TimestampMs
        | L::TimestampSec
        | L::Uuid
        | L::Bignum
        | L::Decimal => ty.clone(),
        L::IntegerLiteral => ty.integer_literal_type().cloned().unwrap_or(LogicalType::INTEGER),
        L::List => LogicalType::list(get_json_type(ty.child_type().unwrap_or(&LogicalType::SQLNULL))),
        L::Array => match ty.array_child() {
            Some((child, size)) => LogicalType::array(get_json_type(child), size),
            None => ty.clone(),
        },
        L::Struct => LogicalType::struct_type(
            ty.struct_children()
                .unwrap_or_default()
                .iter()
                .map(|(name, child)| (name.clone(), get_json_type(child)))
                .collect(),
        ),
        L::Map => match ty.map_key_value() {
            Some((_, value)) => LogicalType::map(LogicalType::VARCHAR, get_json_type(value)),
            None => ty.clone(),
        },
        L::Union => LogicalType::union_type(
            ty.union_members()
                .unwrap_or_default()
                .iter()
                .map(|(name, member)| (name.clone(), get_json_type(member)))
                .collect(),
        ),
        _ => LogicalType::VARCHAR,
    }
}

/// The casts of the binder's client context, or DuckDB's default casts.
fn context_casts(input: &BindInput<'_>) -> Arc<CastFunctionSet> {
    input
        .binder
        .and_then(|binder| binder.downcast_ref::<rustdb_planner::ClientContext>())
        .map_or_else(
            || Arc::new(CastFunctionSet::new()),
            |context| Arc::clone(&context.casts),
        )
}

/// `JSONCreateBindParams`.
fn create_bind(
    function: &mut ScalarFunction,
    input: &BindInput<'_>,
    object: bool,
) -> Result<Option<Arc<dyn FunctionData>>> {
    function.arguments.clear();
    for (i, ty) in input.argument_types.iter().enumerate() {
        if ty.id() == LogicalTypeId::Unknown {
            return Err(rustdb_planner::expression_binder::parameter_not_resolved());
        }
        if object && i % 2 == 0 {
            if !matches!(ty.id(), LogicalTypeId::Varchar | LogicalTypeId::StringLiteral)
                || ty.alias().is_some()
            {
                let name = input.names.get(i).map_or("", String::as_str);
                return Err(Error::binder(format!(
                    "json_object() keys must be VARCHAR, add an explicit cast to argument \"{name}\""
                )));
            }
            function.arguments.push(LogicalType::VARCHAR);
        } else {
            function.arguments.push(get_json_type(ty));
        }
    }
    Ok(Some(Arc::new(JsonCreateFunctionData {
        casts: context_casts(input),
    })))
}

/// A vector converted to what JSON can hold, ready to be written row by row.
#[derive(Debug)]
enum Node {
    Null,
    Bool(Vector),
    Signed(Vector),
    Unsigned(Vector),
    Double(Vector),
    /// Text written as a JSON string.
    Str(Vector),
    /// Text written verbatim (huge integers, bignums, wide decimals).
    Raw(Vector),
    /// JSON text, parsed and written minified.
    Json(Vector),
    Struct(Vector, Vec<(Vec<u8>, Node)>),
    List(Vector, Box<Node>),
    Array(Vector, u32, Box<Node>),
    /// Map entries whose keys are VARCHAR.
    Map(Vector, Box<Node>, Box<Node>),
    /// Union tags and members.
    Union(Vector, Vec<(Vec<u8>, Node)>),
}

/// A JSON object key (`"name":`), escaped.
fn object_key(name: &str) -> Vec<u8> {
    let mut key = Vec::with_capacity(name.len() + 3);
    write_string(&mut key, name.as_bytes());
    key.push(b':');
    key
}

/// Casts the first `count` rows of `vector` to `target`.
fn cast(casts: &CastFunctionSet, vector: &Vector, target: &LogicalType, count: usize) -> Result<Vector> {
    cast_vector(casts, vector, target, count, &mut CastParameters::new())
}

/// The number of rows a nested vector's child holds for `count` parent rows.
fn list_child_count(vector: &Vector) -> usize {
    vector.list_child().map_or(0, |(_, size)| size)
}

/// `vector` as a flat vector of `count` rows (nested children are indexed by slot).
fn flat_parent(vector: Vector, count: usize) -> Vector {
    if vector.kind() == VectorKind::Flat {
        vector
    } else {
        vector.flattened(count)
    }
}

/// `CreateValues`: converts `vector` (first `count` rows).
fn convert(casts: &CastFunctionSet, vector: &Vector, count: usize) -> Result<Node> {
    use LogicalTypeId as L;
    let ty = vector.logical_type().clone();
    let vector = if vector.kind() == VectorKind::Sequence {
        vector.flattened(count)
    } else {
        vector.clone()
    };
    if is_json_type(&ty) {
        return Ok(Node::Json(vector));
    }
    Ok(match ty.id() {
        L::SqlNull => Node::Null,
        L::Boolean => Node::Bool(vector),
        L::TinyInt | L::SmallInt | L::Integer | L::BigInt => {
            Node::Signed(cast(casts, &vector, &LogicalType::BIGINT, count)?)
        }
        L::UTinyInt | L::USmallInt | L::UInteger | L::UBigInt => {
            Node::Unsigned(cast(casts, &vector, &LogicalType::UBIGINT, count)?)
        }
        L::Float | L::Double => Node::Double(cast(casts, &vector, &LogicalType::DOUBLE, count)?),
        L::Varchar => Node::Str(vector),
        L::HugeInt | L::UHugeInt | L::Bignum => {
            Node::Raw(cast(casts, &vector, &LogicalType::VARCHAR, count)?)
        }
        L::Decimal => {
            let width = ty.decimal_properties().map_or(18, |(width, _)| width);
            if width > 15 {
                Node::Raw(cast(casts, &vector, &LogicalType::VARCHAR, count)?)
            } else {
                Node::Double(cast(casts, &vector, &LogicalType::DOUBLE, count)?)
            }
        }
        L::Struct => {
            // Field vectors are indexed by the parent's slots: work on a flat parent.
            let vector = flat_parent(vector, count);
            let names: Vec<String> = ty
                .struct_children()
                .unwrap_or_default()
                .iter()
                .map(|(name, _)| name.clone())
                .collect();
            let mut fields = Vec::with_capacity(names.len());
            for (name, child) in names.iter().zip(vector.struct_children().unwrap_or_default()) {
                fields.push((object_key(name), convert(casts, child, count)?));
            }
            Node::Struct(vector, fields)
        }
        L::List => {
            let child = match vector.list_child() {
                Some((child, size)) => convert(casts, child, size)?,
                None => Node::Null,
            };
            Node::List(vector, Box::new(child))
        }
        L::Map => {
            let size = list_child_count(&vector);
            let (keys, values) = match vector
                .list_child()
                .and_then(|(entries, _)| entries.struct_children())
            {
                Some([keys, values]) => (
                    Node::Str(cast(casts, keys, &LogicalType::VARCHAR, size)?),
                    convert(casts, values, size)?,
                ),
                _ => (Node::Null, Node::Null),
            };
            Node::Map(vector, Box::new(keys), Box::new(values))
        }
        L::Array => {
            let flat = vector.flattened(count);
            let size = ty.array_child().map_or(0, |(_, size)| size);
            let child = match flat.array_child() {
                Some(child) => convert(casts, child, count * size as usize)?,
                None => Node::Null,
            };
            Node::Array(flat, size, Box::new(child))
        }
        L::Union => {
            let names: Vec<String> = ty
                .union_members()
                .unwrap_or_default()
                .iter()
                .map(|(name, _)| name.clone())
                .collect();
            let vector = flat_parent(vector, count);
            let children = vector.struct_children().unwrap_or_default();
            let mut members = Vec::with_capacity(names.len());
            for (name, member) in names.iter().zip(children.iter().skip(1)) {
                members.push((object_key(name), convert(casts, member, count)?));
            }
            Node::Union(vector, members)
        }
        L::Variant => Node::Raw(
            crate::variant_to_json::variant_column_to_json(&vector, count)?.finish(&LogicalType::VARCHAR)?,
        ),
        _ => Node::Str(cast(casts, &vector, &LogicalType::VARCHAR, count)?),
    })
}

/// Row access to a converted vector.
enum Prepared<'a> {
    Null,
    Bool(Unified<'a, bool>),
    Signed(Unified<'a, i64>),
    Unsigned(Unified<'a, u64>),
    Double(Unified<'a, f64>),
    Str(&'a Vector, Unified<'a, StringT>),
    Raw(&'a Vector, Unified<'a, StringT>),
    Json(&'a Vector, Unified<'a, StringT>),
    Struct(Sel<'a>, &'a ValidityMask, Vec<(&'a [u8], Prepared<'a>)>),
    List(Unified<'a, ListEntry>, Box<Prepared<'a>>),
    Array(Sel<'a>, &'a ValidityMask, usize, Box<Prepared<'a>>),
    Map(Unified<'a, ListEntry>, Box<Prepared<'a>>, Box<Prepared<'a>>),
    Union(
        Sel<'a>,
        &'a ValidityMask,
        Unified<'a, u8>,
        Vec<(&'a [u8], Prepared<'a>)>,
    ),
}

impl<'a> Prepared<'a> {
    fn new(node: &'a Node) -> Self {
        match node {
            Node::Null => Self::Null,
            Node::Bool(v) => Self::Bool(v.unified()),
            Node::Signed(v) => Self::Signed(v.unified()),
            Node::Unsigned(v) => Self::Unsigned(v.unified()),
            Node::Double(v) => Self::Double(v.unified()),
            Node::Str(v) => Self::Str(v, v.unified()),
            Node::Raw(v) => Self::Raw(v, v.unified()),
            Node::Json(v) => Self::Json(v, v.unified()),
            Node::Struct(v, fields) => Self::Struct(
                v.sel(),
                v.validity(),
                fields
                    .iter()
                    .map(|(key, child)| (key.as_slice(), Self::new(child)))
                    .collect(),
            ),
            Node::List(v, child) => Self::List(v.unified(), Box::new(Self::new(child))),
            Node::Array(v, size, child) => {
                Self::Array(v.sel(), v.validity(), *size as usize, Box::new(Self::new(child)))
            }
            Node::Map(v, keys, values) => Self::Map(
                v.unified(),
                Box::new(Self::new(keys)),
                Box::new(Self::new(values)),
            ),
            Node::Union(v, members) => {
                let tags = v
                    .struct_children()
                    .and_then(|children| children.first())
                    .map_or_else(|| v.unified(), Vector::unified);
                Self::Union(
                    v.sel(),
                    v.validity(),
                    tags,
                    members
                        .iter()
                        .map(|(key, member)| (key.as_slice(), Self::new(member)))
                        .collect(),
                )
            }
        }
    }

    /// Whether row `row` is NULL.
    fn is_null(&self, row: usize) -> bool {
        match self {
            Self::Null => true,
            Self::Bool(u) => !u.is_valid(row),
            Self::Signed(u) => !u.is_valid(row),
            Self::Unsigned(u) => !u.is_valid(row),
            Self::Double(u) => !u.is_valid(row),
            Self::Str(_, u) | Self::Raw(_, u) | Self::Json(_, u) => !u.is_valid(row),
            Self::Struct(sel, validity, _)
            | Self::Array(sel, validity, _, _)
            | Self::Union(sel, validity, _, _) => !validity.is_valid(sel.slot(row)),
            Self::List(u, _) | Self::Map(u, _, _) => !u.is_valid(row),
        }
    }

    /// Appends the JSON of row `row`.
    fn write(&self, row: usize, out: &mut Vec<u8>, arena: &mut JsonArena) -> Result<()> {
        if self.is_null(row) {
            out.extend_from_slice(b"null");
            return Ok(());
        }
        match self {
            Self::Null => out.extend_from_slice(b"null"),
            Self::Bool(u) => out.extend_from_slice(if u.values[u.slot(row)] { b"true" } else { b"false" }),
            Self::Signed(u) => crate::yyjson::write_i64(out, u.values[u.slot(row)]),
            Self::Unsigned(u) => crate::yyjson::write_u64(out, u.values[u.slot(row)]),
            Self::Double(u) => write_f64(out, u.values[u.slot(row)]),
            Self::Str(v, u) => write_string(out, v.string_value(&u.values[u.slot(row)])),
            Self::Raw(v, u) => write_raw(out, v.string_value(&u.values[u.slot(row)])),
            Self::Json(v, u) => {
                let text = v.string_value(&u.values[u.slot(row)]);
                arena.reset();
                let doc = crate::json_common::read_document(arena, text)?;
                write_val(out, &arena.doc(), doc.root);
            }
            Self::Struct(sel, _, fields) => {
                let slot = sel.slot(row);
                out.push(b'{');
                for (i, (key, child)) in fields.iter().enumerate() {
                    if i > 0 {
                        out.push(b',');
                    }
                    out.extend_from_slice(key);
                    child.write(slot, out, arena)?;
                }
                out.push(b'}');
            }
            Self::List(u, child) => {
                let entry = u.values[u.slot(row)];
                out.push(b'[');
                for (i, child_row) in entry.range().enumerate() {
                    if i > 0 {
                        out.push(b',');
                    }
                    child.write(child_row, out, arena)?;
                }
                out.push(b']');
            }
            Self::Array(sel, _, size, child) => {
                let slot = sel.slot(row);
                out.push(b'[');
                for i in 0..*size {
                    if i > 0 {
                        out.push(b',');
                    }
                    child.write(slot * size + i, out, arena)?;
                }
                out.push(b']');
            }
            Self::Map(u, keys, values) => {
                let entry = u.values[u.slot(row)];
                out.push(b'{');
                let mut first = true;
                for child_row in entry.range() {
                    if keys.is_null(child_row) {
                        continue;
                    }
                    if !first {
                        out.push(b',');
                    }
                    first = false;
                    keys.write(child_row, out, arena)?;
                    out.push(b':');
                    values.write(child_row, out, arena)?;
                }
                out.push(b'}');
            }
            Self::Union(sel, _, tags, members) => {
                let slot = sel.slot(row);
                out.push(b'{');
                if let Some(tag) = tags.get(slot)
                    && let Some((key, member)) = members.get(usize::from(tag))
                {
                    out.extend_from_slice(key);
                    member.write(slot, out, arena)?;
                }
                out.push(b'}');
            }
        }
        Ok(())
    }
}

/// The casts from the bind data, or the default casts.
fn bound_casts(state: &FunctionState) -> Arc<CastFunctionSet> {
    state
        .bind_data
        .as_deref()
        .and_then(|data| data.as_any().downcast_ref::<JsonCreateFunctionData>())
        .map_or_else(
            || Arc::new(CastFunctionSet::new()),
            |data| Arc::clone(&data.casts),
        )
}

/// `ToJSONFunctionInternal`: the JSON of each row of `input`; NULL rows stay NULL.
fn to_json_internal(
    casts: &CastFunctionSet,
    input: &Vector,
    count: usize,
    result: &mut Vector,
    arena: &mut JsonArena,
) -> Result<()> {
    let constant = input.is_constant() || count == 1;
    let rows = if input.is_constant() { 1 } else { count };
    let node = convert(casts, input, rows)?;
    let prepared = Prepared::new(&node);
    let mut builder = StrBuilder::default();
    let mut buf = Vec::new();
    for row in 0..rows {
        if prepared.is_null(row) {
            builder.push_null();
            continue;
        }
        buf.clear();
        prepared.write(row, &mut buf, arena)?;
        builder.push(&buf);
    }
    let ty = result.logical_type().clone();
    *result = builder.finish(&ty)?;
    if constant {
        result.make_constant();
    }
    Ok(())
}

fn to_json(args: &DataChunk, state: &mut FunctionState, result: &mut Vector) -> Result<()> {
    let casts = bound_casts(state);
    let arena = local_arena(state);
    to_json_internal(&casts, args.column(0), args.len(), result, arena)
}

/// `ObjectFunction` / `ArrayFunction`.
fn create_rows(args: &DataChunk, state: &mut FunctionState, result: &mut Vector, object: bool) -> Result<()> {
    let casts = bound_casts(state);
    let constant = all_constant(args);
    let rows = if constant { 1 } else { args.len() };
    let nodes = args
        .columns()
        .iter()
        .map(|column| convert(&casts, column, rows))
        .collect::<Result<Vec<_>>>()?;
    let prepared: Vec<Prepared<'_>> = nodes.iter().map(Prepared::new).collect();
    let arena = local_arena(state);
    let mut builder = StrBuilder::default();
    let mut buf = Vec::new();
    for row in 0..rows {
        buf.clear();
        if object {
            buf.push(b'{');
            for (pair, chunk) in prepared.chunks(2).enumerate() {
                let [key, value] = chunk else {
                    continue;
                };
                if key.is_null(row) {
                    return Err(Error::invalid_input("JSON key cannot be NULL"));
                }
                if pair > 0 {
                    buf.push(b',');
                }
                key.write(row, &mut buf, arena)?;
                buf.push(b':');
                value.write(row, &mut buf, arena)?;
            }
            buf.push(b'}');
        } else {
            buf.push(b'[');
            for (i, value) in prepared.iter().enumerate() {
                if i > 0 {
                    buf.push(b',');
                }
                value.write(row, &mut buf, arena)?;
            }
            buf.push(b']');
        }
        builder.push(&buf);
    }
    let ty = result.logical_type().clone();
    *result = builder.finish(&ty)?;
    if constant {
        result.make_constant();
    }
    Ok(())
}

fn object_rows(args: &DataChunk, state: &mut FunctionState, result: &mut Vector) -> Result<()> {
    create_rows(args, state, result, true)
}

fn array_rows(args: &DataChunk, state: &mut FunctionState, result: &mut Vector) -> Result<()> {
    create_rows(args, state, result, false)
}

/// A variadic create function with the given bind.
fn create_function(
    name: &str,
    kernel: fn(&DataChunk, &mut FunctionState, &mut Vector) -> Result<()>,
    bind: impl Fn(&mut ScalarFunction, &BindInput<'_>) -> Result<Option<Arc<dyn FunctionData>>>
    + Send
    + Sync
    + 'static,
    special_nulls: bool,
) -> ScalarFunctionSet {
    let mut function = ScalarFunction::new(name, Vec::new(), json_type(), kernel).with_bind(bind);
    function.varargs = Some(LogicalType::ANY);
    if special_nulls {
        function.null_handling = NullHandling::Special;
    }
    let mut set = ScalarFunctionSet::new(name);
    set.add(function);
    set
}

/// `JSONFunctions::GetObjectFunction`.
pub(crate) fn object_function() -> ScalarFunctionSet {
    create_function(
        "json_object",
        object_rows,
        |function, input| {
            if input.argument_types.len() % 2 != 0 {
                return Err(Error::binder(
                    "json_object() requires an even number of arguments",
                ));
            }
            create_bind(function, input, true)
        },
        true,
    )
}

/// `JSONFunctions::GetArrayFunction`.
pub(crate) fn array_function() -> ScalarFunctionSet {
    create_function(
        "json_array",
        array_rows,
        |function, input| create_bind(function, input, false),
        true,
    )
}

/// `JSONFunctions::GetToJSONFunction`.
pub(crate) fn to_json_function() -> ScalarFunctionSet {
    create_function(
        "to_json",
        to_json,
        |function, input| {
            if input.argument_types.len() != 1 {
                return Err(Error::binder("to_json() takes exactly one argument"));
            }
            create_bind(function, input, false)
        },
        false,
    )
}

/// `JSONFunctions::GetArrayToJSONFunction`.
pub(crate) fn array_to_json_function() -> ScalarFunctionSet {
    create_function(
        "array_to_json",
        to_json,
        |function, input| {
            if input.argument_types.len() != 1 {
                return Err(Error::binder("array_to_json() takes exactly one argument"));
            }
            let id = input.argument_types[0].id();
            if id == LogicalTypeId::Unknown {
                return Err(rustdb_planner::expression_binder::parameter_not_resolved());
            }
            if !matches!(
                id,
                LogicalTypeId::List | LogicalTypeId::Array | LogicalTypeId::SqlNull
            ) {
                return Err(Error::binder(
                    "array_to_json() argument type must be LIST or ARRAY",
                ));
            }
            create_bind(function, input, false)
        },
        false,
    )
}

/// `JSONFunctions::GetRowToJSONFunction`.
pub(crate) fn row_to_json_function() -> ScalarFunctionSet {
    create_function(
        "row_to_json",
        to_json,
        |function, input| {
            if input.argument_types.len() != 1 {
                return Err(Error::binder("row_to_json() takes exactly one argument"));
            }
            let id = input.argument_types[0].id();
            if id == LogicalTypeId::Unknown {
                return Err(rustdb_planner::expression_binder::parameter_not_resolved());
            }
            if !matches!(id, LogicalTypeId::Struct | LogicalTypeId::SqlNull) {
                return Err(Error::binder("row_to_json() argument type must be STRUCT"));
            }
            create_bind(function, input, false)
        },
        false,
    )
}

/// `AnyToJSONCast`: the cast of any value to JSON (`to_json` of the value).
pub(crate) fn any_to_json_cast(casts: &CastFunctionSet) -> BoundCastInfo {
    let casts = casts.clone();
    BoundCastInfo::new(move |source, result, count, _params| {
        let mut arena = JsonArena::default();
        to_json_internal(&casts, source, count, result, &mut arena)?;
        Ok(true)
    })
}
