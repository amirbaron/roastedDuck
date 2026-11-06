//! `test_vector_types(values..., all_flat := false)` (DuckDB's
//! `function/table/system/test_vector_types.cpp`): rows of the argument types in every
//! vector layout — flat, constant, dictionary and sequence — to exercise the physical
//! paths of operators and functions.

use std::any::Any;
use std::sync::Arc;

use rustdb_common::{Error, Result};
use rustdb_function::cast::ValueCast;
use rustdb_function::{
    FunctionData, TableFunction, TableFunctionBindInput, TableFunctionBindOutput, TableFunctionSet,
    TableScanState,
};
use rustdb_types::{DataChunk, LogicalType, LogicalTypeId, PhysicalType, SelectionVector, Value, Vector};

use super::test_all_types::test_type_limits;

/// The rows of the flat layout (`TEST_VECTOR_CARDINALITY`).
const CARDINALITY: usize = 3;

/// The layout of one generated chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Layout {
    Flat,
    Constant,
    Dictionary,
    Sequence,
}

/// One generated chunk: its layout and the values of its rows, column by column.
#[derive(Debug)]
struct Chunk {
    layout: Layout,
    columns: Vec<Vec<Value>>,
}

/// `TestVectorBindData`, with the chunks generated up front (DuckDB's `TestVectorTypesInit`).
#[derive(Debug)]
struct TestVectorData {
    types: Vec<LogicalType>,
    chunks: Vec<Chunk>,
    all_flat: bool,
}

impl FunctionData for TestVectorData {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// The minimum and maximum of each `test_all_types` type id, the first column of an id
/// winning (DuckDB's `test_type_map`).
struct TypeMap(Vec<(LogicalTypeId, Value, Value)>);

impl TypeMap {
    /// The minimum, maximum and NULL of `ty`, cast to it.
    fn extremes(&self, ty: &LogicalType) -> Result<[Value; 3]> {
        let (_, min, max) = self.0.iter().find(|(id, _, _)| *id == ty.id()).ok_or_else(|| {
            Error::not_implemented(format!("Unimplemented type for test_vector_types {ty}"))
        })?;
        let cast = |value: &Value| {
            if value.logical_type() == ty {
                Ok(value.clone())
            } else {
                value.default_cast_as(ty)
            }
        };
        Ok([cast(min)?, cast(max)?, Value::null(ty.clone())])
    }
}

/// `TestVectorFlat::GenerateValues`: three values of `ty`.
fn flat_values(map: &TypeMap, ty: &LogicalType) -> Result<Vec<Value>> {
    match ty.physical_type() {
        PhysicalType::Struct => {
            let children = ty.struct_children().unwrap_or_default();
            let mut rows: Vec<Vec<Value>> = vec![Vec::new(); CARDINALITY];
            for (_, child_type) in children {
                for (row, value) in flat_values(map, child_type)?.into_iter().enumerate() {
                    rows[row].push(value);
                }
            }
            Ok(rows
                .into_iter()
                .map(|fields| Value::from_children(ty.clone(), fields))
                .collect())
        }
        PhysicalType::List if ty.id() == LogicalTypeId::Map => {
            let entry_type = ty
                .child_type()
                .ok_or_else(|| Error::internal("MAP without an entry type"))?;
            let (Some([(_, key_type), (_, value_type)]), entries) =
                (entry_type.struct_children(), flat_values(map, entry_type)?)
            else {
                return Err(Error::internal("MAP entries are not key/value structs"));
            };
            let entry = |value: &Value| {
                let fields = value.children().unwrap_or_default();
                Value::map(
                    key_type.clone(),
                    value_type.clone(),
                    vec![(fields[0].clone(), fields[1].clone())],
                )
            };
            Ok(vec![
                entry(&entries[0]),
                Value::null(ty.clone()),
                entry(&entries[1]),
            ])
        }
        PhysicalType::List => {
            let child_type = ty
                .child_type()
                .ok_or_else(|| Error::internal("LIST without a child type"))?;
            let children = flat_values(map, child_type)?;
            Ok(vec![
                Value::list(child_type.clone(), vec![children[0].clone(), children[1].clone()]),
                Value::list(child_type.clone(), Vec::new()),
                Value::list(child_type.clone(), vec![children[2].clone()]),
            ])
        }
        _ => Ok(map.extremes(ty)?.to_vec()),
    }
}

/// `TestVectorSequence::GenerateVector`, as values: integers count 3, 5, 7.
fn sequence_values(map: &TypeMap, ty: &LogicalType) -> Result<Vec<Value>> {
    use LogicalTypeId as L;
    if matches!(
        ty.id(),
        L::TinyInt
            | L::SmallInt
            | L::Integer
            | L::BigInt
            | L::UTinyInt
            | L::USmallInt
            | L::UInteger
            | L::UBigInt
    ) {
        return [3i64, 5, 7]
            .into_iter()
            .map(|value| Value::bigint(value).default_cast_as(ty))
            .collect();
    }
    match ty.physical_type() {
        PhysicalType::Struct => {
            let children = ty.struct_children().unwrap_or_default();
            let mut rows: Vec<Vec<Value>> = vec![Vec::new(); CARDINALITY];
            for (_, child_type) in children {
                for (row, value) in sequence_values(map, child_type)?.into_iter().enumerate() {
                    rows[row].push(value);
                }
            }
            Ok(rows
                .into_iter()
                .map(|fields| Value::from_children(ty.clone(), fields))
                .collect())
        }
        PhysicalType::List => {
            let child_type = ty
                .child_type()
                .ok_or_else(|| Error::internal("LIST without a child type"))?;
            let children = sequence_values(map, child_type)?;
            Ok(vec![
                Value::list(child_type.clone(), vec![children[0].clone(), children[1].clone()]),
                Value::list(child_type.clone(), Vec::new()),
                Value::list(child_type.clone(), vec![children[2].clone()]),
            ])
        }
        _ => Ok(map.extremes(ty)?.to_vec()),
    }
}

/// DuckDB's `TestVectorTypesInit`: flat, constant, dictionary and sequence chunks.
fn generate(types: &[LogicalType]) -> Result<Vec<Chunk>> {
    let map = TypeMap(test_type_limits()?);
    let flat: Vec<Vec<Value>> = types
        .iter()
        .map(|ty| flat_values(&map, ty))
        .collect::<Result<_>>()?;
    let mut chunks = vec![
        Chunk {
            layout: Layout::Flat,
            columns: flat.clone(),
        },
        Chunk {
            layout: Layout::Constant,
            columns: flat
                .iter()
                .map(|values| vec![values[0].clone(); CARDINALITY])
                .collect(),
        },
        // `TestVectorDictionary`: rows 1 and 2 of the flat values.
        Chunk {
            layout: Layout::Dictionary,
            columns: flat,
        },
    ];
    // The sequence layout does not support MAP.
    if types.iter().all(|ty| ty.id() != LogicalTypeId::Map) {
        chunks.push(Chunk {
            layout: Layout::Sequence,
            columns: types
                .iter()
                .map(|ty| sequence_values(&map, ty))
                .collect::<Result<_>>()?,
        });
    }
    Ok(chunks)
}

/// `TestVectorTypesBind`.
fn bind(input: &TableFunctionBindInput<'_>) -> Result<TableFunctionBindOutput> {
    let types: Vec<LogicalType> = input
        .inputs
        .iter()
        .map(|value| value.logical_type().clone())
        .collect();
    let names = (0..types.len())
        .map(|index| {
            if index == 0 {
                rustdb_common::ByteString::from("test_vector")
            } else {
                rustdb_common::byte_format!("test_vector{}", index + 1)
            }
        })
        .collect();
    let mut all_flat = false;
    for (name, value) in input.named_parameters {
        if name != "all_flat" {
            return Err(Error::internal(
                "Unrecognized named parameter for test_vector_types",
            ));
        }
        if value.is_null() {
            return Err(Error::invalid_input("Cannot use NULL as argument for all_flat"));
        }
        all_flat = value.as_bool().unwrap_or(false);
    }
    let chunks = generate(&types)?;
    Ok(TableFunctionBindOutput {
        return_types: types.clone(),
        names,
        bind_data: Some(Arc::new(TestVectorData {
            types,
            chunks,
            all_flat,
        })),
    })
}

/// The vector of one column of `chunk`, in the chunk's layout.
fn column_vector(chunk: &Chunk, ty: &LogicalType, values: &[Value], all_flat: bool) -> Result<Vector> {
    let mut vector = Vector::flat(ty.clone(), CARDINALITY);
    for (row, value) in values.iter().enumerate() {
        vector.set_value(row, value)?;
    }
    if all_flat {
        if chunk.layout == Layout::Dictionary {
            let mut sliced = Vector::flat(ty.clone(), CARDINALITY);
            for (row, value) in values[1..].iter().enumerate() {
                sliced.set_value(row, value)?;
            }
            return Ok(sliced);
        }
        return Ok(vector);
    }
    Ok(match chunk.layout {
        Layout::Flat | Layout::Sequence => vector,
        Layout::Constant => Vector::constant(&values[0])?,
        Layout::Dictionary => vector.slice(&SelectionVector::new(vec![1u32, 2])),
    })
}

/// `TestVectorTypesFunction`: one generated chunk per call.
fn scan(
    bind_data: Option<&dyn FunctionData>,
    _: &[u64],
    state: &mut TableScanState,
    chunk: &mut DataChunk,
) -> Result<bool> {
    let data = bind_data
        .and_then(|data| data.as_any().downcast_ref::<TestVectorData>())
        .ok_or_else(|| Error::internal("test_vector_types scan without bind data"))?;
    let Some(current) = usize::try_from(state.position)
        .ok()
        .and_then(|index| data.chunks.get(index))
    else {
        let columns = data.types.iter().map(|ty| Vector::flat(ty.clone(), 1)).collect();
        chunk.set_columns(columns, 0);
        return Ok(false);
    };
    let columns = data
        .types
        .iter()
        .zip(&current.columns)
        .map(|(ty, values)| column_vector(current, ty, values, data.all_flat))
        .collect::<Result<Vec<_>>>()?;
    let rows = if current.layout == Layout::Dictionary {
        2
    } else {
        CARDINALITY
    };
    chunk.set_columns(columns, rows);
    state.position += 1;
    Ok(state.position < data.chunks.len() as u64)
}

/// `test_vector_types`.
pub fn test_vector_types_functions() -> TableFunctionSet {
    let mut function = TableFunction::new("test_vector_types", vec![LogicalType::ANY])
        .with_bind(bind)
        .with_scan(scan);
    function.varargs = Some(LogicalType::ANY);
    function
        .named_parameters
        .push(("all_flat".to_owned(), LogicalType::BOOLEAN));
    let mut set = TableFunctionSet::new("test_vector_types");
    set.add(function);
    set
}
