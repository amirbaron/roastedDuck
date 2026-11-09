//! Python expressions own native parsed trees; composition never renders and reparses operands.
use crate::{conversion, datatype::PyType, error};
use pyo3::basic::CompareOp;
use pyo3::prelude::*;
use pyo3::types::{PyString, PyTuple};
use roastedduck_ast::{
    BetweenExpression, CaseCheck, CaseExpression, CollateExpression, ExpressionType as E, LambdaExpression,
    LambdaSyntaxType, OrderByNullType, OrderType, ParsedExpression, ParsedExpressionKind as K,
    QualifiedColumnName,
};
use roastedduck_common::Error;
use roastedduck_types::{LogicalType, Value};

#[pyclass(module = "_duckdb", name = "Expression", frozen)]
#[derive(Clone, Debug)]
pub struct PyExpression {
    pub(super) native: ParsedExpression,
    pub(super) order: OrderType,
    pub(super) null_order: OrderByNullType,
}
impl PyExpression {
    pub(super) fn new(native: ParsedExpression) -> Self {
        Self {
            native,
            order: OrderType::OrderDefault,
            null_order: OrderByNullType::OrderDefault,
        }
    }
    pub(super) fn from_object(object: &Bound<'_, PyAny>) -> PyResult<Self> {
        if let Ok(value) = object.extract::<PyRef<'_, Self>>() {
            return Ok(value.clone());
        }
        if object.is_instance_of::<PyString>() {
            return column_parts(object.py(), vec![object.extract()?]);
        }
        Ok(Self::new(ParsedExpression::constant(conversion::from_python(
            object,
        )?)))
    }
    fn binary(&self, other: &Bound<'_, PyAny>, name: &str, reverse: bool) -> PyResult<Self> {
        let other = Self::from_object(other)?;
        let children = if reverse {
            vec![other.native, self.native.clone()]
        } else {
            vec![self.native.clone(), other.native]
        };
        Ok(Self::new(operator_function(name, children)))
    }
    fn case(&self) -> PyResult<CaseExpression> {
        let K::Case(case) = &self.native.kind else {
            return Err(pyo3::exceptions::PyValueError::new_err(
                "This method can only be used on a Expression resulting from CaseExpression or When",
            ));
        };
        Ok(case.clone())
    }
    fn membership(&self, args: &Bound<'_, PyTuple>, not: bool) -> PyResult<Self> {
        if args.is_empty() {
            return Err(error::native(
                args.py(),
                &Error::invalid_input(format!(
                    "Incorrect amount of parameters to '{}', needs at least 1 parameter",
                    if not { "isnotin" } else { "isin" }
                )),
            ));
        }
        let mut children = vec![self.native.clone()];
        for value in args.iter() {
            children.push(Self::from_object(&value)?.native);
        }
        Ok(Self::new(roastedduck_parser::build::operator(
            if not { E::CompareNotIn } else { E::CompareIn },
            children,
        )))
    }
}
fn operator_function(name: &str, children: Vec<ParsedExpression>) -> ParsedExpression {
    let mut expression = roastedduck_parser::build::function(name, children);
    if let K::Function(function) = &mut expression.kind {
        function.is_operator = true;
    }
    expression
}
fn column_parts(py: Python<'_>, parts: Vec<String>) -> PyResult<PyExpression> {
    let names = if parts.len() == 1 {
        if parts[0] == "*" {
            return Ok(PyExpression::new(roastedduck_parser::build::star()));
        }
        let [catalog, schema, name] = crate::relation::qualified_name(py, &parts[0])?;
        let mut names = Vec::new();
        if !catalog.is_empty() {
            names.push(catalog);
        }
        if !schema.is_empty() {
            names.push(schema);
        }
        names.push(name);
        names
    } else {
        parts.into_iter().map(Into::into).collect()
    };
    Ok(PyExpression::new(roastedduck_parser::build::column_ref(names)))
}
pub(super) fn parse_list(py: Python<'_>, sql: &str) -> PyResult<Vec<ParsedExpression>> {
    roastedduck_parser::parse_expression_list(sql, &roastedduck_parser::ParserOptions::default())
        .map_err(|e| error::native(py, &e))
}
#[pymethods]
impl PyExpression {
    #[new]
    fn constructor(object: &Bound<'_, PyAny>) -> PyResult<Self> {
        Self::from_object(object)
    }
    fn __str__(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        conversion::text(py, &self.native.to_sql_bytes())
    }
    fn __repr__(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        self.__str__(py)
    }
    fn get_name(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        conversion::text(py, &self.native.name())
    }
    fn show(&self, py: Python<'_>) -> PyResult<()> {
        py.import("builtins")?
            .call_method1("print", (self.__str__(py)?,))?;
        Ok(())
    }
    fn alias(&self, name: &str) -> Self {
        let mut native = self.native.clone();
        native.alias = name.into();
        Self::new(native)
    }
    fn cast(&self, r#type: &Bound<'_, PyAny>) -> PyResult<Self> {
        let kind = r#type;
        let py = kind.py();
        let kind = if let Ok(kind) = kind.extract::<PyRef<'_, PyType>>() {
            kind.native.clone()
        } else {
            let name = if kind.is(py.get_type::<pyo3::types::PyString>()) {
                "VARCHAR".to_owned()
            } else if kind.is(py.get_type::<pyo3::types::PyInt>()) {
                "BIGINT".to_owned()
            } else if kind.is(py.get_type::<pyo3::types::PyFloat>()) {
                "DOUBLE".to_owned()
            } else if kind.is(py.get_type::<pyo3::types::PyBool>()) {
                "BOOLEAN".to_owned()
            } else {
                kind.extract::<String>()?
            };
            PyType::parse(py, &name)?.native
        };
        Ok(Self::new(roastedduck_parser::build::cast(
            kind,
            self.native.clone(),
            false,
        )))
    }
    fn __richcmp__(&self, other: &Bound<'_, PyAny>, op: CompareOp) -> PyResult<Self> {
        let kind = match op {
            CompareOp::Eq => E::CompareEqual,
            CompareOp::Ne => E::CompareNotequal,
            CompareOp::Lt => E::CompareLessthan,
            CompareOp::Le => E::CompareLessthanorequalto,
            CompareOp::Gt => E::CompareGreaterthan,
            CompareOp::Ge => E::CompareGreaterthanorequalto,
        };
        Ok(Self::new(roastedduck_parser::build::comparison(
            kind,
            self.native.clone(),
            Self::from_object(other)?.native,
        )))
    }
    fn __neg__(&self) -> Self {
        Self::new(operator_function("-", vec![self.native.clone()]))
    }
    fn __invert__(&self) -> Self {
        Self::new(roastedduck_parser::build::operator(
            E::OperatorNot,
            vec![self.native.clone()],
        ))
    }
    fn isnull(&self) -> Self {
        Self::new(roastedduck_parser::build::operator(
            E::OperatorIsNull,
            vec![self.native.clone()],
        ))
    }
    fn isnotnull(&self) -> Self {
        Self::new(roastedduck_parser::build::operator(
            E::OperatorIsNotNull,
            vec![self.native.clone()],
        ))
    }
    #[pyo3(signature=(*args))]
    fn isin(&self, args: &Bound<'_, PyTuple>) -> PyResult<Self> {
        self.membership(args, false)
    }
    #[pyo3(signature=(*args))]
    fn isnotin(&self, args: &Bound<'_, PyTuple>) -> PyResult<Self> {
        self.membership(args, true)
    }
    fn between(&self, lower: &Bound<'_, PyAny>, upper: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(Self::new(ParsedExpression::new(
            E::CompareBetween,
            K::Between(BetweenExpression {
                input: Box::new(self.native.clone()),
                lower: Box::new(Self::from_object(lower)?.native),
                upper: Box::new(Self::from_object(upper)?.native),
            }),
        )))
    }
    fn collate(&self, collation: &str) -> Self {
        Self::new(ParsedExpression::new(
            E::Collate,
            K::Collate(CollateExpression {
                child: Box::new(self.native.clone()),
                collation: collation.into(),
            }),
        ))
    }
    fn asc(&self) -> Self {
        let mut result = self.clone();
        result.order = OrderType::Ascending;
        result
    }
    fn desc(&self) -> Self {
        let mut result = self.clone();
        result.order = OrderType::Descending;
        result
    }
    fn nulls_first(&self) -> Self {
        let mut result = self.clone();
        result.null_order = OrderByNullType::NullsFirst;
        result
    }
    fn nulls_last(&self) -> Self {
        let mut result = self.clone();
        result.null_order = OrderByNullType::NullsLast;
        result
    }
    fn when(&self, condition: &Bound<'_, PyAny>, value: &Bound<'_, PyAny>) -> PyResult<Self> {
        let mut case = self.case()?;
        case.case_checks.push(CaseCheck {
            when_expr: Box::new(Self::from_object(condition)?.native),
            then_expr: Box::new(Self::from_object(value)?.native),
        });
        Ok(Self::new(ParsedExpression::new(E::CaseExpr, K::Case(case))))
    }
    fn otherwise(&self, value: &Bound<'_, PyAny>) -> PyResult<Self> {
        let mut case = self.case()?;
        *case.else_expr = Self::from_object(value)?.native;
        Ok(Self::new(ParsedExpression::new(E::CaseExpr, K::Case(case))))
    }
    fn __add__(&self, other: &Bound<'_, PyAny>) -> PyResult<Self> {
        self.binary(other, "+", false)
    }
    fn __radd__(&self, other: &Bound<'_, PyAny>) -> PyResult<Self> {
        self.binary(other, "+", true)
    }
    fn __sub__(&self, other: &Bound<'_, PyAny>) -> PyResult<Self> {
        self.binary(other, "-", false)
    }
    fn __rsub__(&self, other: &Bound<'_, PyAny>) -> PyResult<Self> {
        self.binary(other, "-", true)
    }
    fn __mul__(&self, other: &Bound<'_, PyAny>) -> PyResult<Self> {
        self.binary(other, "*", false)
    }
    fn __rmul__(&self, other: &Bound<'_, PyAny>) -> PyResult<Self> {
        self.binary(other, "*", true)
    }
    fn __truediv__(&self, other: &Bound<'_, PyAny>) -> PyResult<Self> {
        self.binary(other, "/", false)
    }
    fn __div__(&self, other: &Bound<'_, PyAny>) -> PyResult<Self> {
        self.binary(other, "/", false)
    }
    fn __rtruediv__(&self, other: &Bound<'_, PyAny>) -> PyResult<Self> {
        self.binary(other, "/", true)
    }
    fn __rdiv__(&self, other: &Bound<'_, PyAny>) -> PyResult<Self> {
        self.binary(other, "/", true)
    }
    fn __floordiv__(&self, other: &Bound<'_, PyAny>) -> PyResult<Self> {
        self.binary(other, "//", false)
    }
    fn __rfloordiv__(&self, other: &Bound<'_, PyAny>) -> PyResult<Self> {
        self.binary(other, "//", true)
    }
    fn __mod__(&self, other: &Bound<'_, PyAny>) -> PyResult<Self> {
        self.binary(other, "%", false)
    }
    fn __rmod__(&self, other: &Bound<'_, PyAny>) -> PyResult<Self> {
        self.binary(other, "%", true)
    }
    fn __pow__(&self, other: &Bound<'_, PyAny>, _modulo: Option<&Bound<'_, PyAny>>) -> PyResult<Self> {
        self.binary(other, "**", false)
    }
    fn __rpow__(&self, other: &Bound<'_, PyAny>, _modulo: Option<&Bound<'_, PyAny>>) -> PyResult<Self> {
        self.binary(other, "**", true)
    }
    fn __and__(&self, other: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(Self::new(roastedduck_parser::build::conjunction(
            E::ConjunctionAnd,
            self.native.clone(),
            Self::from_object(other)?.native,
        )))
    }
    fn __rand__(&self, other: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(Self::new(roastedduck_parser::build::conjunction(
            E::ConjunctionAnd,
            Self::from_object(other)?.native,
            self.native.clone(),
        )))
    }
    fn __or__(&self, other: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(Self::new(roastedduck_parser::build::conjunction(
            E::ConjunctionOr,
            self.native.clone(),
            Self::from_object(other)?.native,
        )))
    }
    fn __ror__(&self, other: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(Self::new(roastedduck_parser::build::conjunction(
            E::ConjunctionOr,
            Self::from_object(other)?.native,
            self.native.clone(),
        )))
    }
}

#[pyfunction(name="ColumnExpression",signature=(*args))]
fn column_expression(py: Python<'_>, args: &Bound<'_, PyTuple>) -> PyResult<PyExpression> {
    column_parts(
        py,
        args.iter()
            .map(|a| a.str()?.extract())
            .collect::<PyResult<Vec<_>>>()?,
    )
}
#[pyfunction(name = "ConstantExpression")]
fn constant_expression(value: &Bound<'_, PyAny>) -> PyResult<PyExpression> {
    Ok(PyExpression::new(ParsedExpression::constant(
        conversion::from_python(value)?,
    )))
}
#[pyfunction(name = "DefaultExpression")]
fn default_expression() -> PyExpression {
    PyExpression::new(ParsedExpression::new(E::ValueDefault, K::Default))
}
#[pyfunction(name = "SQLExpression")]
fn sql_expression(py: Python<'_>, expression: &str) -> PyResult<PyExpression> {
    let mut list = parse_list(py, expression)?;
    if list.len() != 1 {
        return Err(error::native(
            py,
            &Error::invalid_input(format!(
                "Please provide only a single expression to SQLExpression, found {} expressions in the parsed string",
                list.len()
            )),
        ));
    }
    Ok(PyExpression::new(list.remove(0)))
}
#[pyfunction(name="FunctionExpression",signature=(*args, **kwargs))]
fn function_expression(
    args: &Bound<'_, PyTuple>,
    kwargs: Option<&Bound<'_, pyo3::types::PyDict>>,
) -> PyResult<PyExpression> {
    let bad = || {
        error::arguments(
            args.py(),
            "FunctionExpression",
            "(function_name: str, *args) -> _duckdb.Expression",
            None,
            args,
            kwargs,
        )
    };
    if kwargs.is_some_and(|kwargs| {
        kwargs
            .iter()
            .any(|(key, _)| key.extract::<String>().ok().as_deref() != Some("function_name"))
    }) {
        return Err(bad()?);
    }
    let keyword = kwargs
        .map(|kwargs| kwargs.get_item("function_name"))
        .transpose()?
        .flatten();
    if !args.is_empty() && keyword.is_some() {
        return Err(bad()?);
    }
    let value = if args.is_empty() {
        keyword
    } else {
        Some(args.get_item(0)?)
    }
    .ok_or_else(|| bad().unwrap_or_else(|error| error))?;
    let Some(function_name) = crate::arguments::string_bytes(&value)? else {
        return Err(bad()?);
    };
    let children = args
        .iter()
        .skip(usize::from(!args.is_empty()))
        .map(|a| PyExpression::from_object(&a).map(|e| e.native))
        .collect::<PyResult<Vec<_>>>()?;
    Ok(PyExpression::new(roastedduck_parser::build::function(
        function_name,
        children,
    )))
}
#[pyfunction(name="CoalesceOperator",signature=(*args))]
fn coalesce_operator(args: &Bound<'_, PyTuple>) -> PyResult<PyExpression> {
    if args.is_empty() {
        return Err(error::native(
            args.py(),
            &Error::invalid_input("Please provide at least one argument"),
        ));
    }
    Ok(PyExpression::new(roastedduck_parser::build::operator(
        E::OperatorCoalesce,
        args.iter()
            .map(|a| PyExpression::from_object(&a).map(|e| e.native))
            .collect::<PyResult<Vec<_>>>()?,
    )))
}
#[pyfunction(name = "CaseExpression")]
fn case_expression(condition: &Bound<'_, PyAny>, value: &Bound<'_, PyAny>) -> PyResult<PyExpression> {
    Ok(PyExpression::new(ParsedExpression::new(
        E::CaseExpr,
        K::Case(CaseExpression {
            case_checks: vec![CaseCheck {
                when_expr: Box::new(PyExpression::from_object(condition)?.native),
                then_expr: Box::new(PyExpression::from_object(value)?.native),
            }],
            else_expr: Box::new(ParsedExpression::constant(Value::null(LogicalType::SQLNULL))),
        }),
    )))
}
#[pyfunction(name="StarExpression",signature=(*,exclude=None))]
fn star_expression(exclude: Option<&Bound<'_, PyAny>>) -> PyResult<PyExpression> {
    let mut native = roastedduck_parser::build::star();
    let K::Star(star) = &mut native.kind else {
        unreachable!()
    };
    if let Some(exclude) = exclude {
        for item in exclude.try_iter()? {
            let expression = PyExpression::from_object(&item?)?;
            let K::ColumnRef(column) = expression.native.kind else {
                return Err(pyo3::exceptions::PyValueError::new_err(
                    "Only ColumnExpressions are accepted Expression types here",
                ));
            };
            let mut name =
                QualifiedColumnName::column(column.column_names.last().cloned().unwrap_or_default());
            if column.column_names.len() > 1 {
                name.table = column.column_names[column.column_names.len() - 2].clone();
            }
            star.exclude_list.push(name);
        }
    }
    Ok(PyExpression::new(native))
}
#[pyfunction(name = "LambdaExpression")]
fn lambda_expression(lhs: &Bound<'_, PyAny>, rhs: &Bound<'_, PyAny>) -> PyResult<PyExpression> {
    let left = if let Ok(tuple) = lhs.cast::<PyTuple>() {
        let mut children = Vec::new();
        for value in tuple.iter() {
            let expression = if let Ok(parts) = value.cast::<PyTuple>() {
                column_parts(
                    lhs.py(),
                    parts
                        .iter()
                        .map(|p| p.str()?.extract::<String>())
                        .collect::<PyResult<Vec<_>>>()?,
                )?
            } else {
                PyExpression::from_object(&value)?
            };
            if !matches!(expression.native.kind, K::ColumnRef(_)) {
                return Err(pyo3::exceptions::PyValueError::new_err(
                    "'lhs' was provided as a tuple of columns, but one of the columns is not of type ColumnExpression",
                ));
            }
            children.push(expression.native);
        }
        roastedduck_parser::build::function("row", children)
    } else if lhs.is_instance_of::<PyString>() {
        let expression = PyExpression::from_object(lhs)?;
        if !matches!(expression.native.kind, K::ColumnRef(_)) {
            return Err(pyo3::exceptions::PyValueError::new_err(
                "'lhs' should be a valid ColumnExpression (or be used to create one)",
            ));
        }
        expression.native
    } else if let Ok(expression) = lhs.extract::<PyRef<'_, PyExpression>>() {
        if !matches!(expression.native.kind, K::ColumnRef(_)) {
            return Err(pyo3::exceptions::PyValueError::new_err(
                "'lhs' was an Expression, but is not of type ColumnExpression",
            ));
        }
        expression.native.clone()
    } else {
        return Err(pyo3::exceptions::PyValueError::new_err(
            "Please provide 'lhs' as either a tuple containing strings, or a single string",
        ));
    };
    Ok(PyExpression::new(ParsedExpression::new(
        E::Lambda,
        K::Lambda(LambdaExpression {
            lhs: Box::new(left),
            expr: Box::new(PyExpression::from_object(rhs)?.native),
            syntax_type: LambdaSyntaxType::SingleArrow,
        }),
    )))
}
pub(super) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<PyExpression>()?;
    module.add_function(wrap_pyfunction!(column_expression, module)?)?;
    module.add_function(wrap_pyfunction!(constant_expression, module)?)?;
    module.add_function(wrap_pyfunction!(default_expression, module)?)?;
    module.add_function(wrap_pyfunction!(sql_expression, module)?)?;
    module.add_function(wrap_pyfunction!(function_expression, module)?)?;
    module.add_function(wrap_pyfunction!(coalesce_operator, module)?)?;
    module.add_function(wrap_pyfunction!(case_expression, module)?)?;
    module.add_function(wrap_pyfunction!(star_expression, module)?)?;
    module.add_function(wrap_pyfunction!(lambda_expression, module)?)?;
    Ok(())
}
