//! SQL completion (DuckDB's `GenerateSuggestions` and `ComputeSuggestions` in
//! `autocomplete_extension.cpp`): the tokens before the cursor run through the grammar,
//! the places where they run out name keywords and kinds of names, the names are read
//! from the catalog, the settings and the file system, and the candidates are ranked
//! against the word being typed.

use std::collections::HashMap;
use std::sync::Arc;

use rustdb_ast::keyword::write_optionally_quoted;
use rustdb_catalog::{Catalog, CatalogEntry, CatalogSet, EntryKind, SchemaEntry};
use rustdb_common::Result;
use rustdb_common::string_util::{similarity_score, top_n_distances};
use rustdb_planner::ClientContext;

use crate::matcher::{
    AutoCompleteCandidate, CandidateType, Grammar, MatchContext, MatchState, MatcherSuggestion,
    SuggestionState,
};
use crate::tokenizer::{
    MatcherToken, TokenizeState, TokenizerCallbacks, is_string_util_operator, tokenize_input,
};

/// The options of a `sql_auto_complete` call (DuckDB's `AutoCompleteParameters`).
#[derive(Clone, Copy, Debug)]
pub struct AutoCompleteParameters {
    pub max_suggestion_count: usize,
    pub max_file_suggestion_count: usize,
    pub max_exact_suggestion_count: usize,
    /// Set once file names are among the candidates.
    pub suggestion_contains_files: bool,
}

impl Default for AutoCompleteParameters {
    fn default() -> Self {
        Self {
            max_suggestion_count: 20,
            max_file_suggestion_count: 1,
            max_exact_suggestion_count: 100,
            suggestion_contains_files: false,
        }
    }
}

/// One completion (DuckDB's `AutoCompleteSuggestion`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AutoCompleteSuggestion {
    pub text: String,
    /// The byte offset in the query the suggestion replaces from.
    pub pos: usize,
    /// The kind of suggestion (`GetSuggestionType`), empty for variables.
    pub suggestion_type: &'static str,
    pub score: usize,
    pub extra_char: Option<char>,
}

/// Where completion candidates come from: the client's catalog and settings, and the
/// pragma functions (which DuckDB keeps in the catalog).
pub struct CompletionSources<'a> {
    pub context: &'a ClientContext,
    pub pragma_names: &'a dyn Fn() -> Vec<String>,
}

impl std::fmt::Debug for CompletionSources<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompletionSources").finish_non_exhaustive()
    }
}

/// `GetSuggestionType`.
const fn suggestion_type_name(state: SuggestionState) -> &'static str {
    match state {
        SuggestionState::Keyword => "keyword",
        SuggestionState::CatalogName => "catalog",
        SuggestionState::SchemaName => "schema",
        SuggestionState::TableName => "table",
        SuggestionState::TypeName => "type",
        SuggestionState::ColumnName => "column",
        SuggestionState::FileName => "file_name",
        SuggestionState::Directory => "directory",
        SuggestionState::ScalarFunctionName => "scalar_function",
        SuggestionState::TableFunctionName => "table_function",
        SuggestionState::PragmaName => "pragma_function",
        SuggestionState::SettingName => "setting",
        SuggestionState::Variable => "",
    }
}

/// `PreferCaseMatching`: names whose case is meaningful rank lower when it differs.
const fn prefer_case_matching(state: SuggestionState) -> bool {
    matches!(
        state,
        SuggestionState::ScalarFunctionName
            | SuggestionState::TableFunctionName
            | SuggestionState::PragmaName
            | SuggestionState::SettingName
            | SuggestionState::FileName
    )
}

/// `ComputeSuggestions`: scores the candidates against `prefix` (the word being typed)
/// and returns the best ones, best first.
fn compute_suggestions(
    mut available: Vec<AutoCompleteCandidate>,
    prefix: &str,
    parameters: &AutoCompleteParameters,
) -> Vec<AutoCompleteSuggestion> {
    const BASE_SCORE: i32 = 10;
    const SUBSTRING_PENALTY: usize = 10;
    let mut scores: Vec<(String, usize)> = Vec::with_capacity(available.len());
    // The first candidate of every (case-insensitive) text.
    let mut matches: HashMap<String, usize> = HashMap::new();
    let lower_prefix = prefix.to_ascii_lowercase();
    let prefix_is_lower = prefix == lower_prefix;
    let prefix_is_upper = prefix == prefix.to_ascii_uppercase();
    for (index, suggestion) in available.iter_mut().enumerate() {
        let mut text = suggestion.candidate.clone();
        if let Some(extra_char) = suggestion.extra_char {
            text.push(extra_char);
        }
        let lower_text = text.to_ascii_lowercase();
        if matches.contains_key(&lower_text) {
            continue;
        }
        let mut score = usize::try_from(BASE_SCORE - suggestion.score_bonus).unwrap_or(0);
        let mut match_score = if prefix.is_empty() {
            0
        } else {
            let compared = &text.as_bytes()[..prefix.len().min(text.len())];
            similarity_score(compared, prefix.as_bytes())
        };
        let ty = suggestion.suggestion_type;
        if text.starts_with('.') {
            if matches!(ty, SuggestionState::Directory | SuggestionState::FileName) {
                score += 1;
            }
        } else if ty == SuggestionState::Directory && score > 0 {
            score -= 1;
        }
        if !lower_text.contains(&lower_prefix) {
            score += SUBSTRING_PENALTY;
        } else if prefer_case_matching(ty) && !text.contains(prefix) {
            // A small penalty for not matching the case.
            match_score += 1;
        }
        score += match_score;
        suggestion.score = Some(match_score);
        matches.insert(lower_text, index);
        scores.push((text, score));
    }
    let fuzzy_suggestion_count = if parameters.suggestion_contains_files {
        parameters.max_file_suggestion_count
    } else {
        parameters.max_suggestion_count
    };
    let suggestion_count = parameters.max_exact_suggestion_count.max(fuzzy_suggestion_count);
    let mut results = Vec::new();
    for mut result in top_n_distances(scores, suggestion_count, 999) {
        let lower_result = result.to_ascii_lowercase();
        // DuckDB compares the length of the text here (not the number of results):
        // longer texts are only accepted if they start with the prefix.
        if result.len() > fuzzy_suggestion_count && !lower_result.starts_with(&lower_prefix) {
            break;
        }
        let Some(&index) = matches.get(&lower_result) else {
            continue;
        };
        let suggestion = &available[index];
        if suggestion.extra_char.is_some() {
            result.pop();
        }
        match suggestion.candidate_type {
            CandidateType::Keyword if prefix_is_lower => result = result.to_ascii_lowercase(),
            CandidateType::Keyword if prefix_is_upper => result = result.to_ascii_uppercase(),
            CandidateType::Identifier => result = write_optionally_quoted(&result),
            CandidateType::Keyword | CandidateType::Literal => {}
        }
        if let Some(extra_char) = suggestion.extra_char {
            result.push(extra_char);
        }
        results.push(AutoCompleteSuggestion {
            text: result,
            pos: suggestion.suggestion_pos,
            suggestion_type: suggestion_type_name(suggestion.suggestion_type),
            score: suggestion.score.unwrap_or(0),
            extra_char: suggestion.extra_char,
        });
    }
    results
}

/// The catalogs `GetAllSchemas` reads: every attached database but the hidden ones.
fn visible_catalogs(context: &ClientContext) -> impl Iterator<Item = &Arc<Catalog>> {
    context
        .catalogs
        .iter()
        .filter(|catalog| !catalog.attach_info().is_some_and(|info| info.hidden))
}

/// `Catalog::GetAllSchemas`: the schemas of every visible catalog, ordered by catalog
/// and schema name.
fn all_schemas(context: &ClientContext) -> Vec<(String, Arc<CatalogEntry>)> {
    let mut schemas: Vec<(String, Arc<CatalogEntry>)> = visible_catalogs(context)
        .flat_map(|catalog| {
            catalog
                .schemas()
                .scan(&context.transaction)
                .into_iter()
                .map(|schema| (catalog.name().to_owned(), schema))
        })
        .collect();
    schemas.sort_by(|(left_catalog, left), (right_catalog, right)| {
        left_catalog
            .cmp(right_catalog)
            .then_with(|| left.name().cmp(right.name()))
    });
    schemas
}

/// The contents of a schema entry.
fn schema_contents(schema: &CatalogEntry) -> Option<&SchemaEntry> {
    match &schema.kind {
        EntryKind::Schema(contents) => Some(contents),
        _ => None,
    }
}

/// The entries of one kind of every schema (`Catalog::GetAllEntries`), `set` picking
/// the schema's catalog set of the kind (`DuckSchemaEntry::GetCatalogSet`).
fn all_entries(context: &ClientContext, set: fn(&SchemaEntry) -> &Arc<CatalogSet>) -> Vec<Arc<CatalogEntry>> {
    all_schemas(context)
        .iter()
        .filter_map(|(_, schema)| schema_contents(schema))
        .flat_map(|contents| set(contents).scan(&context.transaction))
        .collect()
}

/// `GetAllTables`: tables and views (internal ones only for table names), then table
/// functions for table names or scalar functions for column names.
fn all_tables(context: &ClientContext, for_table_names: bool) -> Vec<Arc<CatalogEntry>> {
    let mut result: Vec<Arc<CatalogEntry>> = all_entries(context, |schema| &schema.tables)
        .into_iter()
        .filter(|entry| !entry.internal || for_table_names)
        .collect();
    if for_table_names {
        result.extend(all_entries(context, |schema| &schema.table_functions));
    } else {
        result.extend(all_entries(context, |schema| &schema.functions));
    }
    result
}

/// A candidate that is followed by `extra_char`.
fn with_extra_char(mut candidate: AutoCompleteCandidate, extra_char: char) -> AutoCompleteCandidate {
    candidate.extra_char = Some(extra_char);
    candidate
}

/// `SuggestCatalogName`: every attached database (`DatabaseManager::GetDatabases`).
fn suggest_catalog_name(context: &ClientContext) -> Vec<AutoCompleteCandidate> {
    context
        .catalogs
        .iter()
        .map(|catalog| {
            with_extra_char(
                AutoCompleteCandidate::identifier(catalog.name(), SuggestionState::CatalogName, 0),
                '.',
            )
        })
        .collect()
}

/// `SuggestSchemaName`.
fn suggest_schema_name(context: &ClientContext) -> Vec<AutoCompleteCandidate> {
    all_schemas(context)
        .iter()
        .map(|(_, schema)| {
            with_extra_char(
                AutoCompleteCandidate::identifier(schema.name(), SuggestionState::SchemaName, 0),
                '.',
            )
        })
        .collect()
}

/// `SuggestTableName`: user-defined entries rank first, except table functions (table
/// macros are entries of their own type in DuckDB, so user-defined ones do rank first).
fn suggest_table_name(context: &ClientContext) -> Vec<AutoCompleteCandidate> {
    all_tables(context, true)
        .iter()
        .map(|entry| {
            let is_table_function = matches!(entry.kind, EntryKind::TableFunction(_));
            let bonus = i32::from(!entry.internal && !is_table_function);
            AutoCompleteCandidate::identifier(entry.name(), SuggestionState::TableName, bonus)
        })
        .collect()
}

/// `SuggestType`: the types of every schema (with the builtin types of every `main`
/// schema, DuckDB's `DefaultTypeGenerator`); user-defined types rank first.
fn suggest_type(context: &ClientContext) -> Vec<AutoCompleteCandidate> {
    let mut suggestions = Vec::new();
    for (_, schema) in all_schemas(context) {
        let Some(contents) = schema_contents(&schema) else {
            continue;
        };
        let mut types: Vec<(String, bool)> = contents
            .types
            .scan(&context.transaction)
            .iter()
            .map(|entry| (entry.name().to_owned(), entry.internal))
            .collect();
        if schema.name() == rustdb_catalog::DEFAULT_SCHEMA {
            types.extend(
                rustdb_types::builtin_type_names()
                    .filter(|name| rustdb_types::builtin_type_id(name).is_some())
                    .map(|name| (name.to_ascii_lowercase(), true)),
            );
        }
        suggestions.extend(types.into_iter().map(|(name, internal)| {
            AutoCompleteCandidate::new(
                name,
                SuggestionState::TypeName,
                i32::from(!internal),
                CandidateType::Keyword,
            )
        }));
    }
    suggestions
}

/// `SuggestColumnName`: the columns of user tables and views, then function names.
fn suggest_column_name(context: &ClientContext) -> Vec<AutoCompleteCandidate> {
    let mut suggestions = Vec::new();
    let column =
        |name: &str, bonus: i32| AutoCompleteCandidate::identifier(name, SuggestionState::ColumnName, bonus);
    for entry in all_tables(context, false) {
        match &entry.kind {
            EntryKind::Table(table) => {
                let bonus = if entry.internal { 0 } else { 3 };
                suggestions.extend(table.columns.iter().map(|col| column(&col.name, bonus)));
            }
            EntryKind::View(view) => {
                let bonus = if entry.internal { 0 } else { 3 };
                if view.names.is_empty() {
                    // Only the aliases.
                    suggestions.extend(view.aliases.iter().map(|alias| column(alias, bonus)));
                } else {
                    suggestions.extend(
                        view.names
                            .iter()
                            .enumerate()
                            .map(|(n, name)| column(view.aliases.get(n).unwrap_or(name), bonus)),
                    );
                }
            }
            _ => {
                if entry.name().bytes().next().is_some_and(is_string_util_operator) {
                    continue;
                }
                let bonus = if entry.internal { 0 } else { 2 };
                suggestions.push(column(entry.name(), bonus));
            }
        }
    }
    suggestions
}

/// Candidates of `state` named by `names`.
fn named_candidates(
    names: impl IntoIterator<Item = String>,
    state: SuggestionState,
) -> Vec<AutoCompleteCandidate> {
    names
        .into_iter()
        .map(|name| AutoCompleteCandidate::identifier(name, state, 0))
        .collect()
}

/// `SuggestSettingName`: the options and their aliases.
fn suggest_setting_name(context: &ClientContext) -> Vec<AutoCompleteCandidate> {
    let settings = context
        .function_client
        .settings()
        .map(|settings| settings.settings())
        .unwrap_or_default();
    let names: Vec<String> = settings.iter().map(|setting| setting.name.clone()).collect();
    let aliases = settings.into_iter().flat_map(|setting| setting.aliases);
    named_candidates(names.into_iter().chain(aliases), SuggestionState::SettingName)
}

/// `KnownExtension`: files of the formats DuckDB reads rank higher.
fn known_extension(file_name: &str) -> bool {
    [".parquet", ".csv", ".tsv", ".csv.gz", ".tsv.gz", ".tbl"]
        .iter()
        .any(|extension| file_name.ends_with(extension))
}

/// Whether `enable_external_access` is on (it is unless the client says otherwise).
fn external_access_enabled(context: &ClientContext) -> bool {
    context
        .function_client
        .settings()
        .and_then(|settings| settings.current_setting("enable_external_access").ok())
        .and_then(|value| value.as_bool())
        .unwrap_or(true)
}

/// `SuggestFileName`: the files and directories next to the typed path. `prefix`
/// becomes the part after the last separator and `last_pos` where that part starts.
fn suggest_file_name(
    context: &ClientContext,
    prefix: &mut String,
    last_pos: &mut usize,
) -> Vec<AutoCompleteCandidate> {
    if !external_access_enabled(context) {
        // With external access disabled the file system is not searched.
        return Vec::new();
    }
    let is_path_absolute = prefix.starts_with('/') || prefix.starts_with("file:/");
    let mut search_dir = String::new();
    if let Some(separator) = prefix.rfind(['/', '\\']) {
        prefix[..separator].clone_into(&mut search_dir);
        *prefix = prefix[separator + 1..].to_owned();
        *last_pos += separator + 1;
    }
    let search_dir = if search_dir.is_empty() {
        if is_path_absolute { "/" } else { "." }.to_owned()
    } else {
        rustdb_function::file_system::expand_path(&search_dir)
    };
    let mut result = Vec::new();
    // `LocalFileSystem::ListFiles`: regular files and directories (following links).
    let Ok(entries) = std::fs::read_dir(rustdb_function::file_system::expand_path(&search_dir)) else {
        return result;
    };
    for entry in entries.flatten() {
        let file_name = entry.file_name().to_string_lossy().into_owned();
        let Ok(metadata) = std::fs::metadata(entry.path()) else {
            continue;
        };
        let is_dir = metadata.is_dir();
        if !is_dir && !metadata.is_file() {
            continue;
        }
        let mut score = if is_dir && !file_name.starts_with('.') {
            2
        } else {
            0
        };
        if known_extension(&file_name) {
            score = 1;
        }
        let (state, extra_char) = if is_dir {
            (SuggestionState::Directory, '/')
        } else {
            (SuggestionState::FileName, '\'')
        };
        result.push(with_extra_char(
            AutoCompleteCandidate::new(file_name, state, score, CandidateType::Literal),
            extra_char,
        ));
    }
    result
}

/// The tokenizer of the query being completed (DuckDB's `AutoCompleteTokenizer`): an
/// unterminated string at the end suggests file names.
#[derive(Debug, Default)]
struct AutoCompleteTokenizer {
    suggestions: Vec<MatcherSuggestion>,
    last_word: String,
    last_pos: usize,
}

impl TokenizerCallbacks for AutoCompleteTokenizer {
    fn on_last_token(
        &mut self,
        _: &mut Vec<MatcherToken>,
        state: TokenizeState,
        last_word: String,
        last_pos: usize,
    ) {
        if state == TokenizeState::StringLiteral {
            self.suggestions
                .push(MatcherSuggestion::Entries(SuggestionState::FileName));
        }
        self.last_word = last_word;
        self.last_pos = last_pos;
    }
}

/// `GenerateSuggestions`: the completions of the end of `sql`.
pub fn generate_suggestions(
    sources: &CompletionSources<'_>,
    sql: &str,
    parameters: &mut AutoCompleteParameters,
) -> Result<Vec<AutoCompleteSuggestion>> {
    let context = sources.context;
    let stripped = rustdb_pg_parser::strip_unicode_spaces(sql);
    let sql = stripped.as_deref().unwrap_or(sql);
    let mut tokens = Vec::new();
    let mut tokenizer = AutoCompleteTokenizer::default();
    if !tokenize_input(sql, &mut tokens, &mut tokenizer) {
        return Ok(Vec::new());
    }
    let mut suggestions = std::mem::take(&mut tokenizer.suggestions);
    if suggestions.is_empty() {
        // Nothing from the tokenizer: run the grammar.
        let mut ctx = MatchContext::new(&tokens);
        Grammar::get()?.match_root(&mut ctx, &mut MatchState::default())?;
        suggestions = ctx.suggestions;
    }
    let mut available = Vec::new();
    for suggestion in suggestions {
        let mut suggestion_pos = tokenizer.last_pos;
        let new_suggestions = match suggestion {
            MatcherSuggestion::Keyword(candidate) => vec![candidate],
            // Variables have no suggestions.
            MatcherSuggestion::Entries(SuggestionState::Variable | SuggestionState::Keyword) => Vec::new(),
            MatcherSuggestion::Entries(SuggestionState::CatalogName) => suggest_catalog_name(context),
            MatcherSuggestion::Entries(SuggestionState::SchemaName) => suggest_schema_name(context),
            MatcherSuggestion::Entries(SuggestionState::TableName) => suggest_table_name(context),
            MatcherSuggestion::Entries(SuggestionState::ColumnName) => suggest_column_name(context),
            MatcherSuggestion::Entries(SuggestionState::TypeName) => suggest_type(context),
            MatcherSuggestion::Entries(SuggestionState::FileName | SuggestionState::Directory) => {
                if parameters.max_file_suggestion_count > 0 {
                    parameters.suggestion_contains_files = true;
                    suggest_file_name(context, &mut tokenizer.last_word, &mut suggestion_pos)
                } else {
                    Vec::new()
                }
            }
            MatcherSuggestion::Entries(SuggestionState::ScalarFunctionName) => named_candidates(
                all_entries(context, |schema| &schema.functions)
                    .iter()
                    .map(|entry| entry.name().to_owned()),
                SuggestionState::ScalarFunctionName,
            ),
            MatcherSuggestion::Entries(SuggestionState::TableFunctionName) => named_candidates(
                all_entries(context, |schema| &schema.table_functions)
                    .iter()
                    .map(|entry| entry.name().to_owned()),
                SuggestionState::TableFunctionName,
            ),
            MatcherSuggestion::Entries(SuggestionState::PragmaName) => {
                named_candidates((sources.pragma_names)(), SuggestionState::PragmaName)
            }
            MatcherSuggestion::Entries(SuggestionState::SettingName) => suggest_setting_name(context),
        };
        available.extend(new_suggestions.into_iter().map(|mut candidate| {
            candidate.suggestion_pos = suggestion_pos;
            candidate
        }));
    }
    Ok(compute_suggestions(available, &tokenizer.last_word, parameters))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keyword(text: &str) -> AutoCompleteCandidate {
        let mut candidate =
            AutoCompleteCandidate::new(text, SuggestionState::Keyword, 0, CandidateType::Keyword);
        candidate.extra_char = Some(' ');
        candidate
    }

    #[test]
    fn keywords_follow_the_prefix_case() {
        let parameters = AutoCompleteParameters::default();
        let results = compute_suggestions(vec![keyword("SELECT"), keyword("SET")], "sel", &parameters);
        assert_eq!(results[0].text, "select ");
        assert_eq!(results[0].score, 0);
        let results = compute_suggestions(vec![keyword("SELECT")], "Sel", &parameters);
        assert_eq!(results[0].text, "SELECT ");
    }

    #[test]
    fn identifiers_are_quoted_and_deduplicated() {
        let parameters = AutoCompleteParameters::default();
        let candidates = vec![
            AutoCompleteCandidate::identifier("My Table", SuggestionState::TableName, 1),
            AutoCompleteCandidate::identifier("my table", SuggestionState::TableName, 0),
        ];
        let results = compute_suggestions(candidates, "my", &parameters);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].text, "\"My Table\"");
        assert_eq!(results[0].suggestion_type, "table");
    }
}
