//! PRAGMA functions (DuckDB's `PragmaFunction`): `PRAGMA name` and `PRAGMA name(args)`.
//!
//! The catalog holds their signatures, which binding a PRAGMA checks; the connection
//! runs them (the pragmas that stand for a query are expanded into it, the others change
//! the connection's state).

use std::sync::Arc;

use roastedduck_types::LogicalType;

/// How a pragma is called (DuckDB's `PragmaType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PragmaType {
    /// `PRAGMA name` (`PRAGMA_STATEMENT`).
    Statement,
    /// `PRAGMA name(args)` (`PRAGMA_CALL`).
    Call,
}

/// One overload of a pragma (DuckDB's `PragmaFunction`).
#[derive(Clone, Debug)]
pub struct PragmaFunction {
    pub name: Arc<str>,
    pub pragma_type: PragmaType,
    pub arguments: Vec<LogicalType>,
    /// Type of any additional trailing arguments, for variadic pragmas.
    pub varargs: Option<LogicalType>,
    /// The accepted `name = value` parameters and their types.
    pub named_parameters: Vec<(String, LogicalType)>,
    /// The pragma stands for a query (DuckDB's `query` callback), which the connection
    /// runs in its place, rather than running a function (`function`).
    pub query: bool,
}

impl PragmaFunction {
    /// `PragmaFunction::PragmaStatement`: called without arguments.
    pub fn statement(name: impl Into<Arc<str>>) -> Self {
        Self {
            name: name.into(),
            pragma_type: PragmaType::Statement,
            arguments: Vec::new(),
            varargs: None,
            named_parameters: Vec::new(),
            query: false,
        }
    }

    /// `PragmaFunction::PragmaCall`: called with `arguments`.
    pub fn call(name: impl Into<Arc<str>>, arguments: Vec<LogicalType>) -> Self {
        Self {
            name: name.into(),
            pragma_type: PragmaType::Call,
            arguments,
            varargs: None,
            named_parameters: Vec::new(),
            query: false,
        }
    }

    /// The pragma with a query callback (see [`PragmaFunction::query`]).
    #[must_use]
    pub fn with_query(mut self) -> Self {
        self.query = true;
        self
    }
}

/// All overloads of a pragma (DuckDB's `PragmaFunctionSet`).
#[derive(Clone, Debug)]
pub struct PragmaFunctionSet {
    pub name: Arc<str>,
    pub functions: Vec<PragmaFunction>,
}

impl PragmaFunctionSet {
    /// A set holding a single overload, named after it.
    pub fn single(function: PragmaFunction) -> Self {
        Self {
            name: Arc::clone(&function.name),
            functions: vec![function],
        }
    }
}
