//! The transform functions of DuckDB's `transform_use.cpp`.

use roastedduck_ast::keyword::write_optionally_quoted_bytes;

use super::prelude::*;

impl PegTransformer {
    /// `TransformUseStatement`: `USE UseTarget` is `SET schema = 'db[.schema]'`.
    pub(super) fn transform_use_statement(&mut self, parse_result: &ParseResult) -> Result<SqlStatement> {
        let qn: QualifiedName = self.transform_child(parse_result.list()?, 1)?;
        let value_str = if qn.schema.is_empty() {
            write_optionally_quoted_bytes(&qn.name)
        } else {
            byte_format!(
                "{}.{}",
                write_optionally_quoted_bytes(&qn.schema),
                write_optionally_quoted_bytes(&qn.name)
            )
        };
        Ok(SqlStatement::new(SqlStatementKind::Set(SetStatement {
            name: "schema".to_owned().into(),
            scope: SetScope::Automatic,
            value: Some(Box::new(ParsedExpression::constant(Value::try_varchar(
                &value_str,
            )?))),
        })))
    }

    /// `TransformUseTarget`: `UseTargetCatalogSchema / SchemaName / CatalogName`.
    pub(super) fn transform_use_target(&mut self, parse_result: &ParseResult) -> Result<QualifiedName> {
        let choice = parse_result.list()?.choice(0)?;
        if choice.result_type() == ParseResultType::Identifier {
            return Ok(QualifiedName {
                name: choice.identifier()?.to_owned().into(),
                ..QualifiedName::default()
            });
        }
        self.transform(choice)
    }

    /// `TransformUseTargetCatalogSchema`: `CatalogName '.' ReservedSchemaName ('.'
    /// Identifier)*`; the database and schema are the schema and name.
    pub(super) fn transform_use_target_catalog_schema(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<QualifiedName> {
        let list = parse_result.list()?;
        let catalog = list.identifier(0)?.to_owned();
        let schema = list.identifier(2)?.to_owned();
        if list.has(3)? {
            return Err(Error::parser(
                "Expected \"USE database\" or \"USE database.schema\"",
            ));
        }
        Ok(QualifiedName {
            catalog: String::new().into(),
            schema: catalog.into(),
            name: schema.into(),
        })
    }
}
