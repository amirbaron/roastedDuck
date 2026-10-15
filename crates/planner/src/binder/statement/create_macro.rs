//! `CREATE MACRO` (DuckDB's `Binder::BindCreateFunctionInfo`): default values are folded
//! to constants, parameter types are bound, and each overload's body is bound once with
//! its parameters as placeholders to report errors early.

use rustdb_ast::visit::{VisitorMut, walk_query_node};
use rustdb_ast::{
    CreateInfo, CreateInfoKind, ExpressionType, MacroFunctionKind, ParsedExpression, ParsedExpressionKind,
};
use rustdb_common::{Error, ErrorKind, Result};
use rustdb_types::{LogicalType, LogicalTypeId};

use super::super::expression::bind_logical_type;
use super::super::expression::bind_macro::parameter_name;
use super::super::query_node::{BoundSelectNode, bind_node};
use super::super::{BindScope, Binding, Scope};
use super::create::bind_create_schema;
use crate::expression_binder::{
    BoundGroupInformation, ConstantBinder, ExpressionBinder, SelectBinder, SelectRole, parsed_has_parameter,
    qualify_column_names,
};

/// The first serialization version that stores macro parameter types (v1.4.0).
const TYPED_MACRO_VERSION: u64 = 6;

/// Whether any expression of a query node has a prepared-statement parameter.
#[derive(Default)]
struct ParameterFinder {
    found: bool,
}

impl VisitorMut for ParameterFinder {
    fn visit_expression(&mut self, expression: &mut ParsedExpression) {
        self.found |= parsed_has_parameter(expression);
    }
}

/// Qualifies the top-level expressions of a query node (DuckDB's
/// `EnumerateQueryNodeChildren` with `QualifyColumnNames`).
struct NodeQualifier<'a> {
    binder: &'a super::super::Binder,
    error: Option<Error>,
}

impl VisitorMut for NodeQualifier<'_> {
    fn visit_expression(&mut self, expression: &mut ParsedExpression) {
        if let Err(error) = qualify_column_names(self.binder, expression) {
            self.error.get_or_insert(error);
        }
    }
}

/// Whether macros of `info` may store parameter types: always for temporary macros and
/// databases without a file, else from storage version v1.4.0.
fn can_store_types(scope: &BindScope<'_>, info: &CreateInfo) -> bool {
    if info.temporary {
        return true;
    }
    let catalog = if info.catalog.is_empty() {
        scope.context.default_catalog()
    } else {
        &info.catalog
    };
    scope
        .context
        .catalog(catalog)
        .and_then(|catalog| catalog.wal())
        .is_none_or(|wal| wal.batch().options().compatibility.serialization_version >= TYPED_MACRO_VERSION)
}

/// DuckDB's `Binder::BindCreateFunctionInfo`.
pub fn bind_create_function_info(scope: &mut BindScope<'_>, info: &mut CreateInfo) -> Result<()> {
    let store_types = can_store_types(scope, info);
    let (CreateInfoKind::Macro(base) | CreateInfoKind::TableMacro(base)) = &mut info.kind else {
        return Err(Error::internal("expected a macro"));
    };
    let name = base.name.clone();
    let mut type_overloads: Vec<Vec<LogicalType>> = Vec::new();
    for function in &mut base.macros {
        if !store_types && function.types.iter().any(|ty| ty.id() != LogicalTypeId::Unknown) {
            return Err(Error::binder(
                "Typed macro parameters are only supported for storage versions v1.4.0 and higher.\nUse an \
                 in-memory database, ATTACH with (STORAGE_VERSION v1.4.0), or create a TEMP macro",
            ));
        }
        let has_parameter = match &mut function.kind {
            MacroFunctionKind::Scalar(scalar) => parsed_has_parameter(&mut scalar.expression),
            MacroFunctionKind::Table(table) => {
                let mut finder = ParameterFinder::default();
                finder.visit_query_node(&mut table.query_node);
                finder.found
            }
        };
        if has_parameter {
            return Err(Error::binder(
                "Parameter expressions within macro's are not supported!",
            ));
        }

        // Constant-fold the default values.
        for (parameter, default) in function.default_parameters.iter_mut() {
            if default.expression_type == ExpressionType::ValueConstant {
                continue;
            }
            let mut copy = default.clone();
            let bound = {
                let (inner, outer) = scope.split();
                let mut binder =
                    ConstantBinder::new(inner, outer, format!("Default value for parameter '{parameter}'"));
                binder.bind(&mut copy, false)?
            };
            if !bound.is_foldable() {
                return Err(Error::binder(format!(
                    "Default value '{default}' for parameter '{parameter}' is not a constant expression."
                )));
            }
            let value = scope.context.evaluator.evaluate_scalar(&bound)?;
            *default = ParsedExpression::constant(value).with_alias(parameter);
        }

        // Resolve the parameter types.
        for (index, ty) in function.types.iter_mut().enumerate() {
            if ty.id() == LogicalTypeId::Unknown {
                continue;
            }
            if ty.id() == LogicalTypeId::Unbound {
                *ty = bind_logical_type(&scope.context, ty)?;
            }
            let parameter = function
                .parameters
                .get(index)
                .map(parameter_name)
                .unwrap_or_default();
            if let Some(default) = function.default_parameters.get(parameter)
                && let ParsedExpressionKind::Constant(constant) = &default.kind
                && scope
                    .context
                    .casts
                    .implicit_cast_cost(constant.value.logical_type(), ty)
                    < 0
            {
                return Err(Error::binder(format!(
                    "Default value '{default}' for parameter '{parameter}' cannot be implicitly cast to '{ty}'. \
                     Please add an explicit type cast."
                )));
            }
        }

        let dummy_types: Vec<LogicalType> = (0..function.parameters.len())
            .map(|index| function.types.get(index).cloned().unwrap_or(LogicalType::UNKNOWN))
            .collect();
        let dummy_names: Vec<String> = function
            .parameters
            .iter()
            .map(|parameter| parameter_name(parameter).to_owned())
            .collect();
        if type_overloads.contains(&dummy_types) {
            return Err(Error::binder(format!(
                "Ambiguity in macro overloads - macro {name}() has multiple definitions with the same parameters"
            )));
        }
        type_overloads.push(dummy_types.clone());

        // Bind the body once to report errors; unresolved parameter types postpone the
        // rest of the checks until the macro is called.
        let saved_binding =
            scope
                .binder
                .macro_binding
                .replace(Binding::dummy(dummy_types, dummy_names, &name)?);
        let result = verify_macro_body(scope, &function.kind);
        scope.binder.macro_binding = saved_binding;
        if let Err(error) = result
            && error.kind() != ErrorKind::ParameterNotResolved
        {
            return Err(error);
        }
    }
    bind_create_schema(scope, info)
}

/// Binds a copy of a macro body with the binder's macro binding in scope.
fn verify_macro_body(scope: &mut BindScope<'_>, body: &MacroFunctionKind) -> Result<()> {
    match body {
        MacroFunctionKind::Scalar(scalar) => {
            let mut expression = (*scalar.expression).clone();
            qualify_column_names(scope.binder, &mut expression)?;
            let mut node = BoundSelectNode::default();
            let info = BoundGroupInformation::default();
            let (inner, outer) = scope.split();
            let mut binder = SelectBinder::new(inner, outer, &mut node, &info, SelectRole::Select);
            match binder.bind_in_place(&mut expression, 0, false)? {
                Some(error) => Err(error.error),
                None => Ok(()),
            }
        }
        MacroFunctionKind::Table(table) => {
            let mut dummy_binder = scope.binder.child()?;
            let mut node = (*table.query_node).clone();
            let mut qualifier = NodeQualifier {
                binder: &dummy_binder,
                error: None,
            };
            walk_query_node(&mut qualifier, &mut node);
            if let Some(error) = qualifier.error {
                return Err(error);
            }
            let mut dummy_scope = BindScope {
                binder: &mut dummy_binder,
                outer: Some(scope as &mut dyn Scope),
            };
            bind_node(&mut dummy_scope, &mut node).map(|_| ())
        }
    }
}
