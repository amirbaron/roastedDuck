//! The transform functions of DuckDB's `transform_create_macro.cpp`: `CREATE MACRO` /
//! `CREATE FUNCTION`, scalar and table macros with their parameters.

use std::collections::HashSet;

use super::create_table::{create_info, on_create_conflict};
use super::prelude::*;

impl PegTransformer {
    /// `TransformCreateMacroStmt`: `MACRO | FUNCTION [IF NOT EXISTS] name
    /// definition, ...`; the overloads must all be scalar or all table macros.
    pub(super) fn transform_create_macro_stmt(&mut self, parse_result: &ParseResult) -> Result<CreateInfo> {
        let list = parse_result.list()?;
        let if_not_exists = list.has(1)?;
        let qualified_name: QualifiedName = self.transform_child(list, 2)?;
        let (catalog, schema) = if qualified_name.schema.is_empty() {
            (String::new(), qualified_name.catalog)
        } else {
            (qualified_name.catalog, qualified_name.schema)
        };
        let macro_definition_list = list.child(3)?.list_elements()?;
        let mut macros = Vec::with_capacity(macro_definition_list.len());
        for macro_definition in &macro_definition_list {
            macros.push(self.transform::<MacroFunction>(macro_definition)?);
        }
        let macro_type = macros
            .first()
            .ok_or_else(|| Error::internal("CREATE MACRO without a definition"))?
            .kind
            .macro_type();
        if macros.len() > 1
            && macros
                .iter()
                .take(macro_definition_list.len())
                .skip(1)
                .any(|function| function.kind.macro_type() != macro_type)
        {
            return Err(Error::parser(
                "Cannot mix table and scalar macro function definitions",
            ));
        }
        let macro_info = CreateMacroInfo {
            name: qualified_name.name,
            macros,
        };
        let kind = if macro_type == MacroType::TableMacro {
            CreateInfoKind::TableMacro(macro_info)
        } else {
            CreateInfoKind::Macro(macro_info)
        };
        let mut info = create_info(&schema, kind);
        info.catalog = catalog;
        info.on_conflict = on_create_conflict(if_not_exists);
        self.pivot_entry_check("macro")?;
        Ok(info)
    }

    /// `TransformMacroDefinition`: `(parameters) AS body`; parameters with defaults
    /// must come last.
    pub(super) fn transform_macro_definition(&mut self, parse_result: &ParseResult) -> Result<MacroFunction> {
        let list = parse_result.list()?;
        let nested_list = list.list(2)?;
        let mut macro_function: MacroFunction = self.transform(nested_list.choice(0)?)?;
        let Some(parameters_pr) = list.child(0)?.parens_content()?.optional()? else {
            return Ok(macro_function);
        };
        let mut default_value_found = false;
        let parameters: Vec<MacroParameter> = self.transform(parameters_pr)?;
        // A `case_insensitive_string_set_t`.
        let mut parameter_names: HashSet<String> = HashSet::new();
        for parameter in parameters {
            if !parameter_names.insert(parameter.name.to_ascii_lowercase()) {
                return Err(Error::parser(format!(
                    "Duplicate parameter '{}' in macro definition",
                    parameter.name
                )));
            }
            let expression = parameter
                .expression
                .ok_or_else(|| Error::internal("Macro parameter without an expression"))?;
            if parameter.is_default {
                let default_expr = expression.with_alias(parameter.name.clone());
                macro_function
                    .default_parameters
                    .insert(parameter.name.clone(), default_expr);
                macro_function
                    .parameters
                    .push(ParsedExpression::column_ref([parameter.name]));
                default_value_found = true;
            } else {
                if default_value_found {
                    return Err(Error::parser(
                        "Parameter without a default follows parameter with a default",
                    ));
                }
                macro_function.parameters.push(expression);
            }
            macro_function.types.push(parameter.ty);
        }
        Ok(macro_function)
    }

    /// `TransformTableMacroDefinition`: `TABLE query`.
    pub(super) fn transform_table_macro_definition(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<MacroFunction> {
        let select_statement: SelectStatement = self.transform_child(parse_result.list()?, 1)?;
        Ok(MacroFunction {
            parameters: Vec::new(),
            default_parameters: CaseInsensitiveMap::default(),
            types: Vec::new(),
            kind: MacroFunctionKind::Table(TableMacroFunction {
                query_node: select_statement.node,
            }),
        })
    }

    /// `TransformScalarMacroDefinition`: an expression.
    pub(super) fn transform_scalar_macro_definition(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<MacroFunction> {
        let expression: ParsedExpression = self.transform_child(parse_result.list()?, 0)?;
        Ok(MacroFunction {
            parameters: Vec::new(),
            default_parameters: CaseInsensitiveMap::default(),
            types: Vec::new(),
            kind: MacroFunctionKind::Scalar(ScalarMacroFunction {
                expression: Box::new(expression),
            }),
        })
    }

    /// `TransformMacroParameters`: `parameter, ...`.
    pub(super) fn transform_macro_parameters(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<MacroParameter>> {
        parse_result
            .list()?
            .child(0)?
            .list_elements()?
            .into_iter()
            .map(|parameter| self.transform(parameter))
            .collect()
    }

    /// `TransformMacroParameter`: a named parameter (with a default) or a simple one.
    pub(super) fn transform_macro_parameter(&mut self, parse_result: &ParseResult) -> Result<MacroParameter> {
        self.transform(parse_result.list()?.choice(0)?)
    }

    /// `TransformSimpleParameter`: `name [type]`.
    pub(super) fn transform_simple_parameter(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<MacroParameter> {
        let list = parse_result.list()?;
        let parameter: String = self.transform_child(list, 0)?;
        let mut result = MacroParameter {
            expression: Some(ParsedExpression::column_ref([parameter.clone()])),
            name: parameter,
            ..MacroParameter::default()
        };
        if let Some(ty) = self.transform_optional(list, 1)? {
            result.ty = ty;
        }
        result.is_default = false;
        Ok(result)
    }
}
