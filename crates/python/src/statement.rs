//! Owned native parsed statements and the frozen statement/result enums.
use crate::{conversion, error};
use pyo3::IntoPyObjectExt;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PySet, PyString, PyTuple};
use rustdb_ast::{SqlStatement, StatementType as S};
use rustdb_common::{Error, Result};

const STATEMENTS: &[(&str, u8)] = &[
    ("INVALID", 0),
    ("SELECT", 1),
    ("INSERT", 2),
    ("UPDATE", 3),
    ("CREATE", 4),
    ("DELETE", 5),
    ("PREPARE", 6),
    ("EXECUTE", 7),
    ("ALTER", 8),
    ("TRANSACTION", 9),
    ("COPY", 10),
    ("ANALYZE", 11),
    ("VARIABLE_SET", 12),
    ("CREATE_FUNC", 13),
    ("EXPLAIN", 14),
    ("DROP", 15),
    ("EXPORT", 16),
    ("PRAGMA", 17),
    ("VACUUM", 18),
    ("CALL", 19),
    ("SET", 20),
    ("LOAD", 21),
    ("RELATION", 22),
    ("EXTENSION", 23),
    ("LOGICAL_PLAN", 24),
    ("ATTACH", 25),
    ("DETACH", 26),
    ("MULTI", 27),
    ("COPY_DATABASE", 28),
    ("MERGE_INTO", 30),
];
const RETURNS: &[(&str, u8)] = &[("QUERY_RESULT", 0), ("CHANGED_ROWS", 1), ("NOTHING", 2)];
const EXPLAINS: &[(&str, u8)] = &[("STANDARD", 0), ("ANALYZE", 1)];
const TOKENS: &[(&str, u8)] = &[
    ("identifier", 0),
    ("numeric_const", 1),
    ("string_const", 2),
    ("operator", 3),
    ("keyword", 4),
    ("comment", 5),
];
pub(super) fn explain_type(object: &Bound<'_, PyAny>) -> PyResult<Option<u8>> {
    if let Ok(value) = object.extract::<PyRef<'_, PyExplainType>>() {
        return Ok(Some(value.value));
    }
    let invalid =
        || error::InvalidInputException::new_err("Invalid Input Error: Unrecognized type for 'explain'");
    if let Ok(name) = object.cast::<PyString>() {
        return match name.to_str()?.to_ascii_lowercase().as_str() {
            "" | "standard" => Ok(Some(0)),
            "analyze" => Ok(Some(1)),
            _ => Err(invalid()),
        };
    }
    if object.is_instance_of::<pyo3::types::PyInt>() {
        return match object.extract::<i64>()? {
            0 => Ok(Some(0)),
            1 => Ok(Some(1)),
            _ => Err(invalid()),
        };
    }
    Ok(None)
}
fn enum_value(
    py: Python<'_>,
    name: &str,
    args: &Bound<'_, PyTuple>,
    kwargs: Option<&Bound<'_, PyDict>>,
) -> PyResult<u8> {
    let bad = || -> PyResult<PyErr> {
        let signature = format!("_duckdb.{name}(value: typing.SupportsInt | typing.SupportsIndex)");
        let error = error::arguments(py, "__init__", &signature, None, args, kwargs)?;
        let message = error.value(py).str()?.extract::<String>()?;
        Ok(pyo3::exceptions::PyTypeError::new_err(message.replacen(
            "function arguments",
            "constructor arguments",
            1,
        )))
    };
    if args.len() > 1
        || kwargs.is_some_and(|kwargs| {
            kwargs.len() > 1
                || kwargs
                    .iter()
                    .any(|(k, _)| !matches!(k.extract::<String>(), Ok(k) if k=="value"))
        })
    {
        return Err(bad()?);
    }
    let keyword = kwargs
        .map(|kwargs| kwargs.get_item("value"))
        .transpose()?
        .flatten();
    if args.len() == 1 && keyword.is_some() {
        return Err(bad()?);
    }
    let value = if args.len() == 1 {
        Some(args.get_item(0)?)
    } else {
        keyword
    };
    match value.and_then(|value| value.extract::<u8>().ok()) {
        Some(value) => Ok(value),
        None => Err(bad()?),
    }
}
macro_rules! native_enum {
    ($rust:ident,$name:literal,$members:ident) => {
        #[pyclass(name=$name,module="_duckdb")]
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub(super) struct $rust {
            pub(super) value: u8,
            initialized: bool,
        }
        #[pymethods]
        impl $rust {
            fn __richcmp__(
                &self,
                other: &Bound<'_, PyAny>,
                operation: pyo3::basic::CompareOp,
            ) -> PyResult<Py<PyAny>> {
                use pyo3::basic::CompareOp;
                if !matches!(operation, CompareOp::Eq | CompareOp::Ne) {
                    return Ok(other.py().NotImplemented());
                }
                let value = if let Ok(other) = other.extract::<PyRef<'_, Self>>() {
                    Some(other.value)
                } else if $name == "ExplainType" {
                    explain_type(other)?
                } else if $name == "token_type" && other.is_instance_of::<pyo3::types::PyInt>() {
                    other.extract::<u8>().ok()
                } else {
                    None
                };
                match value {
                    Some(value) => (if matches!(operation, CompareOp::Eq) {
                        self.value == value
                    } else {
                        self.value != value
                    })
                    .into_py_any(other.py()),
                    None => Ok(other.py().NotImplemented()),
                }
            }
            #[new]
            #[pyo3(signature=(*args,**kwargs))]
            fn new(
                py: Python<'_>,
                args: &Bound<'_, PyTuple>,
                kwargs: Option<&Bound<'_, PyDict>>,
            ) -> PyResult<Self> {
                if args.is_empty() && kwargs.is_none_or(|kwargs| kwargs.is_empty()) {
                    return Ok(Self {
                        value: 0,
                        initialized: false,
                    });
                }
                enum_value(py, $name, args, kwargs).map(|value| Self {
                    value,
                    initialized: true,
                })
            }
            #[getter]
            fn name(&self) -> &'static str {
                $members
                    .iter()
                    .find(|(_, v)| *v == self.value)
                    .map_or("???", |(n, _)| *n)
            }
            #[getter]
            fn value(&self) -> u8 {
                self.value
            }
            fn __int__(&self) -> u8 {
                self.value
            }
            fn __index__(&self) -> u8 {
                self.value
            }
            fn __hash__(&self) -> isize {
                isize::from(self.value)
            }
            fn __getstate__(&self) -> u8 {
                self.value
            }
            fn __reduce__(&self, py: Python<'_>) -> (Py<PyAny>, (u8,)) {
                (py.get_type::<Self>().unbind().into_any(), (self.value,))
            }
            fn __setstate__(&mut self, state: &Bound<'_, PyAny>) -> PyResult<()> {
                if !self.initialized {
                    self.value = state.extract::<u8>()?;
                    self.initialized = true;
                }
                Ok(())
            }
            fn __reduce_ex__(&self, py: Python<'_>, _protocol: i32) -> PyResult<Py<PyAny>> {
                let constructor = py.import("copyreg")?.getattr("__newobj__")?;
                let class = PyTuple::new(py, [py.get_type::<Self>()])?.into_any();
                let state = self.value.into_pyobject(py)?.into_any();
                Ok(PyTuple::new(
                    py,
                    [
                        constructor,
                        class,
                        state,
                        py.None().into_bound(py),
                        py.None().into_bound(py),
                    ],
                )?
                .unbind()
                .into_any())
            }
            fn __str__(&self) -> String {
                format!("{}.{}", $name, self.name())
            }
            fn __repr__(&self) -> String {
                format!("<{}.{}: {}>", $name, self.name(), self.value)
            }
        }
    };
}
native_enum!(PyStatementType, "StatementType", STATEMENTS);
native_enum!(PyExpectedResultType, "ExpectedResultType", RETURNS);
native_enum!(PyExplainType, "ExplainType", EXPLAINS);
native_enum!(PyTokenType, "token_type", TOKENS);

#[pyfunction(signature=(*args, **kwargs))]
fn tokenize(args: &Bound<'_, PyTuple>, kwargs: Option<&Bound<'_, PyDict>>) -> PyResult<Py<PyAny>> {
    let py = args.py();
    let bad = || error::arguments(py, "tokenize", "(query: str) -> list", None, args, kwargs);
    if args.len() > 1
        || kwargs.is_some_and(|kwargs| {
            kwargs
                .iter()
                .any(|(key, _)| key.extract::<String>().is_ok_and(|key| key != "query"))
        })
    {
        return Err(bad()?);
    }
    let keyword = kwargs
        .map(|kwargs| kwargs.get_item("query"))
        .transpose()?
        .flatten();
    if !args.is_empty() && keyword.is_some() {
        return Err(bad()?);
    }
    let value = if args.is_empty() {
        keyword
    } else {
        Some(args.get_item(0)?)
    };
    let Some(value) = value else {
        return Err(bad()?);
    };
    let Some(bytes) = crate::type_creation::string_bytes(&value)? else {
        return Err(bad()?);
    };
    let tokens = py.detach(|| rustdb_parser::tokenize_bytes(&bytes));
    let mut result = Vec::with_capacity(tokens.len());
    for token in tokens {
        use rustdb_parser::SimplifiedTokenType as T;
        let value = match token.kind {
            T::Identifier => 0,
            T::NumericConstant => 1,
            T::StringConstant => 2,
            T::Operator => 3,
            T::Keyword => 4,
            T::Comment => 5,
        };
        result.push((
            token.start,
            PyTokenType {
                value,
                initialized: true,
            },
        ));
    }
    Ok(PyList::new(py, result)?.unbind().into_any())
}

#[pyclass(name = "Statement", module = "_duckdb", frozen)]
#[derive(Debug)]
pub(super) struct PyStatement {
    pub(super) statement: SqlStatement,
}
#[pymethods]
impl PyStatement {
    #[new]
    #[pyo3(signature=(*_args,**_kwargs))]
    fn new(_args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>) -> PyResult<Self> {
        Err(pyo3::exceptions::PyTypeError::new_err(
            "_duckdb.Statement: No constructor defined!",
        ))
    }
    #[getter]
    fn query(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        conversion::text(py, &self.statement.query)
    }
    #[getter]
    fn r#type(&self) -> PyStatementType {
        PyStatementType {
            value: self.statement.statement_type() as u8,
            initialized: true,
        }
    }
    #[getter]
    fn named_parameters(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let set = PySet::empty(py)?;
        for (name, _) in self.statement.named_param_map.iter() {
            set.add(conversion::text(py, name)?)?;
        }
        Ok(set.unbind().into_any())
    }
    #[getter]
    fn expected_result_type(&self, py: Python<'_>) -> PyResult<Vec<PyExpectedResultType>> {
        let values: &[u8] = match self.statement.statement_type() {
            S::PrepareStatement
            | S::VacuumStatement
            | S::AlterStatement
            | S::TransactionStatement
            | S::SetStatement
            | S::LoadStatement
            | S::ExportStatement
            | S::DropStatement
            | S::DetachStatement
            | S::CreateFuncStatement
            | S::CopyDatabaseStatement
            | S::AttachStatement => &[2],
            S::PragmaStatement
            | S::ExplainStatement
            | S::LogicalPlanStatement
            | S::CallStatement
            | S::SelectStatement
            | S::ExecuteStatement => &[0],
            S::CopyStatement | S::DeleteStatement | S::UpdateStatement | S::InsertStatement => &[1, 0],
            S::AnalyzeStatement
            | S::VariableSetStatement
            | S::RelationStatement
            | S::ExtensionStatement
            | S::CreateStatement
            | S::MultiStatement
            | S::MergeIntoStatement => &[1, 0, 2],
            other => {
                return Err(error::native(
                    py,
                    &Error::internal(format!(
                        "Unrecognized StatementType in ExpectedResultType: {}",
                        other.name().trim_end_matches("_STATEMENT")
                    )),
                ));
            }
        };
        Ok(values
            .iter()
            .map(|&value| PyExpectedResultType {
                value,
                initialized: true,
            })
            .collect())
    }
}
#[derive(Debug)]
pub(super) enum Query<'a> {
    Text(&'a [u8]),
    Parsed(Box<SqlStatement>),
}
impl Query<'_> {
    pub(super) fn statements(self, connection: &mut rustdb::Connection) -> Result<Vec<SqlStatement>> {
        match self {
            Self::Text(text) => connection.parse_statements_bytes(text),
            Self::Parsed(statement) => {
                let query = statement.query.clone();
                connection.preprocess_statements(query, vec![*statement])
            }
        }
    }
}
pub(super) fn input<'a>(query: &'a Bound<'_, PyAny>) -> PyResult<Query<'a>> {
    if query.is_none() {
        return Err(error::native(
            query.py(),
            &Error::internal("Attempted to dereference shared_ptr that is NULL!"),
        ));
    }
    if let Ok(statement) = query.extract::<PyRef<'_, PyStatement>>() {
        return Ok(Query::Parsed(Box::new(statement.statement.clone())));
    }
    if query.is_instance_of::<PyString>() {
        return Ok(Query::Text(query.extract::<&str>()?.as_bytes()));
    }
    Err(error::native(
        query.py(),
        &Error::invalid_input("Please provide either a DuckDBPyStatement or a string representing the query"),
    ))
}
#[pyfunction]
fn _enum_initialize(
    py: Python<'_>,
    name: &str,
    args: &Bound<'_, PyTuple>,
    kwargs: &Bound<'_, PyDict>,
) -> PyResult<()> {
    enum_value(py, name, args, Some(kwargs)).map(|_| ())
}
pub(super) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = module.py();
    module.add_class::<PyStatement>()?;
    module.add_class::<PyStatementType>()?;
    module.add_class::<PyExpectedResultType>()?;
    module.add_class::<PyExplainType>()?;
    module.add_class::<PyTokenType>()?;
    module.add_function(wrap_pyfunction!(tokenize, module)?)?;
    module.add_function(wrap_pyfunction!(_enum_initialize, module)?)?;
    // pybind11 separates allocation from initialization. Keep that protocol so
    // NEWOBJ pickles can allocate first and restore their integer state afterward.
    let globals = PyDict::new(py);
    globals.set_item("_enum_init", module.getattr("_enum_initialize")?)?;
    globals.set_item("__name__", "_duckdb")?;
    let initialize = py.eval(
        c"lambda self, *args, **kwargs: _enum_init(type(self).__name__, args, kwargs)",
        Some(&globals),
        None,
    )?;
    for (name, members) in [
        ("StatementType", STATEMENTS),
        ("ExpectedResultType", RETURNS),
        ("ExplainType", EXPLAINS),
        ("token_type", TOKENS),
    ] {
        let class = module.getattr(name)?;
        let dictionary = PyDict::new(py);
        for &(name, value) in members {
            let object = class.call1((value,))?;
            class.setattr(name, &object)?;
            dictionary.set_item(name, object)?;
        }
        class.setattr("__members__", dictionary)?;
        class.setattr("__init__", &initialize)?;
    }
    Ok(())
}
pub(super) fn list(py: Python<'_>, statements: Vec<SqlStatement>) -> PyResult<Py<PyAny>> {
    let list = PyList::empty(py);
    for statement in statements {
        list.append(Py::new(py, PyStatement { statement })?)?;
    }
    Ok(list.unbind().into_any())
}
