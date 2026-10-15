//! String functions (DuckDB's `function/scalar/string` and `core_functions/scalar/string`).

pub mod caseconvert;
pub mod character;
pub mod concat;
pub mod contains;
pub mod encode;
pub mod hash;
pub mod length;
pub mod like;
pub mod normalize;
pub mod regexp;
pub mod replace;
pub mod reverse;
pub mod similarity;
pub mod substring;
pub mod trim_pad;
pub(crate) mod utf8;
