//! The transform functions of DuckDB's `transform_create_secret.cpp`: `CREATE SECRET
//! [name] [IN storage] (option value, ...)`.

use rustdb_parser::hash_order::CiHashMap;

use super::create_table::{create_info, on_create_conflict};
use super::prelude::*;

/// `DEFAULT_SCHEMA`, the schema of a `CreateSecretInfo`.
const DEFAULT_SCHEMA: &str = "main";

/// `PEGTransformerFactory::GetConstantExpressionValue`: a constant's value, a column's
/// name, or NULL.
fn constant_expression_value(expr: &ParsedExpression) -> Value {
    match &expr.kind {
        ParsedExpressionKind::Constant(constant) if expr.expression_type == ExpressionType::ValueConstant => {
            constant.value.clone()
        }
        ParsedExpressionKind::ColumnRef(column_ref) if expr.expression_type == ExpressionType::ColumnRef => {
            // `ColumnRefExpression::GetName`.
            let name = if expr.alias.is_empty() {
                column_ref.column_names.last().cloned().unwrap_or_default()
            } else {
                expr.alias.clone()
            };
            Value::varchar(name)
        }
        _ => Value::default(),
    }
}

impl PegTransformer {
    /// `TransformCreateSecretStmt`: the `SCOPE`, `TYPE` and `PROVIDER` options are
    /// members, the others go to `options` (a `case_insensitive_map_t`) by lower-cased
    /// name. A secret without a name is named after its type.
    pub(super) fn transform_create_secret_stmt(&mut self, parse_result: &ParseResult) -> Result<CreateInfo> {
        let list = parse_result.list()?;
        let if_not_exists = list.has(1)?;
        let mut info = CreateSecretInfo {
            persist_type: SecretPersistType::Default,
            storage_type: String::new(),
            name: String::new(),
            scope: None,
            secret_type: None,
            provider: None,
            options: CaseInsensitiveMap::default(),
        };
        if let Some(secret_name) = list.optional(2)? {
            info.name = self.transform(secret_name)?;
        }
        if let Some(secret_storage_specifier) = list.optional(3)? {
            let storage_type: String = self.transform(secret_storage_specifier)?;
            info.storage_type = storage_type.to_ascii_lowercase();
        }
        let option_list: Vec<GenericCopyOption> = self.transform_child(list, 4)?;
        let mut options: CiHashMap<ParsedExpression> = CiHashMap::new();
        for option in option_list {
            let lower_name = option.name.to_ascii_lowercase();
            match lower_name.as_str() {
                "scope" => info.scope = Some(Box::new(option.first_child_or_expression()?)),
                "type" => info.secret_type = Some(Box::new(option.first_child_or_expression()?)),
                "provider" => info.provider = Some(Box::new(option.first_child_or_expression()?)),
                _ => {
                    if options.contains_key(&lower_name) {
                        return Err(Error::binder(format!(
                            "Duplicate query param found while parsing create secret: '{lower_name}'"
                        )));
                    }
                    let value = option.first_child_or_expression()?;
                    options.insert(lower_name, value);
                }
            }
        }
        info.options = options.into_ordered().into_iter().collect();
        if info.name.is_empty() {
            // DuckDB dereferences the type expression here, even when there is none.
            let secret_type = info
                .secret_type
                .as_deref()
                .ok_or_else(|| Error::internal("CREATE SECRET without a name or a TYPE"))?;
            let value = constant_expression_value(secret_type);
            if value.is_null() {
                return Err(Error::invalid_input(
                    "Can not combine a non-constant expression for the secret type with a default-named secret. \
                     Either provide an explicit secret name or use a constant expression for the secret type.",
                ));
            }
            info.name = format!("__default_{}", value.to_varchar()?.to_ascii_lowercase());
        }
        let mut result = create_info(DEFAULT_SCHEMA, CreateInfoKind::Secret(Box::new(info)));
        result.on_conflict = on_create_conflict(if_not_exists);
        Ok(result)
    }

    /// `TransformSecretName`.
    pub(super) fn transform_secret_name(&mut self, parse_result: &ParseResult) -> Result<String> {
        self.transform_child(parse_result.list()?, 0)
    }

    /// `TransformSecretStorageSpecifier`: `IN storage`.
    pub(super) fn transform_secret_storage_specifier(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<String> {
        Ok(parse_result.list()?.identifier(1)?.to_owned())
    }
}
