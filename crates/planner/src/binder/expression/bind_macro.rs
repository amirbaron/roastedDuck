//! Macro calls (DuckDB's `MacroFunction::BindMacroFunction`, `bind_macro_expression.cpp`
//! and `Binder::BindTableMacro`): the call's arguments are matched to an overload, and
//! the macro body replaces the call with the parameters substituted by the arguments.

use std::collections::HashSet;

use roastedduck_ast::visit::{VisitorMut, walk_expression, walk_query_node};
use roastedduck_ast::{
    CastExpression, ExpressionType, FunctionExpression, MacroFunction, MacroFunctionKind, ParsedExpression,
    ParsedExpressionKind, QueryNode, lambda_parameters,
};
use roastedduck_common::{ByteString, CaseInsensitiveMap, Error, Result};
use roastedduck_types::{LogicalType, LogicalTypeId};

use crate::binder::{Binder, Binding, DUMMY_NAME};
use crate::expression_binder::{BaseBinder, BindResult, ExpressionBinder, qualify_column_names};

/// The cost of passing an argument to an untyped parameter (DuckDB's `1000000`).
const UNTYPED_PARAMETER_COST: u64 = 1_000_000;

/// The arguments of a macro call matched to the chosen overload.
#[derive(Debug)]
pub struct MacroArguments {
    /// The chosen overload.
    pub index: usize,
    /// The positional arguments.
    pub positional: Vec<ParsedExpression>,
    /// The named arguments and the defaults of parameters without argument.
    pub named: CaseInsensitiveMap<ParsedExpression, ByteString>,
}

/// The name of a macro parameter (`ColumnRefExpression::GetColumnName`).
pub fn parameter_name(parameter: &ParsedExpression) -> &[u8] {
    match &parameter.kind {
        ParsedExpressionKind::ColumnRef(column) => {
            column.column_names.last().map_or(b"", ByteString::as_bytes)
        }
        _ => b"",
    }
}

/// The types of the parameters, `UNKNOWN` where untyped.
fn parameter_types(function: &MacroFunction) -> Vec<LogicalType> {
    let mut types = function.types.clone();
    types.resize(function.parameters.len(), LogicalType::UNKNOWN);
    types
}

/// `name(a, b INTEGER, c := 1)` for error messages (DuckDB's `FormatMacroFunction`).
pub fn format_macro_function(function: &MacroFunction, name: impl AsRef<[u8]>) -> ByteString {
    let name = name.as_ref();
    let parameters: Vec<ByteString> = function
        .parameters
        .iter()
        .enumerate()
        .map(|(index, parameter)| {
            let parameter_name = parameter_name(parameter);
            let mut text = ByteString::from(parameter_name);
            if let Some(ty) = function.types.get(index)
                && ty.id() != LogicalTypeId::Unknown
            {
                text.push(b' ');
                text.push_bytes(ty.to_bytes());
            }
            if let Some(default) = function.default_parameters.get(parameter_name) {
                text.push_bytes(" := ");
                text.push_bytes(default.to_sql_bytes());
            }
            text
        })
        .collect();
    roastedduck_common::byte_format!("{name}({})", ByteString::join(&parameters, ", "))
}

/// A cast of a macro argument to its parameter's type.
fn cast_argument(ty: &LogicalType, argument: ParsedExpression) -> ParsedExpression {
    ParsedExpression::new(
        ExpressionType::OperatorCast,
        ParsedExpressionKind::Cast(CastExpression {
            child: Box::new(argument),
            cast_type: ty.clone(),
            try_cast: false,
        }),
    )
}

/// The cost of passing an argument of type `argument` to a parameter of type `parameter`;
/// `None` if no implicit cast exists.
fn argument_cost(binder: &Binder, argument: &LogicalType, parameter: &LogicalType) -> Option<u64> {
    if parameter.id() == LogicalTypeId::Unknown {
        return Some(UNTYPED_PARAMETER_COST);
    }
    u64::try_from(binder.context.casts.implicit_cast_cost(argument, parameter)).ok()
}

/// The cost of calling `function` with the arguments; `None` if it cannot be called with
/// them.
fn overload_cost(
    binder: &Binder,
    function: &MacroFunction,
    positional_types: &[LogicalType],
    named: &CaseInsensitiveMap<ParsedExpression, ByteString>,
    named_types: &CaseInsensitiveMap<LogicalType, ByteString>,
) -> Option<u64> {
    let types = parameter_types(function);
    let parameter_count = function.parameters.len();
    if positional_types.len() > parameter_count || positional_types.len() + named.len() > parameter_count {
        return None;
    }
    let all_named_exist = named.iter().all(|(name, _)| {
        function
            .parameters
            .iter()
            .any(|parameter| parameter_name(parameter).eq_ignore_ascii_case(name))
    });
    if !all_named_exist {
        return None;
    }
    let mut cost = 0;
    for (index, argument_type) in positional_types.iter().enumerate() {
        if named.contains_key(parameter_name(&function.parameters[index])) {
            return None;
        }
        cost += argument_cost(binder, argument_type, &types[index])?;
    }
    for (parameter, ty) in function
        .parameters
        .iter()
        .zip(&types)
        .skip(positional_types.len())
    {
        let name = parameter_name(parameter);
        match named_types.get(name) {
            Some(argument_type) => cost += argument_cost(binder, argument_type, ty)?,
            None if function.default_parameters.contains_key(name) => {}
            // The parameter has no argument.
            None => return None,
        }
    }
    Some(cost)
}

/// Separates the arguments of a macro call into positional and named ones and picks the
/// overload they fit best (DuckDB's `MacroFunction::BindMacroFunction`). Typed parameters
/// get casts, and parameters without argument their default. `Ok(Err(message))` if no
/// single overload fits.
pub fn bind_macro_function(
    binder: &mut Binder,
    functions: &[MacroFunction],
    name: impl AsRef<[u8]>,
    function: &mut FunctionExpression,
    depth: u64,
) -> Result<std::result::Result<MacroArguments, ByteString>> {
    let name = name.as_ref();
    let requires_bind = functions
        .iter()
        .any(|function| function.types.iter().any(|ty| ty.id() != LogicalTypeId::Unknown));
    let mut positional = Vec::new();
    let mut positional_types = Vec::new();
    let mut named: CaseInsensitiveMap<ParsedExpression, ByteString> = CaseInsensitiveMap::default();
    let mut named_types: CaseInsensitiveMap<LogicalType, ByteString> = CaseInsensitiveMap::default();
    for argument in std::mem::take(&mut function.children) {
        let mut argument_type = LogicalType::UNKNOWN;
        if requires_bind {
            let mut copy = argument.clone();
            let mut expression_binder = BaseBinder::new(binder, None);
            if let BindResult::Bound(bound) =
                expression_binder.bind_expression(&mut copy, depth + 1, false)?
            {
                argument_type = bound.return_type;
            }
        }
        if !argument.alias.is_empty() {
            if named.contains_key(&argument.alias) {
                return Ok(Err(roastedduck_common::byte_format!(
                    "Macro {name}() has named argument repeated '{}'",
                    argument.alias
                )));
            }
            named_types.insert(argument.alias.clone(), argument_type);
            named.insert(argument.alias.clone(), argument);
        } else if !named.is_empty() {
            return Ok(Err(roastedduck_common::byte_format!(
                "Macro {name}() has positional argument following named argument"
            )));
        } else {
            positional.push(argument);
            positional_types.push(argument_type);
        }
    }

    let mut lowest_cost = u64::MAX;
    let mut candidates: Vec<usize> = Vec::new();
    for (index, function) in functions.iter().enumerate() {
        let Some(cost) = overload_cost(binder, function, &positional_types, &named, &named_types) else {
            continue;
        };
        if cost <= lowest_cost {
            if cost < lowest_cost {
                lowest_cost = cost;
                candidates.clear();
            }
            candidates.push(index);
        }
    }
    let [index] = candidates.as_slice() else {
        let (message, listed): (ByteString, Vec<&MacroFunction>) = if candidates.is_empty() {
            (
                roastedduck_common::byte_format!(
                    "Macro {name}() does not support the supplied arguments. You might need to add explicit type \
                     casts.\nCandidate macros:"
                ),
                functions.iter().collect(),
            )
        } else {
            (
                roastedduck_common::byte_format!(
                    "Macro {name}() has multiple overloads that match the supplied arguments.\nIn order to select \
                     one, please supply all arguments by name, and/or add explicit type casts.\nCandidate macros:"
                ),
                candidates.iter().map(|&index| &functions[index]).collect(),
            )
        };
        let mut message = message;
        for function in listed {
            message.push_bytes("\n\t");
            message.push_bytes(format_macro_function(function, name));
        }
        return Ok(Err(message));
    };
    let index = *index;
    let definition = &functions[index];
    let types = parameter_types(definition);
    let cast_in_place = |argument: &mut ParsedExpression, ty: &LogicalType| {
        if ty.id() != LogicalTypeId::Unknown {
            let value = std::mem::replace(
                argument,
                ParsedExpression::constant(roastedduck_types::Value::default()),
            );
            *argument = cast_argument(ty, value);
        }
    };
    for (argument, ty) in positional.iter_mut().zip(&types) {
        cast_in_place(argument, ty);
    }
    for (parameter, ty) in definition.parameters.iter().zip(&types).skip(positional.len()) {
        let parameter = parameter_name(parameter);
        if let Some(argument) = named.get_mut(parameter) {
            cast_in_place(argument, ty);
            continue;
        }
        let mut argument = definition
            .default_parameters
            .get(parameter)
            .cloned()
            .ok_or_else(|| Error::internal("macro parameter without argument"))?;
        cast_in_place(&mut argument, ty);
        named.insert(parameter.to_owned(), argument);
    }
    Ok(Ok(MacroArguments {
        index,
        positional,
        named,
    }))
}

/// The binding of a macro's parameters to the call's arguments (DuckDB's
/// `MacroFunction::CreateDummyBinding`).
pub fn create_dummy_binding(
    definition: &MacroFunction,
    name: impl AsRef<[u8]>,
    arguments: MacroArguments,
) -> Result<Binding> {
    let name = name.as_ref();
    let types = parameter_types(definition);
    let MacroArguments {
        positional, named, ..
    } = arguments;
    let mut names: Vec<ByteString> = definition
        .parameters
        .iter()
        .take(positional.len())
        .map(|parameter| ByteString::from(parameter_name(parameter)))
        .collect();
    let mut values = positional;
    for (parameter, argument) in named {
        names.push(parameter);
        values.push(argument);
    }
    // The types follow the definition's order; the names the order of the arguments.
    let mut binding = Binding::dummy(types, names, name)?;
    if let crate::binder::BindingKind::Dummy { arguments } = &mut binding.kind {
        *arguments = values;
    }
    Ok(binding)
}

/// Replaces references to macro parameters by the arguments (DuckDB's
/// `ExpressionBinder::ReplaceMacroParameters`); lambda parameters shadow them.
struct ParameterReplacer<'a> {
    binding: &'a Binding,
    lambda_parameters: Vec<HashSet<ByteString>>,
    error: Option<Error>,
}

impl ParameterReplacer<'_> {
    fn is_lambda_parameter(&self, name: &[u8]) -> bool {
        self.lambda_parameters.iter().any(|level| level.contains(name))
    }

    /// `ReplaceMacroParametersInLambda`: only the bodies of lambdas are replaced in.
    fn replace_in_lambda(&mut self, function: &mut FunctionExpression) {
        for child in &mut function.children {
            let ParsedExpressionKind::Lambda(lambda) = &mut child.kind else {
                self.visit_expression(child);
                continue;
            };
            let Some(parameters) = lambda_parameters(lambda) else {
                // Possibly a JSON function: replace on both sides.
                self.visit_expression(&mut lambda.lhs);
                self.visit_expression(&mut lambda.expr);
                continue;
            };
            let names = parameters.iter().map(|parameter| parameter.name()).collect();
            self.lambda_parameters.push(names);
            self.visit_expression(&mut lambda.expr);
            self.lambda_parameters.pop();
        }
    }
}

impl VisitorMut for ParameterReplacer<'_> {
    fn visit_expression(&mut self, expression: &mut ParsedExpression) {
        match &mut expression.kind {
            ParsedExpressionKind::ColumnRef(column) => {
                let Some(column_name) = column.column_names.last() else {
                    return;
                };
                if column.column_names.len() == 1 && self.is_lambda_parameter(column_name) {
                    return;
                }
                let replace = if column.column_names.len() > 1 {
                    column.column_names[column.column_names.len() - 2]
                        .windows(DUMMY_NAME.len())
                        .any(|part| part == DUMMY_NAME.as_bytes())
                } else {
                    self.binding.has_matching_binding(column_name)
                };
                if replace {
                    match self.binding.param_to_arg(column_name, &expression.alias) {
                        Ok(argument) => *expression = argument,
                        Err(error) => {
                            self.error.get_or_insert(error);
                        }
                    }
                }
            }
            ParsedExpressionKind::Function(function)
                if crate::expression_binder::is_lambda_function(function) =>
            {
                self.replace_in_lambda(function);
            }
            _ => walk_expression(self, expression),
        }
    }
}

/// Replaces the parameters of `binding` in `expression`.
pub fn replace_macro_parameters(expression: &mut ParsedExpression, binding: &Binding) -> Result<()> {
    let mut replacer = ParameterReplacer {
        binding,
        lambda_parameters: Vec::new(),
        error: None,
    };
    replacer.visit_expression(expression);
    replacer.error.map_or(Ok(()), Err)
}

/// Replaces the parameters of `binding` in every expression of a query node.
pub fn replace_macro_parameters_in_node(node: &mut QueryNode, binding: &Binding) -> Result<()> {
    let mut replacer = ParameterReplacer {
        binding,
        lambda_parameters: Vec::new(),
        error: None,
    };
    walk_query_node(&mut replacer, node);
    replacer.error.map_or(Ok(()), Err)
}

/// Replaces the macro call `expr` by the macro's body with the arguments substituted
/// (DuckDB's `ExpressionBinder::UnfoldMacroExpression`). For a window call (a macro used
/// as aggregate in `OVER`), the body must be a function call, which takes the window's
/// place.
pub fn unfold_macro_expression(
    binder: &mut Binder,
    function: &mut FunctionExpression,
    name: impl AsRef<[u8]>,
    macros: &[MacroFunction],
    expr: &mut ParsedExpression,
    depth: u64,
) -> Result<()> {
    let name = name.as_ref();
    let location = expr.query_location;
    let arguments = match bind_macro_function(binder, macros, name, function, depth)? {
        Ok(arguments) => arguments,
        Err(message) => return Err(Error::binder(message).with_position(location)),
    };
    let definition = &macros[arguments.index];
    let MacroFunctionKind::Scalar(scalar) = &definition.kind else {
        return Err(Error::internal("expected a scalar macro"));
    };
    let binding = create_dummy_binding(definition, name, arguments)?;
    if let ParsedExpressionKind::Window(window) = &mut expr.kind {
        let ParsedExpressionKind::Function(body) = &scalar.expression.kind else {
            return Err(Error::binder("Window function macros must be functions"));
        };
        let body = body.clone();
        window.catalog = body.catalog;
        window.schema = body.schema;
        window.function_name = body.function_name;
        window.children = body.children;
        window.distinct = body.distinct;
        window.filter_expr = body.filter;
    } else {
        *expr = (*scalar.expression).clone();
    }
    // Qualify only the macro parameters, with a binder that knows nothing else.
    let mut dummy_binder = Binder::new(binder.context.clone())?;
    dummy_binder.macro_binding = Some(binding.clone());
    qualify_column_names(&dummy_binder, expr)?;
    replace_macro_parameters(expr, &binding)
}

/// A call of a scalar macro (DuckDB's `ExpressionBinder::BindMacro`): unfolds the macro
/// and binds the result.
pub fn bind_macro<B: ExpressionBinder + ?Sized>(
    binder: &mut B,
    expr: &mut ParsedExpression,
    name: impl AsRef<[u8]>,
    macros: &[MacroFunction],
    depth: u64,
) -> Result<BindResult> {
    let name = name.as_ref();
    let ParsedExpressionKind::Function(function) = &mut expr.kind else {
        return Err(Error::internal("expected a function call"));
    };
    let mut function = std::mem::replace(function, Box::new(empty_function()));
    unfold_macro_expression(binder.binder(), &mut function, name, macros, expr, depth)?;
    binder.bind_expression(expr, depth, false)
}

fn empty_function() -> FunctionExpression {
    FunctionExpression {
        function_name: String::new().into(),
        schema: String::new().into(),
        children: Vec::new(),
        filter: None,
        order_bys: roastedduck_ast::OrderModifier { orders: Vec::new() },
        distinct: false,
        is_operator: false,
        export_state: false,
        catalog: String::new().into(),
    }
}
