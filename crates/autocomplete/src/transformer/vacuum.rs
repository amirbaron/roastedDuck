//! The transform functions of DuckDB's `transform_vacuum.cpp`.

#![allow(
    clippy::unused_self,
    reason = "transform functions have the signature of the rule registry"
)]

use super::analyze::vacuum_statement;
use super::prelude::*;

impl PegTransformer {
    /// `TransformVacuumStatement`: `VACUUM VacuumOptions? AnalyzeTarget?`.
    pub(super) fn transform_vacuum_statement(&mut self, parse_result: &ParseResult) -> Result<SqlStatement> {
        let list = parse_result.list()?;
        let mut options = VacuumOptions::default();
        if let Some(vacuum_options) = self.transform_optional(list, 1)? {
            options = vacuum_options;
        }
        let target = self.transform_optional(list, 2)?;
        Ok(vacuum_statement(options, target))
    }

    /// `TransformVacuumOptions`: `VacuumParensOptions / VacuumLegacyOptions`.
    pub(super) fn transform_vacuum_options(&mut self, parse_result: &ParseResult) -> Result<VacuumOptions> {
        self.transform(parse_result.list()?.choice(0)?)
    }

    /// `TransformVacuumLegacyOptions`: `OptFull? OptFreeze? OptVerbose? OptAnalyze?`.
    pub(super) fn transform_vacuum_legacy_options(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<VacuumOptions> {
        let list = parse_result.list()?;
        let options = VacuumOptions {
            vacuum: true,
            analyze: list.has(3)?,
        };
        if list.has(0)? {
            return Err(Error::not_implemented("FULL is not yet implemented"));
        }
        if list.has(1)? {
            return Err(Error::not_implemented("FREEZE is not yet implemented"));
        }
        if list.has(2)? {
            return Err(Error::not_implemented("VERBOSE is not yet implemented"));
        }
        Ok(options)
    }

    /// `TransformVacuumParensOptions`: `Parens(List(VacuumOption))`.
    pub(super) fn transform_vacuum_parens_options(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<VacuumOptions> {
        let option_list = parse_result.list()?.child(0)?.parens_content()?.list_elements()?;
        let mut options = VacuumOptions {
            vacuum: true,
            analyze: false,
        };
        for option in option_list {
            let option_name: String = self.transform(option)?;
            if option_name.eq_ignore_ascii_case("disable_page_skipping") {
                return Err(Error::not_implemented("Disable Page Skipping vacuum option"));
            }
            if option_name.eq_ignore_ascii_case("freeze") {
                return Err(Error::not_implemented("FREEZE is not yet implemented"));
            }
            if option_name.eq_ignore_ascii_case("full") {
                return Err(Error::not_implemented("FULL is not yet implemented"));
            }
            if option_name.eq_ignore_ascii_case("verbose") {
                return Err(Error::not_implemented("VERBOSE is not yet implemented"));
            }
            if option_name.eq_ignore_ascii_case("analyze") {
                options.analyze = true;
            }
        }
        Ok(options)
    }

    /// `TransformVacuumOption`: `OptAnalyze / OptFreeze / OptFull / OptVerbose /
    /// Identifier`.
    pub(super) fn transform_vacuum_option(&mut self, parse_result: &ParseResult) -> Result<String> {
        let option = parse_result.list()?.choice(0)?;
        if option.result_type() == ParseResultType::Identifier {
            return Ok(option.identifier()?.to_owned());
        }
        Self::transform_enum(option)
    }

    /// `TransformNameList`: `Parens(List(ColId))`.
    pub(super) fn transform_name_list(&mut self, parse_result: &ParseResult) -> Result<Vec<String>> {
        parse_result
            .list()?
            .child(0)?
            .parens_content()?
            .list_elements()?
            .into_iter()
            .map(|colid| self.transform(colid))
            .collect()
    }
}
