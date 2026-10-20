//! `SET` / `RESET` of configuration options (DuckDB's `PhysicalSet` / `PhysicalReset`
//! and the option table of `config.cpp`). Options the binder reads are applied to the
//! connection's [`PlannerConfig`]; other known options are accepted and remembered.

use std::collections::HashMap;

use rustdb_ast::OrderType;
use rustdb_common::string_util::candidates_error_message;
use rustdb_common::{Error, ErrorKind, Result};
use rustdb_execution::{ExplainOutput, JoinPlanSettings};
use rustdb_function::cast::ValueCast;
use rustdb_optimizer::{OptimizerConfig, OptimizerType};
use rustdb_planner::{DefaultNullOrder, PlannerConfig};
use rustdb_types::{LogicalType, Value};

use crate::default_settings::{SETTINGS, SettingDefinition};
use crate::query_profiler::ProfilerConfig;

/// Options whose `SetGlobal`/`ResetGlobal` always fail
/// (`DeltaOnlyVariantEncodingEnabledSetting`).
fn check_user_adjustable(option: &str) -> Result<()> {
    if option == "__delta_only_variant_encoding_enabled" {
        return Err(Error::invalid_input("This setting is not adjustable by a user"));
    }
    Ok(())
}

/// DuckDB's built-in option names (`internal_options`), followed by the aliases.
const OPTION_NAMES: &[&str] = &[
    "__delta_only_variant_encoding_enabled",
    "access_mode",
    "allocator_background_threads",
    "allocator_bulk_deallocation_flush_threshold",
    "allocator_flush_threshold",
    "allow_community_extensions",
    "allow_extensions_metadata_mismatch",
    "allow_parser_override_extension",
    "allow_persistent_secrets",
    "allow_unredacted_secrets",
    "allow_unsigned_extensions",
    "allowed_configs",
    "allowed_directories",
    "allowed_paths",
    "arrow_large_buffer_size",
    "arrow_lossless_conversion",
    "arrow_output_list_view",
    "arrow_output_version",
    "asof_loop_join_threshold",
    "auto_checkpoint_skip_wal_threshold",
    "autoinstall_extension_repository",
    "autoinstall_known_extensions",
    "autoload_known_extensions",
    "binary_as_string",
    "block_allocator_memory",
    "catalog_error_max_schemas",
    "checkpoint_threshold",
    "current_transaction_invalidation_policy",
    "custom_extension_repository",
    "custom_profiling_settings",
    "custom_user_agent",
    "debug_asof_iejoin",
    "debug_checkpoint_abort",
    "debug_checkpoint_scan_sleep_ms",
    "debug_checkpoint_sleep_ms",
    "debug_eviction_queue_sleep_micro_seconds",
    "debug_force_external",
    "debug_force_no_cross_product",
    "debug_physical_table_scan_execution_strategy",
    "debug_skip_checkpoint_on_commit",
    "debug_verify_blocks",
    "debug_verify_vector",
    "debug_window_mode",
    "default_block_size",
    "default_collation",
    "default_null_order",
    "default_order",
    "default_secret_storage",
    "deprecated_using_key_syntax",
    "disable_database_invalidation",
    "disable_parquet_prefetching",
    "disable_timestamptz_casts",
    "disabled_compression_methods",
    "disabled_filesystems",
    "disabled_log_types",
    "disabled_optimizers",
    "duckdb_api",
    "dynamic_or_filter_threshold",
    "enable_caching_operators",
    "enable_external_access",
    "enable_external_file_cache",
    "enable_fsst_vectors",
    "enable_geoparquet_conversion",
    "enable_http_logging",
    "enable_http_metadata_cache",
    "enable_logging",
    "enable_macro_dependencies",
    "enable_object_cache",
    "enable_optimistic_write",
    "enable_profiling",
    "enable_progress_bar",
    "enable_progress_bar_print",
    "enable_view_dependencies",
    "enabled_log_types",
    "errors_as_json",
    "experimental_metadata_reuse",
    "explain_output",
    "extension_directories",
    "extension_directory",
    "external_threads",
    "file_search_path",
    "force_bitpacking_mode",
    "force_column_metadata_reuse",
    "force_compression",
    "force_mbedtls_unsafe",
    "force_variant_shredding",
    "geometry_minimum_shredding_size",
    "home_directory",
    "http_logging_output",
    "http_proxy",
    "http_proxy_password",
    "http_proxy_username",
    "ieee_floating_point_ops",
    "ignore_unknown_crs",
    "immediate_transaction_mode",
    "index_scan_max_count",
    "index_scan_percentage",
    "integer_division",
    "lambda_syntax",
    "late_materialization_max_rows",
    "lock_configuration",
    "log_query_path",
    "logging_level",
    "logging_mode",
    "logging_storage",
    "max_expression_depth",
    "max_memory",
    "max_temp_directory_size",
    "max_vacuum_tasks",
    "merge_join_threshold",
    "nested_loop_join_threshold",
    "old_implicit_casting",
    "order_by_non_integer_literal",
    "ordered_aggregate_threshold",
    "parquet_metadata_cache",
    "partitioned_write_flush_threshold",
    "partitioned_write_max_open_files",
    "password",
    "perfect_ht_threshold",
    "pin_threads",
    "pivot_filter_threshold",
    "pivot_limit",
    "prefer_range_joins",
    "prefetch_all_parquet_files",
    "preserve_identifier_case",
    "preserve_insertion_order",
    "produce_arrow_string_view",
    "profile_output",
    "profiling_coverage",
    "profiling_mode",
    "progress_bar_time",
    "scalar_subquery_error_on_multiple_rows",
    "scheduler_process_partial",
    "schema",
    "search_path",
    "secret_directory",
    "storage_block_prefetch",
    "storage_compatibility_version",
    "streaming_buffer_size",
    "temp_directory",
    "temp_file_encryption",
    "threads",
    "username",
    "vacuum_rebuild_indexes",
    "validate_external_file_cache",
    "variant_minimum_shredding_size",
    "wal_autocheckpoint_entries",
    "warnings_as_errors",
    "write_buffer_row_group_count",
    "write_buffer_row_group_memory_limit",
    "zstd_min_string_length",
    // aliases
    "memory_limit",
    "null_order",
    "profiling_output",
    "user",
    "wal_autocheckpoint",
    "worker_threads",
];

/// The canonical name of an option (`null_order` is an alias of `default_null_order`).
fn canonical_name(name: &str) -> Option<&'static str> {
    let lower = name.to_ascii_lowercase();
    let lower = if lower == "null_order" {
        "default_null_order".to_owned()
    } else {
        lower
    };
    OPTION_NAMES.iter().copied().find(|option| *option == lower)
}

/// DuckDB's `Catalog::UnrecognizedConfigurationError`; `enabled` reads the boolean
/// options `autoload_known_extensions` and `autoinstall_known_extensions`.
fn unrecognized(name: &str, enabled: impl Fn(&str) -> bool) -> Error {
    use rustdb_function::extension_entries::{EXTENSION_SETTINGS, add_install_hint, find_extension};
    if let Some(extension) = find_extension(name, EXTENSION_SETTINGS) {
        let message = format!(
            "Setting with name \"{name}\" is not in the catalog, but it exists in the {extension} extension."
        );
        return Error::catalog(add_install_hint(
            &message,
            extension,
            enabled("autoload_known_extensions"),
            enabled("autoinstall_known_extensions"),
        ));
    }
    let names: Vec<String> = OPTION_NAMES.iter().map(|name| (*name).to_owned()).collect();
    Error::catalog(format!(
        "unrecognized configuration parameter \"{name}\"\n{}",
        candidates_error_message(&names, name, "Did you mean")
    ))
}

/// Whether a parser extension replaces the Postgres parser
/// (`allow_parser_override_extension`, DuckDB's `AllowParserOverride`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ParserOverride {
    /// The Postgres parser only.
    Default,
    /// The extension's parser, falling back to the Postgres parser when it fails.
    Fallback,
    /// The extension's parser; its errors are final.
    Strict,
}

/// The connection's option values.
#[derive(Clone, Debug, Default)]
pub(crate) struct Settings {
    pub planner: PlannerConfig,
    /// Values of options the engine does not interpret yet.
    other: HashMap<&'static str, Value>,
    /// Every option set (and not reset since), in the order of the first `SET`.
    pub(crate) assigned: Vec<(&'static str, Value)>,
    /// The options that stay changeable under `lock_configuration`, by canonical name
    /// (`DBConfigOptions::allowed_configs`).
    allowed_configs: Vec<&'static str>,
    /// `PRAGMA disable_optimizer` (DuckDB's `ClientConfig::enable_optimizer`, negated).
    pub optimizer_disabled: bool,
    /// The profiling options (`enable_profiling`, `profiling_output`, `profiling_mode`,
    /// `profiling_coverage`, `custom_profiling_settings`).
    pub profiler: ProfilerConfig,
}

/// The canonical option `name` stands for, resolving aliases (`DBConfig::GetOptionByName`).
fn option_by_name(name: &str) -> Option<&'static str> {
    SETTINGS
        .iter()
        .find(|definition| {
            definition
                .aliases
                .iter()
                .any(|alias| alias.eq_ignore_ascii_case(name))
        })
        .or_else(|| {
            SETTINGS
                .iter()
                .find(|definition| definition.name.eq_ignore_ascii_case(name))
        })
        .map(|definition| definition.name)
}

/// The canonical name of option `name` (resolving aliases), if it is one.
pub(crate) fn canonical_option(name: &str) -> Option<&'static str> {
    option_by_name(name).or_else(|| canonical_name(name))
}

/// Whether a `SET` of option `name` without a scope sets it for the whole database
/// (DuckDB's `GetSettingScope`: the option's scope is global).
pub(crate) fn is_global_option(name: &str) -> bool {
    option_by_name(name)
        .and_then(|canonical| SETTINGS.iter().find(|definition| definition.name == canonical))
        .is_some_and(|definition| definition.global)
}

/// `StringUtil::StringToEnum`'s error for an unknown value of enum `enum_name`.
pub(crate) fn enum_value_error(value: &str, enum_name: &str, values: &[&str]) -> Error {
    let candidates: Vec<String> = values.iter().map(|&value| value.to_owned()).collect();
    Error::not_implemented(format!(
        "Enum value: unrecognized value \"{value}\" for enum \"{enum_name}\"\n{}",
        rustdb_common::string_util::candidates_message(
            &rustdb_common::string_util::top_n_jaro_winkler(&candidates, value),
            "Candidates"
        )
    ))
}

/// `EnumUtil::FromString` of an enum given by its values: case-insensitive.
fn check_enum_value(value: &Value, enum_name: &str, values: &[&str]) -> Result<()> {
    let text = value.to_varchar()?;
    if values
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(&text))
    {
        Ok(())
    } else {
        Err(enum_value_error(&text, enum_name, values))
    }
}

/// The options whose values name a member of an enum (the `OnSet` callbacks of
/// DuckDB's `autogenerated_settings.cpp`): option, enum and the enum's values.
const ENUM_SETTINGS: [(&str, &str, &[&str]); 13] = [
    (
        "allow_parser_override_extension",
        "AllowParserOverride",
        &["DEFAULT", "FALLBACK", "STRICT"],
    ),
    (
        "arrow_output_version",
        "ArrowFormatVersion",
        &["1.0", "1.1", "1.2", "1.3", "1.4", "1.5"],
    ),
    (
        "debug_checkpoint_abort",
        "CheckpointAbort",
        &[
            "NONE",
            "BEFORE_TRUNCATE",
            "BEFORE_HEADER",
            "AFTER_FREE_LIST_WRITE",
            "BEFORE_WAL_FINISH",
            "BEFORE_MOVING_RECOVERY",
            "BEFORE_DELETING_CHECKPOINT_WAL",
            "BEFORE_HEADER_NON_FATAL",
            "IN_MEMORY_CHECKPOINT",
        ],
    ),
    (
        "debug_physical_table_scan_execution_strategy",
        "PhysicalTableScanExecutionStrategy",
        &[
            "DEFAULT",
            "TASK_EXECUTOR",
            "SYNCHRONOUS",
            "TASK_EXECUTOR_BUT_FORCE_SYNC_CHECKS",
        ],
    ),
    (
        "debug_verify_vector",
        "DebugVectorVerification",
        &[
            "NONE",
            "DICTIONARY_EXPRESSION",
            "DICTIONARY_OPERATOR",
            "CONSTANT_OPERATOR",
            "SEQUENCE_OPERATOR",
            "NESTED_SHUFFLE",
            "VARIANT_VECTOR",
        ],
    ),
    (
        "debug_window_mode",
        "WindowAggregationMode",
        &["WINDOW", "COMBINE", "SEPARATE"],
    ),
    (
        "deprecated_using_key_syntax",
        "DeprecatedUsingKeySyntax",
        &["DEFAULT", "UNION_AS_UNION_ALL"],
    ),
    (
        "explain_output",
        "ExplainOutputType",
        &["ALL", "OPTIMIZED_ONLY", "PHYSICAL_ONLY"],
    ),
    (
        "force_bitpacking_mode",
        "BitpackingMode",
        &[
            "INVALID",
            "AUTO",
            "CONSTANT",
            "CONSTANT_DELTA",
            "DELTA_FOR",
            "FOR",
        ],
    ),
    (
        "lambda_syntax",
        "LambdaSyntax",
        &["DEFAULT", "ENABLE_SINGLE_ARROW", "DISABLE_SINGLE_ARROW"],
    ),
    ("pin_threads", "ThreadPinMode", &["OFF", "ON", "AUTO"]),
    (
        "storage_block_prefetch",
        "StorageBlockPrefetch",
        &["REMOTE_ONLY", "NEVER", "ALWAYS_PREFETCH", "DEBUG_FORCE_ALWAYS"],
    ),
    (
        "validate_external_file_cache",
        "CacheValidationMode",
        &["VALIDATE_ALL", "VALIDATE_REMOTE", "NO_VALIDATION"],
    ),
];

/// `internal_optimizer_types` (DuckDB's `OptimizerTypeFromString`).
const OPTIMIZER_NAMES: [&str; 34] = [
    "expression_rewriter",
    "filter_pullup",
    "filter_pushdown",
    "empty_result_pullup",
    "cte_filter_pusher",
    "regex_range",
    "in_clause",
    "join_order",
    "deliminator",
    "unnest_rewriter",
    "unused_columns",
    "statistics_propagation",
    "common_subexpressions",
    "common_aggregate",
    "column_lifetime",
    "limit_pushdown",
    "row_group_pruner",
    "top_n",
    "top_n_window_elimination",
    "build_side_probe_side",
    "compressed_materialization",
    "duplicate_groups",
    "reorder_filter",
    "sampling_pushdown",
    "join_filter_pushdown",
    "extension",
    "materialized_cte",
    "sum_rewriter",
    "late_materialization",
    "cte_inlining",
    "common_subplan",
    "join_elimination",
    "window_self_join",
    "",
];

/// The comma-separated entries of a list option, lowercased and trimmed (empty ones
/// skipped), as DuckDB's `StringUtil::Split` / `Lower` / `Trim` loops read them.
fn list_entries(value: &Value) -> Result<Vec<String>> {
    Ok(value
        .to_varchar()?
        .split(',')
        .map(|entry| entry.trim().to_lowercase())
        .filter(|entry| !entry.is_empty())
        .collect())
}

/// The checks of the options' `OnSet` / `SetGlobal` / `SetLocal` callbacks on the
/// value (DuckDB's `custom_settings.cpp`).
fn check_value(option: &str, value: &Value) -> Result<()> {
    if let Some((_, enum_name, values)) = ENUM_SETTINGS.iter().find(|(name, _, _)| *name == option) {
        if value.is_null() {
            return Err(Error::invalid_input(format!("{option} setting cannot be NULL")));
        }
        return check_enum_value(value, enum_name, values);
    }
    match option {
        "threads" => {
            let threads = value.default_cast_as(&LogicalType::BIGINT)?;
            if threads.as_i128().is_some_and(|threads| threads < 1) {
                return Err(Error::new(ErrorKind::Syntax, "Must have at least 1 thread!"));
            }
        }
        "disabled_optimizers" => {
            let names: Vec<String> = OPTIMIZER_NAMES
                .iter()
                .filter(|name| !name.is_empty())
                .map(|&name| name.to_owned())
                .collect();
            for entry in list_entries(value)? {
                if !names.contains(&entry) {
                    return Err(Error::parser(format!(
                        "Optimizer type \"{entry}\" not recognized\n{}",
                        rustdb_common::string_util::candidates_error_message(
                            &names,
                            &entry,
                            "Candidate optimizers"
                        )
                    )));
                }
            }
        }
        "block_allocator_memory" => {
            let input = value.to_varchar()?;
            if let Some(percentage) = input.strip_suffix('%') {
                let valid = percentage
                    .trim()
                    .parse::<f64>()
                    .is_ok_and(|percentage| (0.0..=100.0).contains(&percentage));
                if !valid {
                    return Err(Error::invalid_input(format!(
                        "Unable to parse valid percentage (input: {input})"
                    )));
                }
            } else {
                rustdb_buffer::parse_memory_limit(&input)?;
            }
        }
        "max_memory" => {
            rustdb_buffer::parse_memory_limit(&value.to_varchar()?)?;
        }
        "disabled_compression_methods" => {
            use rustdb_storage::compression::CompressionType as C;
            for entry in list_entries(value)? {
                if entry == "none" {
                    break;
                }
                let compression = C::from_enum_name(&entry).ok_or_else(|| {
                    let values: Vec<&str> =
                        C::ALL.iter().map(|compression| compression.enum_name()).collect();
                    enum_value_error(&entry, "CompressionType", &values)
                })?;
                if matches!(compression, C::Auto | C::Constant | C::Empty | C::Uncompressed) {
                    return Err(Error::invalid_input(format!(
                        "Compression method {entry} cannot be disabled"
                    )));
                }
            }
        }
        "http_logging_output" => {
            return Err(Error::not_implemented(
                "This setting is deprecated and can no longer be used. Check out the DuckDB docs on logging for \
                 more information",
            ));
        }
        _ => {}
    }
    Ok(())
}

impl Settings {
    /// `DBConfig::CheckLock`: with `lock_configuration` set only `schema`, `search_path`
    /// and the `allowed_configs` options can change.
    pub(crate) fn check_lock(&self, name: &str) -> Result<()> {
        let locked = self
            .other
            .get("lock_configuration")
            .and_then(|value| value.default_cast_as(&LogicalType::BOOLEAN).ok())
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        if !locked || name.eq_ignore_ascii_case("schema") || name.eq_ignore_ascii_case("search_path") {
            return Ok(());
        }
        if !self.allowed_configs.is_empty() {
            let canonical = option_by_name(name);
            if canonical.is_some_and(|canonical| self.allowed_configs.contains(&canonical)) {
                return Ok(());
            }
        }
        Err(Error::invalid_input(format!(
            "Cannot change configuration option \"{name}\" - the configuration has been locked"
        )))
    }

    /// The `OnSet` / `SetGlobal` checks of options that cannot change while the database
    /// is running.
    pub(crate) fn check_running_set(name: &str, value: &Value) -> Result<()> {
        let enabled = || {
            value
                .default_cast_as(&LogicalType::BOOLEAN)
                .ok()
                .and_then(|value| value.as_bool())
                .unwrap_or(false)
        };
        match option_by_name(name).unwrap_or_default() {
            option @ ("allowed_directories" | "allowed_paths")
                if !rustdb_function::file_system::external_access_enabled() =>
            {
                Err(Error::invalid_input(format!(
                    "Cannot change {option} when enable_external_access is disabled"
                )))
            }
            "access_mode" => Err(Error::invalid_input(
                "Cannot change access_mode setting while database is running - it must be set when opening or \
                 attaching the database",
            )),
            option @ ("custom_user_agent"
            | "disable_database_invalidation"
            | "duckdb_api"
            | "vacuum_rebuild_indexes") => Err(Error::invalid_input(format!(
                "Cannot change {option} setting while database is running"
            ))),
            option @ ("allow_community_extensions"
            | "allow_unsigned_extensions"
            | "allow_unredacted_secrets")
                if enabled() =>
            {
                Err(Error::invalid_input(format!(
                    "Cannot change {option} setting while database is running"
                )))
            }
            "enable_external_access" if enabled() => Err(Error::invalid_input(
                "Cannot enable external access while database is running",
            )),
            option @ ("allow_community_extensions" | "allow_unsigned_extensions") if enabled() => {
                Err(Error::invalid_input(format!(
                    "Cannot change {option} setting while database is running"
                )))
            }
            _ => Ok(()),
        }
    }

    /// The `ResetGlobal` / `ResetLocal` checks of options that cannot be reset.
    pub(crate) fn check_running_reset(name: &str) -> Result<()> {
        match option_by_name(name).unwrap_or_default() {
            option @ ("allowed_directories" | "allowed_paths")
                if !rustdb_function::file_system::external_access_enabled() =>
            {
                Err(Error::invalid_input(format!(
                    "Cannot change {option} when enable_external_access is disabled"
                )))
            }
            "custom_user_agent" => Err(Error::invalid_input(
                "Cannot change custom_user_agent setting while database is running",
            )),
            "http_logging_output" => Err(Error::not_implemented(
                "This setting is deprecated and can no longer be used. Check out the DuckDB docs on logging for \
                 more information",
            )),
            _ => Ok(()),
        }
    }

    /// `AllowedConfigsSetting::SetGlobal` with `DBConfig::AddAllowedConfig`.
    fn set_allowed_configs(&mut self, value: &Value) -> Result<()> {
        use rustdb_function::extension_entries::{EXTENSION_SETTINGS, find_extension};
        let mut allowed = Vec::new();
        for entry in value.children().unwrap_or_default() {
            let name = entry.to_varchar()?;
            if name.is_empty() {
                return Err(Error::invalid_input(
                    "Cannot provide an empty string for allowed_configs",
                ));
            }
            if name.eq_ignore_ascii_case("allowed_configs") || name.eq_ignore_ascii_case("lock_configuration")
            {
                return Err(Error::invalid_input(format!(
                    "Cannot include '{name}' in allowed_configs"
                )));
            }
            if let Some(option) = option_by_name(&name) {
                allowed.push(option);
            } else if find_extension(&name, EXTENSION_SETTINGS).is_none() {
                return Err(Error::invalid_input(format!(
                    "Unknown configuration option '{name}' in allowed_configs"
                )));
            }
        }
        self.allowed_configs = allowed;
        Ok(())
    }

    /// The options set and not reset since, by canonical name.
    pub fn assigned(&self) -> impl Iterator<Item = (&'static str, &Value)> {
        self.assigned.iter().map(|(name, value)| (*name, value))
    }

    /// The options of the parser (DuckDB's `ClientContext::GetParserOptions`).
    pub(crate) fn parser_options(&self) -> rustdb_parser::ParserOptions {
        let flag = |option: &str, default: bool| {
            self.assigned
                .iter()
                .find(|(name, _)| *name == option)
                .map_or(default, |(_, value)| {
                    value
                        .default_cast_as(&LogicalType::BOOLEAN)
                        .ok()
                        .and_then(|value| value.as_bool())
                        .unwrap_or(default)
                })
        };
        rustdb_parser::ParserOptions {
            preserve_identifier_case: flag("preserve_identifier_case", true),
            integer_division: flag("integer_division", false),
            max_expression_depth: self.planner.max_expression_depth,
        }
    }

    /// `allow_parser_override_extension` (DuckDB's `AllowParserOverride`).
    pub(crate) fn parser_override(&self) -> ParserOverride {
        let mode = self
            .assigned
            .iter()
            .find(|(name, _)| *name == "allow_parser_override_extension")
            .and_then(|(_, value)| value.to_varchar().ok())
            .unwrap_or_default();
        if mode.eq_ignore_ascii_case("strict") {
            ParserOverride::Strict
        } else if mode.eq_ignore_ascii_case("fallback") {
            ParserOverride::Fallback
        } else {
            ParserOverride::Default
        }
    }

    /// Whether boolean option `option` was set to true.
    fn enabled(&self, option: &str) -> bool {
        self.assigned.iter().any(|(name, value)| {
            *name == option
                && value
                    .to_varchar()
                    .is_ok_and(|text| text.eq_ignore_ascii_case("true") || text == "1")
        })
    }

    /// `SET name = value`.
    pub fn set(&mut self, name: &str, value: &Value) -> Result<()> {
        let option = canonical_name(name).ok_or_else(|| unrecognized(name, |option| self.enabled(option)))?;
        check_user_adjustable(option)?;
        self.apply(option, value)?;
        match self.assigned.iter_mut().find(|(name, _)| *name == option) {
            Some((_, assigned)) => *assigned = value.clone(),
            None => self.assigned.push((option, value.clone())),
        }
        Ok(())
    }

    /// Applies the value of the option `option` (a canonical name).
    fn apply(&mut self, option: &'static str, value: &Value) -> Result<()> {
        let canonical = option_by_name(option).unwrap_or(option);
        check_value(canonical, value)?;
        if canonical == "allowed_configs" {
            self.set_allowed_configs(value)?;
        }
        match canonical {
            "enable_profiling" => return self.profiler.set_enable_profiling(value),
            "profile_output" => return self.profiler.set_save_location(value),
            "profiling_mode" => return self.profiler.set_mode(value),
            "profiling_coverage" => return self.profiler.set_coverage(value),
            "custom_profiling_settings" => {
                let disabled = self.optimizer_config().disabled;
                return self.profiler.set_custom_settings(value, &disabled);
            }
            _ => {}
        }
        match option {
            "default_order" => {
                let parameter = value.to_varchar()?.to_lowercase();
                self.planner.default_order = match parameter.as_str() {
                    "ascending" | "asc" => OrderType::Ascending,
                    "descending" | "desc" => OrderType::Descending,
                    _ => {
                        return Err(Error::invalid_input(format!(
                            "Unrecognized parameter for option DEFAULT_ORDER \"{parameter}\". Expected ASC or DESC."
                        )));
                    }
                };
            }
            "default_null_order" => {
                let parameter = value.to_varchar()?.to_lowercase();
                self.planner.default_null_order = match parameter.as_str() {
                    "nulls_first" | "nulls first" | "null first" | "first" => DefaultNullOrder::NullsFirst,
                    "nulls_last" | "nulls last" | "null last" | "last" => DefaultNullOrder::NullsLast,
                    "nulls_first_on_asc_last_on_desc" | "sqlite" | "mysql" => {
                        DefaultNullOrder::NullsFirstOnAscLastOnDesc
                    }
                    "nulls_last_on_asc_first_on_desc" | "postgres" => {
                        DefaultNullOrder::NullsLastOnAscFirstOnDesc
                    }
                    _ => {
                        return Err(Error::new(
                            ErrorKind::Parser,
                            format!(
                                "Unrecognized parameter for option NULL_ORDER \"{parameter}\", expected either NULLS \
                                 FIRST, NULLS LAST, SQLite, MySQL or Postgres"
                            ),
                        ));
                    }
                };
            }
            "max_expression_depth" => {
                let depth = value.default_cast_as(&LogicalType::UBIGINT)?;
                self.planner.max_expression_depth = depth
                    .as_i128()
                    .and_then(|depth| u64::try_from(depth).ok())
                    .ok_or_else(|| Error::internal("max_expression_depth is not a UBIGINT"))?;
            }
            "catalog_error_max_schemas" => {
                let count = value.default_cast_as(&LogicalType::UBIGINT)?;
                self.planner.catalog_error_max_schemas = count
                    .as_i128()
                    .and_then(|count| usize::try_from(count).ok())
                    .ok_or_else(|| Error::internal("catalog_error_max_schemas is not a UBIGINT"))?;
            }
            "default_collation" => {
                let collation = value.to_varchar()?.to_lowercase();
                // `DefaultCollationSetting::OnSet` binds a dummy collation.
                rustdb_planner::collation_binding::test_collation(&collation)?;
                self.planner.default_collation = collation;
            }
            "pivot_filter_threshold" | "pivot_limit" => {
                let count = value.default_cast_as(&LogicalType::UBIGINT)?;
                let count = count
                    .as_i128()
                    .and_then(|count| u64::try_from(count).ok())
                    .ok_or_else(|| Error::internal("pivot setting is not a UBIGINT"))?;
                if option == "pivot_limit" {
                    self.planner.pivot_limit = count;
                } else {
                    self.planner.pivot_filter_threshold = count;
                }
            }
            "order_by_non_integer_literal" => {
                let enabled = value.default_cast_as(&LogicalType::BOOLEAN)?;
                self.planner.order_by_non_integer_literal = enabled.as_bool().unwrap_or(false);
            }
            "scalar_subquery_error_on_multiple_rows" => {
                let enabled = value.default_cast_as(&LogicalType::BOOLEAN)?;
                self.planner.scalar_subquery_error_on_multiple_rows = enabled.as_bool().unwrap_or(true);
            }
            "enable_view_dependencies" | "enable_macro_dependencies" | "allow_unredacted_secrets" => {
                let enabled = value
                    .default_cast_as(&LogicalType::BOOLEAN)?
                    .as_bool()
                    .unwrap_or(false);
                match option {
                    "enable_view_dependencies" => self.planner.enable_view_dependencies = enabled,
                    "enable_macro_dependencies" => self.planner.enable_macro_dependencies = enabled,
                    _ => self.planner.allow_unredacted_secrets = enabled,
                }
                self.other.insert(option, value.clone());
            }
            "perfect_ht_threshold" => {
                // DuckDB's `PerfectHtThresholdSetting::OnSet`.
                let bits = value
                    .default_cast_as(&LogicalType::UBIGINT)?
                    .as_i128()
                    .unwrap_or(0);
                if bits > 32 {
                    return Err(Error::new(
                        ErrorKind::Parser,
                        "Perfect HT threshold out of range: should be within range 0 - 32",
                    ));
                }
                self.other.insert(option, value.clone());
            }
            "disabled_optimizers" => {
                OptimizerType::parse_list(&value.to_varchar()?)?;
                self.other.insert(option, value.clone());
            }
            "explain_output" => {
                let parameter = value.to_varchar()?;
                ExplainOutput::parse(&parameter)?;
                self.other
                    .insert(option, Value::varchar(parameter.to_ascii_lowercase()));
            }
            other => {
                self.other.insert(other, value.clone());
            }
        }
        Ok(())
    }

    /// The settings that steer the optimizer.
    pub fn optimizer_config(&self) -> OptimizerConfig {
        let boolean = |name: &str| {
            self.other
                .get(name)
                .and_then(|value| value.default_cast_as(&LogicalType::BOOLEAN).ok())
                .and_then(|value| value.as_bool())
                .unwrap_or(false)
        };
        OptimizerConfig {
            enabled: !self.optimizer_disabled,
            disabled: self
                .other
                .get("disabled_optimizers")
                .and_then(|value| value.to_varchar().ok())
                .and_then(|list| OptimizerType::parse_list(&list).ok())
                .unwrap_or_default(),
            force_no_cross_product: boolean("debug_force_no_cross_product"),
            prefer_range_joins: boolean("prefer_range_joins"),
            preserve_insertion_order: self
                .other
                .get("preserve_insertion_order")
                .and_then(|value| value.default_cast_as(&LogicalType::BOOLEAN).ok())
                .and_then(|value| value.as_bool())
                .unwrap_or(true),
            late_materialization_max_rows: self
                .other
                .get("late_materialization_max_rows")
                .and_then(|value| value.default_cast_as(&LogicalType::UBIGINT).ok())
                .and_then(|value| value.as_i128())
                .and_then(|value| u64::try_from(value).ok())
                .unwrap_or(50),
        }
    }

    /// The `explain_output` setting.
    pub fn explain_output(&self) -> ExplainOutput {
        self.other
            .get("explain_output")
            .and_then(|value| value.to_varchar().ok())
            .and_then(|value| ExplainOutput::parse(&value).ok())
            .unwrap_or_default()
    }

    /// `index_scan_max_count` and `index_scan_percentage`: an index scan is used when at
    /// most `max(count, percentage * rows)` rows match.
    pub fn index_scan_thresholds(&self) -> (u64, f64) {
        let max_count = self
            .other
            .get("index_scan_max_count")
            .and_then(|value| value.default_cast_as(&LogicalType::UBIGINT).ok())
            .and_then(|value| value.as_i128())
            .and_then(|value| u64::try_from(value).ok())
            .unwrap_or(rustdb_execution::operators::DEFAULT_INDEX_SCAN_MAX_COUNT);
        let percentage = self
            .other
            .get("index_scan_percentage")
            .and_then(|value| value.default_cast_as(&LogicalType::DOUBLE).ok())
            .and_then(|value| value.as_f64())
            .unwrap_or(rustdb_execution::operators::DEFAULT_INDEX_SCAN_PERCENTAGE);
        (max_count, percentage)
    }

    /// The settings that steer physical join planning.
    pub fn join_plan_settings(&self) -> JoinPlanSettings {
        let defaults = JoinPlanSettings::default();
        let unsigned = |name: &str, default: u64| {
            self.other
                .get(name)
                .and_then(|value| value.default_cast_as(&LogicalType::UBIGINT).ok())
                .and_then(|value| value.as_i128())
                .and_then(|value| u64::try_from(value).ok())
                .unwrap_or(default)
        };
        let boolean = |name: &str, default: bool| {
            self.other
                .get(name)
                .and_then(|value| value.default_cast_as(&LogicalType::BOOLEAN).ok())
                .and_then(|value| value.as_bool())
                .unwrap_or(default)
        };
        JoinPlanSettings {
            nested_loop_join_threshold: unsigned(
                "nested_loop_join_threshold",
                defaults.nested_loop_join_threshold,
            ),
            merge_join_threshold: unsigned("merge_join_threshold", defaults.merge_join_threshold),
            prefer_range_joins: boolean("prefer_range_joins", defaults.prefer_range_joins),
            scalar_subquery_error_on_multiple_rows: self.planner.scalar_subquery_error_on_multiple_rows,
            asof_loop_join_threshold: unsigned("asof_loop_join_threshold", defaults.asof_loop_join_threshold),
            dynamic_or_filter_threshold: unsigned(
                "dynamic_or_filter_threshold",
                defaults.dynamic_or_filter_threshold,
            ),
            perfect_ht_threshold: unsigned("perfect_ht_threshold", defaults.perfect_ht_threshold),
        }
    }

    /// `RESET name`.
    pub fn reset(&mut self, name: &str) -> Result<()> {
        let option = canonical_name(name).ok_or_else(|| unrecognized(name, |option| self.enabled(option)))?;
        check_user_adjustable(option)?;
        self.assigned.retain(|(name, _)| *name != option);
        let defaults = PlannerConfig::default();
        match option_by_name(option).unwrap_or(option) {
            "enable_profiling" => self.profiler.reset_enable_profiling(),
            "profile_output" => self.profiler.reset_save_location(),
            "profiling_mode" => self.profiler.reset_mode(),
            "profiling_coverage" => self.profiler.reset_coverage(),
            "custom_profiling_settings" => self.profiler.reset_custom_settings(),
            _ => {}
        }
        match option {
            "default_order" => self.planner.default_order = defaults.default_order,
            "default_null_order" => self.planner.default_null_order = defaults.default_null_order,
            "max_expression_depth" => self.planner.max_expression_depth = defaults.max_expression_depth,
            "catalog_error_max_schemas" => {
                self.planner.catalog_error_max_schemas = defaults.catalog_error_max_schemas;
            }
            "order_by_non_integer_literal" => {
                self.planner.order_by_non_integer_literal = defaults.order_by_non_integer_literal;
            }
            "scalar_subquery_error_on_multiple_rows" => {
                self.planner.scalar_subquery_error_on_multiple_rows =
                    defaults.scalar_subquery_error_on_multiple_rows;
            }
            "default_collation" => self.planner.default_collation = defaults.default_collation,
            "pivot_limit" => self.planner.pivot_limit = defaults.pivot_limit,
            "enable_view_dependencies" => {
                self.planner.enable_view_dependencies = defaults.enable_view_dependencies;
                self.other.remove(option);
            }
            "enable_macro_dependencies" => {
                self.planner.enable_macro_dependencies = defaults.enable_macro_dependencies;
                self.other.remove(option);
            }
            "allow_unredacted_secrets" => {
                self.planner.allow_unredacted_secrets = defaults.allow_unredacted_secrets;
                self.other.remove(option);
            }
            "pivot_filter_threshold" => self.planner.pivot_filter_threshold = defaults.pivot_filter_threshold,
            other => {
                self.other.remove(other);
            }
        }
        Ok(())
    }
}

/// The configuration a statement sees (the values `current_setting` and
/// `duckdb_settings()` report): the connection's options, its search path and the
/// database's limits.
#[derive(Debug)]
pub(crate) struct SettingsSnapshot {
    pub settings: Settings,
    /// The log manager's configuration, which the logging options read.
    pub logging: crate::logging::LogConfig,
    /// `search_path`, rendered like `CatalogSearchPath::SetPathsToString`.
    pub search_path: String,
    /// The schema unqualified names resolve in (`schema`).
    pub schema: String,
    pub memory_limit: u64,
    pub temp_directory: String,
    pub max_swap: Option<u64>,
    pub threads: u64,
}

/// The SQL type an option's value has (its `input_type`).
fn input_type(definition: &SettingDefinition) -> LogicalType {
    match definition.input_type {
        "BOOLEAN" => LogicalType::BOOLEAN,
        "BIGINT" => LogicalType::BIGINT,
        "UBIGINT" => LogicalType::UBIGINT,
        "INTEGER" => LogicalType::INTEGER,
        "DOUBLE" => LogicalType::DOUBLE,
        "FLOAT" => LogicalType::FLOAT,
        "VARCHAR[]" => LogicalType::list(LogicalType::VARCHAR),
        _ => LogicalType::VARCHAR,
    }
}

/// Byte sizes are shown normalised (`1GB` reads back as `953.6 MiB`).
fn normalized_bytes(value: &Value) -> Result<Value> {
    let limit = rustdb_buffer::parse_memory_limit(&value.to_varchar()?)?;
    Ok(Value::varchar(rustdb_buffer::format_bytes(limit)))
}

impl SettingsSnapshot {
    /// The value assigned to `name` (or an alias of it) by `SET`, if any.
    fn assigned(&self, names: &[&str]) -> Option<&Value> {
        self.settings
            .assigned
            .iter()
            .find(|(option, _)| names.iter().any(|name| option.eq_ignore_ascii_case(name)))
            .map(|(_, value)| value)
    }

    /// The current value of `definition` (`ConfigurationOption::get_setting`).
    fn value(&self, definition: &SettingDefinition) -> Result<Value> {
        let ty = input_type(definition);
        let mut names = vec![definition.name];
        names.extend(definition.aliases.iter().copied());
        // Options that are aliases of another share its value.
        if let Some(primary) = SETTINGS
            .iter()
            .find(|other| other.aliases.contains(&definition.name))
        {
            names.push(primary.name);
        }
        let assigned = self.assigned(&names);
        if let Some(value) = crate::logging::setting_value(&self.logging, definition.name) {
            return Ok(value);
        }
        let profiler = &self.settings.profiler;
        let value = match definition.name {
            "enable_profiling" => profiler.enable_profiling_value(),
            "profile_output" | "profiling_output" => Value::varchar(&profiler.save_location),
            "profiling_mode" => profiler.mode_value(),
            "profiling_coverage" => profiler.coverage_value(),
            "custom_profiling_settings" => profiler.custom_settings_value(),
            "default_null_order" | "null_order" => {
                Value::varchar(match self.settings.planner.default_null_order {
                    DefaultNullOrder::NullsFirst => "NULLS_FIRST",
                    DefaultNullOrder::NullsLast => "NULLS_LAST",
                    DefaultNullOrder::NullsFirstOnAscLastOnDesc => "NULLS_FIRST_ON_ASC_LAST_ON_DESC",
                    DefaultNullOrder::NullsLastOnAscFirstOnDesc => "NULLS_LAST_ON_ASC_FIRST_ON_DESC",
                })
            }
            // The default reads `ASCENDING`; assigned values are stored as `ASC`/`DESC`.
            "default_order" => {
                Value::varchar(match (self.settings.planner.default_order, assigned.is_some()) {
                    (OrderType::Descending, true) => "DESC",
                    (OrderType::Descending, false) => "DESCENDING",
                    (_, true) => "ASC",
                    (_, false) => "ASCENDING",
                })
            }
            "search_path" => Value::varchar(&self.search_path),
            "schema" => Value::varchar(&self.schema),
            "threads" | "worker_threads" => match assigned {
                Some(value) => value.default_cast_as(&ty)?,
                None => Value::bigint(i64::try_from(self.threads).unwrap_or(i64::MAX)),
            },
            "memory_limit" | "max_memory" => match assigned {
                Some(value) => normalized_bytes(value)?,
                None => Value::varchar(rustdb_buffer::format_bytes(self.memory_limit)),
            },
            "max_temp_directory_size" => match (assigned, self.max_swap) {
                (Some(value), _) => normalized_bytes(value)?,
                (None, Some(limit)) => Value::varchar(rustdb_buffer::format_bytes(limit)),
                (None, None) => Value::varchar("90% of available disk space"),
            },
            "temp_directory" => match assigned {
                Some(value) => value.default_cast_as(&ty)?,
                None => Value::varchar(&self.temp_directory),
            },
            _ => match (assigned, definition.default_value) {
                (Some(value), _) => value.default_cast_as(&ty)?,
                (None, Some(default)) => Value::varchar(default).default_cast_as(&ty)?,
                (None, None) => Value::null(ty),
            },
        };
        Ok(value)
    }
}

impl rustdb_function::SettingsSource for SettingsSnapshot {
    fn current_setting(&self, name: &str) -> Result<Value> {
        let definition = SETTINGS
            .iter()
            .find(|definition| definition.name.eq_ignore_ascii_case(name))
            .ok_or_else(|| {
                unrecognized(name, |option| {
                    self.current_setting(option)
                        .ok()
                        .and_then(|value| value.as_bool())
                        .unwrap_or(false)
                })
            })?;
        self.value(definition)
    }

    fn settings(&self) -> Vec<rustdb_function::SettingInfo> {
        SETTINGS
            .iter()
            .map(|definition| rustdb_function::SettingInfo {
                name: definition.name.to_owned(),
                value: self
                    .value(definition)
                    .and_then(|value| {
                        if value.is_null() {
                            Ok(String::new())
                        } else {
                            value.to_varchar()
                        }
                    })
                    .unwrap_or_default(),
                description: definition.description.to_owned(),
                input_type: definition.input_type.to_owned(),
                global: definition.global,
                aliases: definition
                    .aliases
                    .iter()
                    .map(|alias| (*alias).to_owned())
                    .collect(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_options_suggest_similar_names() {
        let mut settings = Settings::default();
        let error = settings.set("foo_bar", &Value::integer(1)).unwrap_err();
        assert_eq!(
            error.to_string(),
            "Catalog Error: unrecognized configuration parameter \"foo_bar\"\n\nDid you mean: \"force_bitpacking_mode\""
        );
    }

    #[test]
    fn planner_options_are_applied() {
        let mut settings = Settings::default();
        settings.set("default_order", &Value::varchar("DESC")).unwrap();
        assert_eq!(settings.planner.default_order, OrderType::Descending);
        settings.set("null_order", &Value::varchar("postgres")).unwrap();
        assert_eq!(
            settings.planner.default_null_order,
            DefaultNullOrder::NullsLastOnAscFirstOnDesc
        );
        let error = settings
            .set("max_expression_depth", &Value::varchar("abc"))
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "Invalid Input Error: Failed to cast value: Could not convert string 'abc' to UINT64"
        );
        settings.reset("default_order").unwrap();
        assert_eq!(settings.planner.default_order, OrderType::Ascending);
    }
}
