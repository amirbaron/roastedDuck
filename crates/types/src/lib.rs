//! The type system and columnar in-memory format of rustdb.
//!
//! Layouts follow DuckDB exactly (16-byte strings, validity bitmasks, flat/constant
//! vectors) because the same bytes are written to storage and exposed via the C API.

pub mod bit;
pub mod blob;
mod buffer;
pub mod cast_rules;
mod chunk;
pub mod date;
pub mod format;
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
pub mod uuid;
mod validity;
mod value;
mod vector;

pub use cast_rules::implicit_cast_cost;
pub use chunk::DataChunk;
pub use date::{Date, DateParseError, ParsedDate};
pub use interval::{Interval, IntervalParseError};
pub use logical_type::{LogicalType, LogicalTypeId, TypeInfo};
pub use native::NativeType;
pub use physical_type::PhysicalType;
pub use rustdb_common::STANDARD_VECTOR_SIZE;
pub use selection::{Sel, SelectionVector};
pub use string_t::StringT;
pub use time::{ParsedTime, Time, TimeNs, TimeTz};
pub use timestamp::{ParsedTimestamp, Timestamp, TimestampParseError};
pub use uuid::Uuid;
pub use validity::ValidityMask;
pub use value::Value;
pub use vector::{ListEntry, StringHeap, Unified, Vector, VectorKind};
