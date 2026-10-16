//! The type system and columnar in-memory format of rustdb.
//!
//! Layouts follow DuckDB exactly (16-byte strings, validity bitmasks, flat/constant
//! vectors) because the same bytes are written to storage and exposed via the C API.
//! Types and values serialize exactly like DuckDB's (`types.json`, `Value::Serialize`);
//! the type-info classes and type enums are generated from DuckDB's specs by
//! `tools/codegen` into `src/generated/`.
//!
//! The temporal, UUID, BLOB and BIT modules hold the scalar logic of those types (parsing,
//! text rendering, conversions) shared by `Value::to_varchar` and the vectorised casts.

pub mod bignum;
pub mod bit;
pub mod blob;
mod buffer;
pub mod cast_rules;
mod chunk;
pub mod compare;
pub mod date;
mod fmt_float;
pub mod format;
mod generated;
pub mod geometry;
pub mod identifier;
pub mod interval;
pub mod kernels;
mod logical_type;
mod native;
mod physical_type;
mod selection;
mod string_t;
pub mod time;
pub mod timestamp;
mod type_info;
mod unbound;
pub mod uuid;
mod validity;
mod value;
mod vector;

pub use cast_rules::implicit_cast_cost;
pub use chunk::DataChunk;
pub use date::{Date, DateParseError, ParsedDate};
pub use generated::enums::{ExtraTypeInfoType, GeometryStorageType, LogicalTypeId};
pub use generated::interval::Interval;
pub use generated::type_info::{
    AggregateStateTypeInfo, AnyTypeInfo, ArrayTypeInfo, DecimalTypeInfo, ExtensionTypeInfo, ExtraTypeInfo,
    GeoTypeInfo, IntegerLiteralTypeInfo, ListTypeInfo, LogicalTypeModifier, StringTypeInfo, StructTypeInfo,
    TemplateTypeInfo, TypeInfo,
};
pub use interval::IntervalParseError;
pub use logical_type::LogicalType;
pub use native::NativeType;
pub use physical_type::PhysicalType;
pub use rustdb_common::STANDARD_VECTOR_SIZE;
pub use selection::{Sel, SelectionVector};
pub use string_t::StringT;
pub use time::{ParsedTime, Time, TimeNs, TimeTz};
pub use timestamp::{ParsedTimestamp, Timestamp, TimestampParseError};
pub use type_info::{CoordinateReferenceSystem, EnumTypeInfo};
pub use unbound::{
    TypeParameter, TypeParameterValue, UnboundTypeInfo, builtin_type_id, builtin_type_names, expression_ids,
};
pub use uuid::Uuid;
pub use validity::ValidityMask;
pub use value::Value;
pub use vector::{ListEntry, StringHeap, Unified, Vector, VectorKind};
