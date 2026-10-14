//! The type system and columnar in-memory format of rustdb.
//!
//! Layouts follow DuckDB exactly (16-byte strings, validity bitmasks, flat/constant
//! vectors) because the same bytes are written to storage and exposed via the C API.

mod buffer;
mod chunk;
pub mod compare;
pub mod kernels;
mod logical_type;
mod native;
mod physical_type;
mod selection;
mod string_t;
mod validity;
mod value;
mod vector;

pub use chunk::DataChunk;
pub use logical_type::{LogicalType, LogicalTypeId, TypeInfo};
pub use native::NativeType;
pub use physical_type::PhysicalType;
pub use rustdb_common::STANDARD_VECTOR_SIZE;
pub use selection::{Sel, SelectionVector};
pub use string_t::StringT;
pub use validity::ValidityMask;
pub use value::Value;
pub use vector::{ListEntry, StringHeap, Unified, Vector, VectorKind};
