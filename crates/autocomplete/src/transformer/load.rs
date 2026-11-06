//! The transform functions of DuckDB's `transform_load.cpp`: `LOAD`, `[FORCE] INSTALL`
//! and `UPDATE EXTENSIONS`.

use super::prelude::*;

impl PegTransformer {
    /// `TransformLoadStatement`: `LOAD ColIdOrString`.
    pub(super) fn transform_load_statement(&mut self, parse_result: &ParseResult) -> Result<SqlStatement> {
        let filename = self.transform_child(parse_result.list()?, 1)?;
        Ok(SqlStatement::new(SqlStatementKind::Load(LoadInfo {
            filename,
            load_type: LoadType::Load,
            repository: ByteString::new(),
            version: ByteString::new(),
            repo_is_alias: false,
        })))
    }

    /// `TransformInstallStatement`: `FORCE? INSTALL IdentifierOrStringLiteral FromSource?
    /// VersionNumber?`.
    pub(super) fn transform_install_statement(&mut self, parse_result: &ParseResult) -> Result<SqlStatement> {
        let list = parse_result.list()?;
        let load_type = if list.has(0)? {
            LoadType::ForceInstall
        } else {
            LoadType::Install
        };
        let extension_name: QualifiedName = self.transform_child(list, 2)?;
        let mut info = LoadInfo {
            filename: extension_name.name,
            load_type,
            repository: ByteString::new(),
            version: ByteString::new(),
            repo_is_alias: false,
        };
        if let Some(from_source) = list.optional(3)? {
            let repository_info: ExtensionRepositoryInfo = self.transform(from_source)?;
            info.repository = repository_info.name.into();
            info.repo_is_alias = repository_info.repository_is_alias;
        }
        if let Some(version) = self.transform_optional(list, 4)? {
            info.version = version;
        }
        Ok(SqlStatement::new(SqlStatementKind::Load(info)))
    }

    /// `TransformFromSource`: `FROM (Identifier / StringLiteral)`; an identifier is a
    /// repository alias.
    pub(super) fn transform_from_source(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ExtensionRepositoryInfo> {
        let string_or_identifier = parse_result.list()?.list(1)?.choice(0)?;
        if string_or_identifier.result_type() == ParseResultType::String {
            return Ok(ExtensionRepositoryInfo {
                name: self.transform(string_or_identifier)?,
                repository_is_alias: false,
            });
        }
        Ok(ExtensionRepositoryInfo {
            name: string_or_identifier.identifier()?.to_owned(),
            repository_is_alias: true,
        })
    }

    /// `TransformUpdateExtensionsStatement`: `UPDATE EXTENSIONS
    /// Parens(List(Identifier))?`.
    pub(super) fn transform_update_extensions_statement(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<SqlStatement> {
        let mut info = UpdateExtensionsInfo {
            extensions_to_update: Vec::new(),
        };
        if let Some(extensions) = parse_result.list()?.optional(2)? {
            for extension in extensions.parens_content()?.list_elements()? {
                info.extensions_to_update
                    .push(extension.identifier()?.to_owned().into());
            }
        }
        Ok(SqlStatement::new(SqlStatementKind::UpdateExtensions(info)))
    }

    /// `TransformVersionNumber`: `VERSION IdentifierOrStringLiteral`.
    pub(super) fn transform_version_number(&mut self, parse_result: &ParseResult) -> Result<ByteString> {
        let version: QualifiedName = self.transform_child(parse_result.list()?, 1)?;
        Ok(version.name)
    }
}
