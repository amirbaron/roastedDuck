//! Overload resolution and binding of function calls (DuckDB's `FunctionBinder`), and the
//! type rules shared by the expression binders: implicit casts
//! (`BoundCastExpression::AddCastToType`), comparison types
//! (`BoundComparisonExpression::TryBindComparison`) and type unification.

use std::fmt::Write as _;
use std::sync::{Arc, PoisonError};

use rustdb_ast::ExpressionType;
use rustdb_common::{CaseInsensitiveMap, Error, Result};
use rustdb_function::{
    AggregateFunction, AggregateFunctionSet, BindInput, FunctionData, NullHandling, ScalarFunction,
    ScalarFunctionSet, TableFunction, TableFunctionSet,
};
use rustdb_types::{LogicalType, LogicalTypeId, TypeInfo, Value};

use crate::context::ClientContext;
use crate::expression::{
    AggregateType, BoundAggregateExpression, BoundCastExpression, BoundOrderModifier, Expression,
    ExpressionKind,
};
use crate::expression_binder::{BindError, collation, expression_return_type, parameter_not_resolved};

/// Whether a type is usable as a cast target (DuckDB's `LogicalType::IsValid`).
fn is_valid_target(ty: &LogicalType) -> bool {
    !matches!(ty.id(), LogicalTypeId::Invalid | LogicalTypeId::Unknown)
}

/// Adds a cast of `expression` to `target` unless it already has that type
/// (DuckDB's `BoundCastExpression::AddCastToType`).
pub fn add_cast_to_type(
    context: &ClientContext,
    mut expression: Expression,
    target: &LogicalType,
    try_cast: bool,
) -> Result<Expression> {
    if let ExpressionKind::Parameter(parameter) = &expression.kind {
        let mut data = parameter
            .parameter_data
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if !is_valid_target(target) {
            data.return_type = LogicalType::INVALID;
        } else if matches!(data.return_type.id(), LogicalTypeId::Invalid) {
            // the parameter's type is unknown
        } else if data.return_type.id() == LogicalTypeId::Unknown {
            data.return_type = target.clone();
        } else if &data.return_type != target {
            data.return_type = LogicalType::INVALID;
        }
        drop(data);
        expression.return_type = target.clone();
        return Ok(expression);
    }
    if matches!(expression.kind, ExpressionKind::Default) {
        expression.return_type = target.clone();
    }
    if !is_valid_target(target) {
        return Ok(expression);
    }
    if &expression_return_type(&expression) == target {
        return Ok(expression);
    }
    if target.id() == LogicalTypeId::List
        && expression.return_type.id() == LogicalTypeId::List
        && let (Some(target_child), Some(child)) = (target.child_type(), expression.return_type.child_type())
        && (target_child.id() == LogicalTypeId::Any || target_child == child)
    {
        return Ok(expression);
    }
    let bound_cast = context.casts.get_cast_function(&expression.return_type, target)?;
    let location = expression.query_location;
    let mut cast = Expression::new(
        ExpressionType::OperatorCast,
        target.clone(),
        ExpressionKind::Cast(Box::new(BoundCastExpression {
            child: expression,
            try_cast,
            bound_cast,
        })),
    );
    cast.query_location = location;
    Ok(cast)
}

/// The concrete type of a literal type (DuckDB's `LogicalType::NormalizeType`).
pub fn normalize_type(ty: &LogicalType) -> Result<LogicalType> {
    match ty.id() {
        LogicalTypeId::StringLiteral => Ok(LogicalType::VARCHAR),
        LogicalTypeId::IntegerLiteral => {
            Ok(ty.integer_literal_type().cloned().unwrap_or(LogicalType::INTEGER))
        }
        LogicalTypeId::Unknown => Err(parameter_not_resolved()),
        _ => Ok(ty.clone()),
    }
}

/// The common type of two types, or a Not implemented error (DuckDB's `MaxLogicalType`).
pub fn max_logical_type(left: &LogicalType, right: &LogicalType) -> Result<LogicalType> {
    LogicalType::try_max_logical_type(left, right).ok_or_else(|| {
        Error::not_implemented(format!(
            "Cannot combine types {left} and {right} - an explicit cast is required"
        ))
    })
}

fn decimal_properties(ty: &LogicalType) -> Option<(u8, u8)> {
    if ty.id() == LogicalTypeId::SqlNull {
        return Some((0, 0));
    }
    ty.numeric_decimal_properties()
}

/// Types compared as numbers rather than strings when compared with a VARCHAR
/// (DuckDB's `SwitchVarcharComparison`).
fn switch_varchar_comparison(ty: &LogicalType) -> bool {
    use LogicalTypeId as L;
    matches!(
        ty.id(),
        L::Boolean
            | L::TinyInt
            | L::SmallInt
            | L::Integer
            | L::BigInt
            | L::HugeInt
            | L::Float
            | L::Double
            | L::Decimal
            | L::UTinyInt
            | L::USmallInt
            | L::UInteger
            | L::UBigInt
            | L::UHugeInt
            | L::Date
            | L::Time
            | L::TimestampSec
            | L::TimestampMs
            | L::Timestamp
            | L::TimestampNs
            | L::Interval
            | L::TimestampTz
            | L::TimeTz
            | L::IntegerLiteral
    )
}

/// The type two sides of a comparison are compared at; `None` if they cannot be compared
/// (DuckDB's `BoundComparisonExpression::TryBindComparison`).
pub fn try_bind_comparison(
    left: &LogicalType,
    right: &LogicalType,
    comparison: ExpressionType,
) -> Result<Option<LogicalType>> {
    let is_equality = matches!(
        comparison,
        ExpressionType::CompareEqual
            | ExpressionType::CompareNotequal
            | ExpressionType::CompareIn
            | ExpressionType::CompareNotIn
            | ExpressionType::CompareDistinctFrom
            | ExpressionType::CompareNotDistinctFrom
    );
    let result = if is_equality {
        LogicalType::force_max_logical_type(left, right)
    } else {
        match LogicalType::try_max_logical_type(left, right) {
            Some(result) => result,
            None => return Ok(None),
        }
    };
    match result.id() {
        LogicalTypeId::Decimal => {
            let (Some((left_width, left_scale)), Some((right_width, right_scale))) =
                (decimal_properties(left), decimal_properties(right))
            else {
                return Ok(Some(result));
            };
            let max_scale = left_scale.max(right_scale);
            let max_width_over_scale = (left_width - left_scale).max(right_width - right_scale);
            let max_width = left_width
                .max(right_width)
                .max(max_scale + max_width_over_scale)
                .min(38);
            Ok(Some(LogicalType::decimal(max_width, max_scale)))
        }
        LogicalTypeId::Varchar => {
            if left.id() != LogicalTypeId::Varchar && switch_varchar_comparison(left) {
                return normalize_type(left).map(Some);
            }
            if right.id() != LogicalTypeId::Varchar && switch_varchar_comparison(right) {
                return normalize_type(right).map(Some);
            }
            if let (Some(l), Some(r)) = (collation(left), collation(right))
                && l != r
            {
                return Err(Error::binder("Cannot combine types with different collation!"));
            }
            Ok(Some(result))
        }
        _ => Ok(Some(result)),
    }
}

/// A function signature for overload resolution (DuckDB's `SimpleFunction`).
pub trait FunctionSignature {
    fn name(&self) -> &str;
    fn arguments(&self) -> &[LogicalType];
    fn varargs(&self) -> Option<&LogicalType>;
    /// How the overload prints in error messages (DuckDB's `ToString`).
    fn signature(&self) -> String;
}

/// `name(A, B[, [C...]])` (DuckDB's `Function::CallToString`).
pub fn call_to_string(name: &str, arguments: &[LogicalType], varargs: Option<&LogicalType>) -> String {
    let mut parts: Vec<String> = arguments.iter().map(ToString::to_string).collect();
    if let Some(varargs) = varargs
        && varargs.id() != LogicalTypeId::Invalid
    {
        parts.push(format!("[{varargs}...]"));
    }
    format!("{name}({})", parts.join(", "))
}

impl FunctionSignature for ScalarFunction {
    fn name(&self) -> &str {
        &self.name
    }

    fn arguments(&self) -> &[LogicalType] {
        &self.arguments
    }

    fn varargs(&self) -> Option<&LogicalType> {
        self.varargs.as_ref()
    }

    fn signature(&self) -> String {
        format!(
            "{} -> {}",
            call_to_string(&self.name, &self.arguments, self.varargs.as_ref()),
            self.return_type
        )
    }
}

impl FunctionSignature for AggregateFunction {
    fn name(&self) -> &str {
        &self.name
    }

    fn arguments(&self) -> &[LogicalType] {
        &self.arguments
    }

    fn varargs(&self) -> Option<&LogicalType> {
        self.varargs.as_ref()
    }

    fn signature(&self) -> String {
        format!(
            "{} -> {}",
            call_to_string(&self.name, &self.arguments, self.varargs.as_ref()),
            self.return_type
        )
    }
}

impl FunctionSignature for TableFunction {
    fn name(&self) -> &str {
        &self.name
    }

    fn arguments(&self) -> &[LogicalType] {
        &self.arguments
    }

    fn varargs(&self) -> Option<&LogicalType> {
        self.varargs.as_ref()
    }

    fn signature(&self) -> String {
        let mut parts: Vec<String> = self.arguments.iter().map(ToString::to_string).collect();
        parts.extend(
            self.named_parameters
                .iter()
                .map(|(name, ty)| format!("{name} : {ty}")),
        );
        format!("{}({})", self.name, parts.join(", "))
    }
}

/// Binds calls against function sets (DuckDB's `FunctionBinder`).
#[derive(Debug)]
pub struct FunctionBinder<'a> {
    context: &'a ClientContext,
}

impl<'a> FunctionBinder<'a> {
    pub fn new(context: &'a ClientContext) -> Self {
        Self { context }
    }

    fn implicit_cast_cost(&self, from: &LogicalType, to: &LogicalType) -> i64 {
        self.context.casts.implicit_cast_cost(from, to)
    }

    /// The cost of calling a variadic overload (DuckDB's `BindVarArgsFunctionCost`).
    fn varargs_cost<F: FunctionSignature>(&self, function: &F, arguments: &[LogicalType]) -> Option<u64> {
        if arguments.len() < function.arguments().len() {
            return None;
        }
        let varargs = function.varargs()?;
        let mut cost = 0u64;
        for (i, argument) in arguments.iter().enumerate() {
            let target = function.arguments().get(i).unwrap_or(varargs);
            if argument == target {
                continue;
            }
            let cast_cost = self.implicit_cast_cost(argument, target);
            if cast_cost < 0 {
                return None;
            }
            cost += cast_cost as u64;
        }
        Some(cost)
    }

    /// The cost of calling an overload with `arguments`, or `None` if impossible
    /// (DuckDB's `BindFunctionCost`).
    pub fn function_cost<F: FunctionSignature>(
        &self,
        function: &F,
        arguments: &[LogicalType],
    ) -> Option<u64> {
        if function
            .varargs()
            .is_some_and(|varargs| varargs.id() != LogicalTypeId::Invalid)
        {
            return self.varargs_cost(function, arguments);
        }
        if function.arguments().len() != arguments.len() {
            return None;
        }
        let mut cost = 0u64;
        let mut has_parameter = false;
        for (argument, target) in arguments.iter().zip(function.arguments()) {
            if argument.id() == LogicalTypeId::Unknown {
                has_parameter = true;
                continue;
            }
            let cast_cost = self.implicit_cast_cost(argument, target);
            if cast_cost < 0 {
                return None;
            }
            cost += cast_cost as u64;
        }
        Some(if has_parameter { 0 } else { cost })
    }

    /// The best overload for `arguments` (DuckDB's `BindFunctionFromArguments`): `Ok(Err)`
    /// is a soft "no function matches"/"could not choose" error.
    pub fn bind_function<F: FunctionSignature>(
        &self,
        name: &str,
        functions: &[F],
        arguments: &[LogicalType],
    ) -> Result<std::result::Result<usize, Error>> {
        let mut lowest_cost = u64::MAX;
        let mut candidates: Vec<usize> = Vec::new();
        let mut best = None;
        for (index, function) in functions.iter().enumerate() {
            let Some(cost) = self.function_cost(function, arguments) else {
                continue;
            };
            if cost == lowest_cost {
                candidates.push(index);
                continue;
            }
            if cost > lowest_cost {
                continue;
            }
            candidates.clear();
            lowest_cost = cost;
            best = Some(index);
        }
        let Some(best) = best else {
            let call = call_to_string(name, arguments, None);
            let list = functions.iter().fold(String::new(), |mut list, f| {
                let _ = writeln!(list, "\t{}", f.signature());
                list
            });
            return Ok(Err(Error::binder(format!(
                "No function matches the given name and argument types '{call}'. You might need to add \
                 explicit type casts.\n\tCandidate functions:\n{list}"
            ))));
        };
        candidates.push(best);
        if candidates.len() > 1 {
            if arguments
                .iter()
                .any(|argument| argument.id() == LogicalTypeId::Unknown)
            {
                return Err(parameter_not_resolved());
            }
            let call = call_to_string(name, arguments, None);
            let list = candidates.iter().fold(String::new(), |mut list, &index| {
                let _ = writeln!(list, "\t{}", functions[index].signature());
                list
            });
            return Ok(Err(Error::binder(format!(
                "Could not choose a best candidate function for the function call \"{call}\". In order to select \
                 one, please add explicit type casts.\n\tCandidate functions:\n{list}"
            ))));
        }
        Ok(Ok(best))
    }

    /// The binder's argument types of `children` (DuckDB's `GetLogicalTypesFromExpressions`).
    pub fn argument_types(children: &[Expression]) -> Vec<LogicalType> {
        children.iter().map(expression_return_type).collect()
    }

    /// The input of a bind callback.
    fn bind_input(&self, children: &[Expression]) -> (Vec<LogicalType>, Vec<Option<Value>>) {
        let types = children.iter().map(|child| child.return_type.clone()).collect();
        let constants = children
            .iter()
            .map(|child| {
                child
                    .is_foldable()
                    .then(|| self.context.evaluator.evaluate_scalar(child).ok())
                    .flatten()
            })
            .collect();
        (types, constants)
    }

    /// Casts `children` to the overload's argument types (DuckDB's `CastToFunctionArguments`).
    fn cast_to_arguments(
        &self,
        name: &str,
        arguments: &mut [LogicalType],
        varargs: &mut Option<LogicalType>,
        children: Vec<Expression>,
    ) -> Result<Vec<Expression>> {
        for argument in arguments.iter_mut().chain(varargs.iter_mut()) {
            prepare_type_for_cast(argument);
        }
        let mut result = Vec::with_capacity(children.len());
        for (i, child) in children.into_iter().enumerate() {
            let target = arguments.get(i).or(varargs.as_ref()).cloned().unwrap_or_default();
            if matches!(
                target.id(),
                LogicalTypeId::StringLiteral | LogicalTypeId::IntegerLiteral
            ) {
                return Err(Error::internal(format!(
                    "Function {name} returned a STRING_LITERAL or INTEGER_LITERAL type - return an explicit type \
                     instead"
                )));
            }
            if child.return_type.id() == LogicalTypeId::Lambda || !requires_cast(&child.return_type, &target)
            {
                result.push(child);
            } else {
                result.push(add_cast_to_type(self.context, child, &target, false)?);
            }
        }
        Ok(result)
    }

    /// Binds a call to the scalar function set (DuckDB's `BindScalarFunction(entry, ...)`):
    /// `Ok(Err)` is a soft error when no overload matches.
    pub fn bind_scalar_function(
        &self,
        functions: &ScalarFunctionSet,
        children: Vec<Expression>,
        is_operator: bool,
    ) -> Result<std::result::Result<Expression, BindError>> {
        let types = Self::argument_types(&children);
        let index = match self.bind_function(&functions.name, &functions.functions, &types)? {
            Ok(index) => index,
            Err(error) => return Ok(Err(error.into())),
        };
        let function = functions.functions[index].clone();
        if function.null_handling == NullHandling::Default {
            let null_type = if function.return_type.is_valid() {
                function.return_type.clone()
            } else {
                LogicalType::SQLNULL
            };
            for child in &children {
                if child.return_type.id() == LogicalTypeId::SqlNull {
                    return Ok(Ok(Expression::constant(Value::null(null_type))));
                }
                if child.is_foldable()
                    && let Ok(value) = self.context.evaluator.evaluate_scalar(child)
                    && value.is_null()
                {
                    return Ok(Ok(Expression::constant(Value::null(null_type))));
                }
            }
        }
        self.bind_scalar_overload(function, children, is_operator).map(Ok)
    }

    /// Binds a call to one chosen overload (DuckDB's `BindScalarFunction(ScalarFunction, ...)`).
    pub fn bind_scalar_overload(
        &self,
        mut function: ScalarFunction,
        mut children: Vec<Expression>,
        is_operator: bool,
    ) -> Result<Expression> {
        resolve_template_types(
            &mut function.arguments,
            &mut function.varargs,
            &mut function.return_type,
            &children,
        );
        let mut bind_info: Option<Arc<dyn FunctionData>> = None;
        if let Some(bind) = function.bind.clone() {
            let (argument_types, constants) = self.bind_input(&children);
            let aliases: Vec<String> = children.iter().map(|child| child.alias.clone()).collect();
            bind_info = bind(
                &mut function,
                &BindInput {
                    argument_types: &argument_types,
                    constants: &constants,
                    aliases: &aliases,
                },
            )?;
            // The bind may have dropped trailing arguments (DuckDB's `EraseArgument`).
            if function.varargs.is_none() {
                children.truncate(function.arguments.len());
            }
        }
        check_template_types_resolved(
            &function.name,
            &function.arguments,
            function.varargs.as_ref(),
            &function.return_type,
        )?;
        let mut arguments = std::mem::take(&mut function.arguments);
        let mut varargs = function.varargs.take();
        let children = self.cast_to_arguments(&function.name, &mut arguments, &mut varargs, children)?;
        function.arguments = arguments;
        function.varargs = varargs;
        Ok(Expression::function(function, children, bind_info, is_operator))
    }

    /// Binds an aggregate call to a chosen overload (DuckDB's `BindAggregateFunction`).
    pub fn bind_aggregate_function(
        &self,
        mut function: AggregateFunction,
        children: Vec<Expression>,
        filter: Option<Expression>,
        aggr_type: AggregateType,
        order_bys: Option<BoundOrderModifier>,
    ) -> Result<Expression> {
        resolve_template_types(
            &mut function.arguments,
            &mut function.varargs,
            &mut function.return_type,
            &children,
        );
        let mut bind_info = None;
        let mut children = children;
        if let Some(bind) = function.bind.clone() {
            let (argument_types, constants) = self.bind_input(&children);
            let aliases: Vec<String> = children.iter().map(|child| child.alias.clone()).collect();
            bind_info = bind(
                &mut function,
                &BindInput {
                    argument_types: &argument_types,
                    constants: &constants,
                    aliases: &aliases,
                },
            )?;
            // The bind may have dropped arguments.
            children.truncate(function.arguments.len());
        }
        check_template_types_resolved(
            &function.name,
            &function.arguments,
            function.varargs.as_ref(),
            &function.return_type,
        )?;
        let mut arguments = std::mem::take(&mut function.arguments);
        let mut varargs = function.varargs.take();
        let children = self.cast_to_arguments(&function.name, &mut arguments, &mut varargs, children)?;
        function.arguments = arguments;
        function.varargs = varargs;
        let return_type = function.return_type.clone();
        Ok(Expression::new(
            ExpressionType::BoundAggregate,
            return_type,
            ExpressionKind::Aggregate(Box::new(BoundAggregateExpression {
                function,
                children,
                bind_info,
                aggr_type,
                filter,
                order_bys,
            })),
        ))
    }

    /// Resolves an aggregate overload (DuckDB's `BindFunction(AggregateFunctionSet)`).
    pub fn bind_aggregate_overload(
        &self,
        functions: &AggregateFunctionSet,
        arguments: &[LogicalType],
    ) -> Result<std::result::Result<AggregateFunction, Error>> {
        Ok(self
            .bind_function(&functions.name, &functions.functions, arguments)?
            .map(|index| functions.functions[index].clone()))
    }

    /// Resolves a table function overload (DuckDB's `BindFunction(TableFunctionSet)`).
    pub fn bind_table_function_overload(
        &self,
        functions: &TableFunctionSet,
        arguments: &[LogicalType],
    ) -> Result<std::result::Result<TableFunction, Error>> {
        Ok(self
            .bind_function(&functions.name, &functions.functions, arguments)?
            .map(|index| functions.functions[index].clone()))
    }
}

/// `ANY` arguments with a target type become that type (DuckDB's `PrepareTypeForCast`).
fn prepare_type_for_cast(ty: &mut LogicalType) {
    match ty.id() {
        LogicalTypeId::Any => {
            if let Some(TypeInfo::Any(info)) = ty.info()
                && info.target_type.id() != LogicalTypeId::Invalid
            {
                *ty = info.target_type.clone();
            }
        }
        LogicalTypeId::List => {
            if let Some(child) = ty.child_type() {
                let mut child = child.clone();
                let before = child.clone();
                prepare_type_for_cast(&mut child);
                if child != before {
                    *ty = LogicalType::list(child);
                }
            }
        }
        _ => {}
    }
}

/// Whether a child of type `source` needs a cast to `target` (DuckDB's `RequiresCast`).
fn requires_cast(source: &LogicalType, target: &LogicalType) -> bool {
    if target.id() == LogicalTypeId::Any || source == target {
        return false;
    }
    match (source.id(), target.id()) {
        (LogicalTypeId::List, LogicalTypeId::List) | (LogicalTypeId::Array, LogicalTypeId::Array) => {
            match (source.child_type(), target.child_type()) {
                (Some(s), Some(t)) => requires_cast(s, t),
                _ => true,
            }
        }
        _ => true,
    }
}

fn template_name(ty: &LogicalType) -> Option<&str> {
    match ty.info() {
        Some(TypeInfo::Template(info)) if ty.id() == LogicalTypeId::Template => Some(&info.name),
        _ => None,
    }
}

fn is_templated(ty: &LogicalType) -> bool {
    ty.id() == LogicalTypeId::Template || ty.child_type().is_some_and(is_templated)
}

/// Infers template bindings from an argument (DuckDB's `InferTemplateType`, for top-level
/// and list/array templates).
fn infer_template(
    source: &LogicalType,
    actual: &LogicalType,
    bindings: &mut CaseInsensitiveMap<LogicalType>,
) {
    if let Some(name) = template_name(source) {
        let new = match bindings.get(name) {
            None => actual.clone(),
            Some(existing) => {
                LogicalType::try_max_logical_type(existing, actual).unwrap_or_else(|| existing.clone())
            }
        };
        bindings.insert(name.to_owned(), new);
        return;
    }
    if matches!(source.id(), LogicalTypeId::List | LogicalTypeId::Array)
        && matches!(actual.id(), LogicalTypeId::List | LogicalTypeId::Array)
        && let (Some(source_child), Some(actual_child)) = (source.child_type(), actual.child_type())
    {
        infer_template(source_child, actual_child, bindings);
    }
}

fn substitute_template(ty: &LogicalType, bindings: &CaseInsensitiveMap<LogicalType>) -> LogicalType {
    if let Some(name) = template_name(ty) {
        return bindings.get(name).map_or_else(
            || ty.clone(),
            |bound| normalize_type(bound).unwrap_or_else(|_| bound.clone()),
        );
    }
    match ty.id() {
        LogicalTypeId::List => ty.child_type().map_or_else(
            || ty.clone(),
            |child| LogicalType::list(substitute_template(child, bindings)),
        ),
        LogicalTypeId::Array => match ty.array_child() {
            Some((child, size)) => LogicalType::array(substitute_template(child, bindings), size),
            None => ty.clone(),
        },
        _ => ty.clone(),
    }
}

/// DuckDB's `FunctionBinder::ResolveTemplateTypes`.
fn resolve_template_types(
    arguments: &mut [LogicalType],
    varargs: &mut Option<LogicalType>,
    return_type: &mut LogicalType,
    children: &[Expression],
) {
    let templated = arguments.iter().any(is_templated)
        || varargs.as_ref().is_some_and(is_templated)
        || is_templated(return_type);
    if !templated {
        return;
    }
    let mut bindings = CaseInsensitiveMap::default();
    for (argument, child) in arguments.iter().zip(children) {
        if is_templated(argument) {
            infer_template(argument, &expression_return_type(child), &mut bindings);
        }
    }
    if let Some(varargs) = varargs.as_ref().filter(|varargs| is_templated(varargs)) {
        for child in children.iter().skip(arguments.len()) {
            infer_template(varargs, &expression_return_type(child), &mut bindings);
        }
    }
    for argument in arguments.iter_mut() {
        *argument = substitute_template(argument, &bindings);
    }
    if let Some(varargs) = varargs.as_mut() {
        *varargs = substitute_template(varargs, &bindings);
    }
    *return_type = substitute_template(return_type, &bindings);
}

/// DuckDB's `FunctionBinder::CheckTemplateTypesResolved`.
fn check_template_types_resolved(
    name: &str,
    arguments: &[LogicalType],
    varargs: Option<&LogicalType>,
    return_type: &LogicalType,
) -> Result<()> {
    for ty in arguments
        .iter()
        .chain(varargs)
        .chain(std::iter::once(return_type))
    {
        if let Some(template) = unresolved_template(ty) {
            return Err(Error::binder(format!(
                "Function '{name}' has a template parameter type '{template}' that could not be resolved to a \
                 concrete type"
            )));
        }
    }
    Ok(())
}

fn unresolved_template(ty: &LogicalType) -> Option<String> {
    template_name(ty)
        .map(ToOwned::to_owned)
        .or_else(|| ty.child_type().and_then(unresolved_template))
}
