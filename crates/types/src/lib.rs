//! The type system and columnar in-memory format of rustdb.
//!
//! Layouts follow DuckDB exactly (16-byte strings, validity bitmasks, flat/constant
//! vectors) because the same bytes are written to storage and exposed via the C API.
//! Types and values serialize exactly like DuckDB's (`types.json`, `Value::Serialize`);
//! the type-info classes and type enums are generated from DuckDB's specs by
//! `tools/codegen` into `src/generated/`.

mod buffer;
mod chunk;
pub mod compare;
mod generated;
pub mod kernels;
mod logical_type;
mod native;
mod physical_type;
mod selection;
mod string_t;
mod type_info;
mod unbound;
mod validity;
mod value;
mod vector;

pub use chunk::DataChunk;
pub use generated::enums::{ExtraTypeInfoType, GeometryStorageType, LogicalTypeId};
pub use generated::interval::Interval;
pub use generated::type_info::{
    AggregateStateTypeInfo, AnyTypeInfo, ArrayTypeInfo, DecimalTypeInfo, ExtensionTypeInfo, ExtraTypeInfo,
    GeoTypeInfo, IntegerLiteralTypeInfo, ListTypeInfo, LogicalTypeModifier, StringTypeInfo, StructTypeInfo,
    TemplateTypeInfo, TypeInfo,
};
pub use logical_type::LogicalType;
pub use native::NativeType;
pub use physical_type::PhysicalType;
pub use rustdb_common::STANDARD_VECTOR_SIZE;
pub use selection::{Sel, SelectionVector};
pub use string_t::StringT;
pub use type_info::{CoordinateReferenceSystem, EnumTypeInfo};
pub use unbound::{TypeParameter, TypeParameterValue, UnboundTypeInfo, expression_ids};
pub use validity::ValidityMask;
pub use value::Value;
pub use vector::{ListEntry, StringHeap, Unified, Vector, VectorKind};
