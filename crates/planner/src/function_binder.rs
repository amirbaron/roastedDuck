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
    ScalarFunctionSet, TableFunction, TableFunctionSet, named_parameters_in_map_order,
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

/// A function's rewrite of its bound call into another expression (see
/// `ScalarFunction::rewrite_expression`, which holds it type-erased).
pub struct RewriteExpression(
    #[allow(clippy::type_complexity, reason = "a callback signature")]
    pub Box<dyn Fn(&ClientContext, Expression) -> Result<Expression> + Send + Sync>,
);

impl std::fmt::Debug for RewriteExpression {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RewriteExpression")
    }
}

impl RewriteExpression {
    /// The callback as the function's `rewrite_expression`.
    pub fn erased(
        rewrite: impl Fn(&ClientContext, Expression) -> Result<Expression> + Send + Sync + 'static,
    ) -> Arc<dyn std::any::Any + Send + Sync> {
        Arc::new(Self(Box::new(rewrite)))
    }
}

/// The common type of two types, or a Not implemented error (DuckDB's `MaxLogicalType`).
pub fn max_logical_type(context: &ClientContext, left: &LogicalType, right: &LogicalType) -> Result<LogicalType> {
    context.try_max_logical_type(left, right).ok_or_else(|| {
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
    context: &ClientContext,
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
        match context.try_max_logical_type(left, right) {
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

/// Whether `ty` takes part in collation propagation: a VARCHAR without an alias
/// (`RequiresCollationPropagation`).
fn requires_collation_propagation(ty: &LogicalType) -> bool {
    ty.id() == LogicalTypeId::Varchar && ty.alias().is_none()
}

/// The collation of a VARCHAR type, empty without one (`StringType::GetCollation`).
fn collation_of(ty: &LogicalType) -> &str {
    collation(ty).unwrap_or("")
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
            named_parameters_in_map_order(&self.named_parameters).map(|(name, ty)| format!("{name} : {ty}")),
        );
        format!("{}({})", self.name, parts.join(", "))
    }
}

impl FunctionSignature for rustdb_function::PragmaFunction {
    fn name(&self) -> &str {
        &self.name
    }

    fn arguments(&self) -> &[LogicalType] {
        &self.arguments
    }

    fn varargs(&self) -> Option<&LogicalType> {
        self.varargs.as_ref()
    }

    /// `PragmaFunction::ToString`.
    fn signature(&self) -> String {
        match self.pragma_type {
            rustdb_function::PragmaType::Statement => format!("PRAGMA {}", self.name),
            rustdb_function::PragmaType::Call => {
                let mut parts: Vec<String> = self.arguments.iter().map(ToString::to_string).collect();
                parts.extend(
                    named_parameters_in_map_order(&self.named_parameters)
                        .map(|(name, ty)| format!("{name} : {ty}")),
                );
                format!("PRAGMA {}({})", self.name, parts.join(", "))
            }
        }
    }
}

/// Binds calls against function sets (DuckDB's `FunctionBinder`).
#[derive(Debug)]
pub struct FunctionBinder<'a> {
    context: &'a ClientContext,
    /// The lambda argument of the call being bound, for lambda functions.
    lambda: Option<rustdb_function::LambdaInput>,
}

impl<'a> FunctionBinder<'a> {
    pub fn new(context: &'a ClientContext) -> Self {
        Self {
            context,
            lambda: None,
        }
    }

    /// A binder for a lambda function call whose lambda argument is `lambda`.
    #[must_use]
    pub fn with_lambda(mut self, lambda: rustdb_function::LambdaInput) -> Self {
        self.lambda = Some(lambda);
        self
    }

    fn implicit_cast_cost(&self, from: &LogicalType, to: &LogicalType) -> i64 {
        let cost = self.context.casts.implicit_cast_cost(from, to);
        if cost < 0
            && self.context.config.old_implicit_casting
            && from.id() != LogicalTypeId::Blob
            && to.id() == LogicalTypeId::Varchar
        {
            // Costlier than any other candidate, including binding a template.
            return 10_000_000_000;
        }
        cost
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
            let signatures: Vec<String> = functions.iter().map(FunctionSignature::signature).collect();
            let list = signatures.iter().fold(String::new(), |mut list, signature| {
                let _ = writeln!(list, "\t{signature}");
                list
            });
            // `BinderException::NoMatchingFunction`, caught into the binder's `ErrorData`;
            // every function set lives in the system catalog's main schema.
            return Ok(Err(Error::binder(format!(
                "No function matches the given name and argument types '{call}'. You might need to add \
                 explicit type casts.\n\tCandidate functions:\n{list}"
            ))
            .with_subtype("NO_MATCHING_FUNCTION")
            .with_extra_info("name", name)
            .with_extra_info("catalog", "system")
            .with_extra_info("schema", "main")
            .with_extra_info("call", call)
            .with_extra_info("candidates", signatures.join(","))
            .with_extra_info_round_trip()));
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
            // DuckDB's `IsComplete`: templates count as incomplete too.
            let null_type = if function.return_type.is_valid() && !is_templated(&function.return_type) {
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

    /// DuckDB's `HandleCollations`: a VARCHAR result inherits the arguments' collation,
    /// and functions such as `like` apply the combinable collations to their arguments.
    fn handle_collations(
        &self,
        function: &mut ScalarFunction,
        children: Vec<Expression>,
    ) -> Result<Vec<Expression>> {
        use rustdb_function::CollationHandling;
        if function.collation_handling == CollationHandling::IgnoreCollations {
            return Ok(children);
        }
        // `ExtractCollation`.
        let mut collation = String::new();
        for child in &children {
            if !requires_collation_propagation(&child.return_type) {
                continue;
            }
            let child_collation = collation_of(&child.return_type);
            if collation.is_empty() {
                child_collation.clone_into(&mut collation);
            } else if !child_collation.is_empty() && collation != child_collation {
                return Err(Error::binder("Cannot combine types with different collation!"));
            }
        }
        if collation.is_empty() {
            return Ok(children);
        }
        let collation_type = LogicalType::with_info(
            LogicalTypeId::Varchar,
            rustdb_types::TypeInfo::String(rustdb_types::StringTypeInfo { collation }),
        );
        if requires_collation_propagation(&function.return_type) {
            function.return_type = collation_type.clone();
        }
        if function.collation_handling == CollationHandling::PropagateCollations {
            return Ok(children);
        }
        children
            .into_iter()
            .map(|mut child| {
                if requires_collation_propagation(&child.return_type) {
                    child.return_type = collation_type.clone();
                }
                let ty = child.return_type.clone();
                crate::collation_binding::push_collation_with(
                    self.context,
                    child,
                    &ty,
                    crate::collation_binding::CollationType::CombinableCollations,
                )
                .map(|(child, _)| child)
            })
            .collect()
    }

    /// Binds a call to one chosen overload (DuckDB's `BindScalarFunction(ScalarFunction, ...)`).
    pub fn bind_scalar_overload(
        &self,
        mut function: ScalarFunction,
        mut children: Vec<Expression>,
        is_operator: bool,
    ) -> Result<Expression> {
        resolve_template_types(
            &function.name,
            &mut function.arguments,
            &mut function.varargs,
            &mut function.return_type,
            &children,
            self.context.config.old_implicit_casting,
        )?;
        let mut bind_info: Option<Arc<dyn FunctionData>> = None;
        if let Some(bind) = function.bind.clone() {
            let (argument_types, constants) = self.bind_input(&children);
            let aliases: Vec<String> = children.iter().map(|child| child.alias.clone()).collect();
            let names: Vec<String> = children.iter().map(expression_name).collect();
            bind_info = bind(
                &mut function,
                &BindInput {
                    argument_types: &argument_types,
                    constants: &constants,
                    aliases: &aliases,
                    names: &names,
                    client: &self.context.function_client,
                    binder: Some(self.context as &dyn std::any::Any),
                    arguments: Some(&children as &dyn std::any::Any),
                    aggregates: Some(self.context as &dyn rustdb_function::AggregateResolver),
                    lambda: self.lambda.as_ref(),
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
        let argument_types: Vec<LogicalType> =
            children.iter().map(|child| child.return_type.clone()).collect();
        if let Some(bind_expression) = &function.bind_expression
            && let Some(value) = bind_expression(bind_info.as_deref(), &argument_types)
        {
            return Ok(Expression::constant(value));
        }
        let children = self.handle_collations(&mut function, children)?;
        let mut arguments = std::mem::take(&mut function.arguments);
        let mut varargs = function.varargs.take();
        let children = self.cast_to_arguments(&function.name, &mut arguments, &mut varargs, children)?;
        function.arguments = arguments;
        function.varargs = varargs;
        let rewrite = function.rewrite_expression.clone();
        let bound = Expression::function(function, children, bind_info, is_operator);
        match rewrite
            .as_deref()
            .and_then(|rewrite| rewrite.downcast_ref::<RewriteExpression>())
        {
            Some(rewrite) => (rewrite.0)(self.context, bound),
            None => Ok(bound),
        }
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
            &function.name,
            &mut function.arguments,
            &mut function.varargs,
            &mut function.return_type,
            &children,
            self.context.config.old_implicit_casting,
        )?;
        let mut bind_info = None;
        let mut children = children;
        self.push_aggregate_collations(&mut function, &mut children)?;
        if let Some(bind) = function.bind.clone() {
            let (argument_types, constants) = self.bind_input(&children);
            let aliases: Vec<String> = children.iter().map(|child| child.alias.clone()).collect();
            let names: Vec<String> = children.iter().map(expression_name).collect();
            bind_info = bind(
                &mut function,
                &BindInput {
                    argument_types: &argument_types,
                    constants: &constants,
                    aliases: &aliases,
                    names: &names,
                    client: &self.context.function_client,
                    binder: Some(self.context as &dyn std::any::Any),
                    arguments: Some(&children as &dyn std::any::Any),
                    aggregates: Some(self.context as &dyn rustdb_function::AggregateResolver),
                    lambda: self.lambda.as_ref(),
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

    /// The collation handling of DuckDB's aggregate binds (`BindMinMax` and
    /// `ArgMinMaxBase::Bind`): see [`rustdb_function::AggregateCollation`].
    fn push_aggregate_collations(
        &self,
        function: &mut AggregateFunction,
        children: &mut Vec<Expression>,
    ) -> Result<()> {
        use crate::collation_binding::{CollationType, push_collation_with};
        match function.collation {
            rustdb_function::AggregateCollation::None => {}
            rustdb_function::AggregateCollation::Argument(index) => {
                if let Some(child) = children.get_mut(index)
                    && child.return_type.physical_type() == rustdb_types::PhysicalType::Varchar
                {
                    let ty = child.return_type.clone();
                    let original =
                        std::mem::replace(child, Expression::constant(Value::null(LogicalType::SQLNULL)));
                    *child =
                        push_collation_with(self.context, original, &ty, CollationType::AllCollations)?.0;
                }
            }
            rustdb_function::AggregateCollation::MinMax => {
                let Some(first) = children.first() else {
                    return Ok(());
                };
                // Only VARCHAR is collated here (DuckDB's internal #8704).
                if first.return_type.id() != LogicalTypeId::Varchar
                    || (collation_of(&first.return_type).is_empty()
                        && self.context.config.default_collation.is_empty())
                {
                    return Ok(());
                }
                let ty = first.return_type.clone();
                let (collated, pushed) =
                    push_collation_with(self.context, first.clone(), &ty, CollationType::AllCollations)?;
                if !pushed {
                    return Ok(());
                }
                let name = if &*function.name == "min" {
                    "arg_min"
                } else {
                    "arg_max"
                };
                let lookup = self.context.get_entry(
                    "system",
                    "main",
                    rustdb_catalog::CatalogType::AggregateFunction,
                    name,
                    true,
                )?;
                let Some(rustdb_catalog::EntryKind::AggregateFunction(functions)) =
                    lookup.as_ref().map(|lookup| &lookup.entry.kind)
                else {
                    return Err(Error::not_implemented(format!(
                        "Failure while binding function \"{}\" using collations - arg_min/arg_max do not exist in \
                         the catalog - load the core_functions module to fix this issue",
                        function.name
                    )));
                };
                let types = [ty.clone(), collated.return_type.clone()];
                let mut replacement = self
                    .bind_aggregate_overload(functions, &types)?
                    .map_err(|error| {
                        Error::binder(format!(
                            "Fail to find corresponding function for collation min/max: {}",
                            error.raw_message()
                        ))
                    })?;
                children.push(collated);
                replacement.arguments[0] = ty.clone();
                replacement.return_type = ty;
                *function = replacement;
            }
        }
        Ok(())
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
    ty.id() == LogicalTypeId::Template
        || ty.child_type().is_some_and(is_templated)
        || ty.array_child().is_some_and(|(child, _)| is_templated(child))
        || ty
            .struct_children()
            .is_some_and(|children| children.iter().any(|(_, child)| is_templated(child)))
}

/// The inference steps of each template (DuckDB's `bindings`): the first type, then
/// pairs of (argument type, promoted type).
type TemplateBindings = CaseInsensitiveMap<Vec<LogicalType>>;

/// What a failed inference reports: the function and the argument being inferred.
struct TemplateContext<'a> {
    signature: &'a str,
    location: Option<u64>,
    /// `old_implicit_casting`: bindings of any two types combine.
    old_implicit_casting: bool,
}

/// Binds every unbound template inside `source` to `actual` (DuckDB's handling of
/// UNKNOWN/SQLNULL arguments in `InferTemplateType`).
fn bind_remaining_templates(source: &LogicalType, actual: &LogicalType, bindings: &mut TemplateBindings) {
    if let Some(name) = template_name(source) {
        if bindings.get(name).is_none() {
            bindings.insert(name.to_owned(), vec![LogicalType::simple(actual.id())]);
        }
        return;
    }
    if let Some(child) = source.child_type() {
        bind_remaining_templates(child, actual, bindings);
    }
    if let Some((child, _)) = source.array_child() {
        bind_remaining_templates(child, actual, bindings);
    }
    if let Some(children) = source.struct_children() {
        for (_, child) in children {
            bind_remaining_templates(child, actual, bindings);
        }
    }
}

/// Infers template bindings from an argument (DuckDB's `InferTemplateType`): a template
/// takes the argument's type, promoted with earlier bindings; incompatible types fail.
fn infer_template(
    source: &LogicalType,
    actual: &LogicalType,
    bindings: &mut TemplateBindings,
    context: &TemplateContext<'_>,
) -> Result<()> {
    if matches!(actual.id(), LogicalTypeId::Unknown | LogicalTypeId::SqlNull) {
        bind_remaining_templates(source, actual, bindings);
        return Ok(());
    }
    if let Some(name) = template_name(source) {
        let Some(steps) = bindings.get_mut(name) else {
            bindings.insert(name.to_owned(), vec![actual.clone()]);
            return Ok(());
        };
        let Some(last) = steps.last().cloned() else {
            return Ok(());
        };
        if &last == actual {
            return Ok(());
        }
        let promoted = if context.old_implicit_casting {
            Some(LogicalType::force_max_logical_type(&last, actual))
        } else {
            LogicalType::try_max_logical_type(&last, actual)
        };
        if let Some(promoted) = promoted {
            if promoted != last {
                steps.push(actual.clone());
                steps.push(promoted);
            }
            return Ok(());
        }
        let mut message = format!(
            "Cannot deduce template type '{name}' in function: '{}'\nType '{name}' was inferred to be:\n",
            context.signature
        );
        for index in (0..steps.len()).step_by(2) {
            if index == 0 {
                let _ = writeln!(message, " - '{}', from first occurrence", steps[index]);
            } else {
                let _ = writeln!(
                    message,
                    " - '{}', by promoting '{}' + '{}'",
                    steps[index],
                    steps[index - 2],
                    steps[index - 1]
                );
            }
        }
        let _ = write!(
            message,
            " - '{actual}', which is incompatible with previously inferred type!"
        );
        return Err(Error::binder(message).with_position(context.location));
    }
    if matches!(source.id(), LogicalTypeId::List | LogicalTypeId::Array)
        && matches!(actual.id(), LogicalTypeId::List | LogicalTypeId::Array)
        && let (Some(source_child), Some(actual_child)) = (source.child_type(), actual.child_type())
    {
        return infer_template(source_child, actual_child, bindings, context);
    }
    if source.id() == LogicalTypeId::Map && actual.id() == LogicalTypeId::Map {
        if let (Some((source_key, source_value)), Some((actual_key, actual_value))) =
            (source.map_key_value(), actual.map_key_value())
        {
            infer_template(source_key, actual_key, bindings, context)?;
            infer_template(source_value, actual_value, bindings, context)?;
        }
        return Ok(());
    }
    if source.id() == LogicalTypeId::Struct
        && actual.id() == LogicalTypeId::Struct
        && let (Some(source_children), Some(actual_children)) =
            (source.struct_children(), actual.struct_children())
        && source_children.iter().all(|(name, _)| name.is_empty())
    {
        for ((_, source_child), (_, actual_child)) in source_children.iter().zip(actual_children) {
            infer_template(source_child, actual_child, bindings, context)?;
        }
    }
    Ok(())
}

fn substitute_template(ty: &LogicalType, bindings: &TemplateBindings) -> LogicalType {
    if let Some(name) = template_name(ty) {
        return bindings.get(name).and_then(|steps| steps.last()).map_or_else(
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
        LogicalTypeId::Map => match ty.map_key_value() {
            Some((key, value)) => LogicalType::map(
                substitute_template(key, bindings),
                substitute_template(value, bindings),
            ),
            None => ty.clone(),
        },
        LogicalTypeId::Struct => match ty.struct_children() {
            Some(children) => LogicalType::struct_type(
                children
                    .iter()
                    .map(|(name, child)| (name.clone(), substitute_template(child, bindings)))
                    .collect(),
            ),
            None => ty.clone(),
        },
        _ => ty.clone(),
    }
}

/// DuckDB's `FunctionBinder::ResolveTemplateTypes`.
fn resolve_template_types(
    name: &str,
    arguments: &mut [LogicalType],
    varargs: &mut Option<LogicalType>,
    return_type: &mut LogicalType,
    children: &[Expression],
    old_implicit_casting: bool,
) -> Result<()> {
    let templated = arguments.iter().any(is_templated)
        || varargs.as_ref().is_some_and(is_templated)
        || is_templated(return_type);
    if !templated {
        return Ok(());
    }
    let signature = format!(
        "{} -> {return_type}",
        call_to_string(name, arguments, varargs.as_ref())
    );
    let mut bindings = TemplateBindings::default();
    for (argument, child) in arguments.iter().zip(children) {
        if is_templated(argument) {
            let context = TemplateContext {
                signature: &signature,
                location: child.query_location,
                old_implicit_casting,
            };
            infer_template(argument, &expression_return_type(child), &mut bindings, &context)?;
        }
    }
    if let Some(varargs) = varargs.as_ref().filter(|varargs| is_templated(varargs)) {
        for child in children.iter().skip(arguments.len()) {
            let context = TemplateContext {
                signature: &signature,
                location: child.query_location,
                old_implicit_casting,
            };
            infer_template(varargs, &expression_return_type(child), &mut bindings, &context)?;
        }
    }
    for argument in arguments.iter_mut() {
        *argument = substitute_template(argument, &bindings);
    }
    if let Some(varargs) = varargs.as_mut() {
        *varargs = substitute_template(varargs, &bindings);
    }
    *return_type = substitute_template(return_type, &bindings);
    Ok(())
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
        .or_else(|| ty.array_child().and_then(|(child, _)| unresolved_template(child)))
        .or_else(|| {
            ty.struct_children()
                .and_then(|children| children.iter().find_map(|(_, child)| unresolved_template(child)))
        })
}

/// DuckDB's `Expression::GetName`: the alias, or the expression's SQL text.
fn expression_name(expression: &Expression) -> String {
    if expression.alias.is_empty() {
        expression.to_string()
    } else {
        expression.alias.clone()
    }
}

/// Aggregate resolution for scalar functions that run an aggregate (`list_aggregate`):
/// DuckDB's `Catalog::GetSystemCatalog(context).GetEntry<AggregateFunctionCatalogEntry>`,
/// `FunctionBinder::BindFunction` and `FunctionBinder::BindAggregateFunction`.
impl rustdb_function::AggregateResolver for ClientContext {
    fn bind_aggregate(
        &self,
        name: &str,
        arguments: &[(LogicalType, Option<Value>)],
    ) -> Result<rustdb_function::BoundAggregateFunction> {
        let lookup = self
            .get_entry(
                "system",
                "main",
                rustdb_catalog::CatalogType::AggregateFunction,
                name,
                false,
            )?
            .ok_or_else(|| Error::internal("aggregate lookup returned nothing"))?;
        let rustdb_catalog::EntryKind::AggregateFunction(functions) = &lookup.entry.kind else {
            return Err(Error::catalog(format!("{name} is not an aggregate function")));
        };
        let binder = FunctionBinder::new(self);
        let types: Vec<LogicalType> = arguments.iter().map(|(ty, _)| ty.clone()).collect();
        let function = binder
            .bind_aggregate_overload(functions, &types)?
            .map_err(|error| Error::binder(format!("No matching aggregate function\n{error}")))?;
        // Constants stay constants; other arguments are references, which binds see as
        // non-constant (`ListAggregatesBind` passes the original expressions).
        let children = arguments
            .iter()
            .enumerate()
            .map(|(index, (ty, value))| match value {
                Some(value) => Expression::constant(value.clone()),
                None => Expression::reference("", ty.clone(), index as u64),
            })
            .collect();
        let bound =
            binder.bind_aggregate_function(function, children, None, AggregateType::NonDistinct, None)?;
        let display = bound.to_string();
        let ExpressionKind::Aggregate(aggregate) = bound.kind else {
            return Err(Error::internal("aggregate binding did not produce an aggregate"));
        };
        let aggregate = *aggregate;
        Ok(rustdb_function::BoundAggregateFunction {
            argument_count: aggregate.children.len(),
            display,
            function: aggregate.function,
            bind_data: aggregate.bind_info,
        })
    }
}
