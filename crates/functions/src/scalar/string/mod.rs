//! String functions (DuckDB's `function/scalar/string` and `core_functions/scalar/string`).

pub mod caseconvert;
pub mod character;
pub mod concat;
pub mod contains;
pub mod hash;
pub mod length;
pub mod normalize;
pub mod replace;
pub mod reverse;
pub mod substring;
pub(crate) mod utf8;
