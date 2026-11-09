//! The transform functions of DuckDB's `transform_execute.cpp`.

use roastedduck_parser::hash_order::CiHashMap;
use roastedduck_parser::is_scalar;

use super::prelude::*;

impl PegTransformer {
    /// `TransformExecuteStatement`: `EXECUTE name TableFunctionArguments?`; the
    /// arguments are named `1`, `2`, ... unless they are named parameters.
    pub(super) fn transform_execute_statement(&mut self, parse_result: &ParseResult) -> Result<SqlStatement> {
        let list = parse_result.list()?;
        let name = list.identifier(1)?.to_owned();
        let mut named_values: CiHashMap<ParsedExpression, ByteString> = CiHashMap::default();
        if let Some(table_function) = list.optional(2)? {
            let mut param_idx = 0;
            let table_function_arguments: Vec<ParsedExpression> = self.transform(table_function)?;
            for (i, mut expr) in table_function_arguments.into_iter().enumerate() {
                if !is_scalar(&expr) {
                    return Err(Error::invalid_input(
                        "Only scalar parameters, named parameters or NULL supported for EXECUTE",
                    ));
                }
                if !expr.alias.is_empty() && param_idx != 0 {
                    return Err(Error::not_implemented(
                        "Mixing named parameters and positional parameters is not supported yet",
                    ));
                }
                let mut param_name = expr.alias.clone();
                if expr.alias.is_empty() {
                    param_name = (param_idx + 1).to_string().into();
                    if param_idx != i {
                        return Err(Error::not_implemented(
                            "Mixing named parameters and positional parameters is not supported yet",
                        ));
                    }
                    param_idx += 1;
                }
                expr.alias.clear();
                named_values.set(param_name, expr);
            }
        }
        Ok(SqlStatement::new(SqlStatementKind::Execute(ExecuteStatement {
            name: name.into(),
            named_values: named_values.into_ordered().into_iter().collect(),
        })))
    }
}
