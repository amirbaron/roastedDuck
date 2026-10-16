//! `SET` / `RESET` of configuration options (DuckDB's `PhysicalSet` / `PhysicalReset`
//! and the option table of `config.cpp`). Options the binder reads are applied to the
//! connection's [`PlannerConfig`]; other known options are accepted and remembered.

use std::collections::HashMap;

use rustdb_ast::OrderType;
use rustdb_common::string_util::candidates_error_message;
use rustdb_common::{Error, ErrorKind, Result};
use rustdb_function::cast::ValueCast;
use rustdb_execution::JoinPlanSettings;
use rustdb_planner::{DefaultNullOrder, PlannerConfig};
use rustdb_types::{LogicalType, Value};

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

/// DuckDB's `Catalog::UnrecognizedConfigurationError`.
fn unrecognized(name: &str) -> Error {
    let names: Vec<String> = OPTION_NAMES.iter().map(|name| (*name).to_owned()).collect();
    Error::catalog(format!(
        "unrecognized configuration parameter \"{name}\"\n{}",
        candidates_error_message(&names, name, "Did you mean")
    ))
}

/// The connection's option values.
#[derive(Clone, Debug, Default)]
pub(crate) struct Settings {
    pub planner: PlannerConfig,
    /// Values of options the engine does not interpret yet.
    other: HashMap<&'static str, Value>,
    /// Every option set (and not reset since), in the order of the first `SET`.
    assigned: Vec<(&'static str, Value)>,
}

impl Settings {
    /// The options set and not reset since, by canonical name.
    pub fn assigned(&self) -> impl Iterator<Item = (&'static str, &Value)> {
        self.assigned.iter().map(|(name, value)| (*name, value))
    }

    /// `SET name = value`.
    pub fn set(&mut self, name: &str, value: &Value) -> Result<()> {
        let option = canonical_name(name).ok_or_else(|| unrecognized(name))?;
        self.apply(option, value)?;
        match self.assigned.iter_mut().find(|(name, _)| *name == option) {
            Some((_, assigned)) => *assigned = value.clone(),
            None => self.assigned.push((option, value.clone())),
        }
        Ok(())
    }

    /// Applies the value of the option `option` (a canonical name).
    fn apply(&mut self, option: &'static str, value: &Value) -> Result<()> {
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
                self.planner.default_collation = value.to_varchar()?.to_lowercase();
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
            "enable_view_dependencies" | "enable_macro_dependencies" => {
                let enabled = value
                    .default_cast_as(&LogicalType::BOOLEAN)?
                    .as_bool()
                    .unwrap_or(false);
                if option == "enable_view_dependencies" {
                    self.planner.enable_view_dependencies = enabled;
                } else {
                    self.planner.enable_macro_dependencies = enabled;
                }
                self.other.insert(option, value.clone());
            }
            other => {
                self.other.insert(other, value.clone());
            }
        }
        Ok(())
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
            nested_loop_join_threshold: unsigned("nested_loop_join_threshold", defaults.nested_loop_join_threshold),
            merge_join_threshold: unsigned("merge_join_threshold", defaults.merge_join_threshold),
            prefer_range_joins: boolean("prefer_range_joins", defaults.prefer_range_joins),
            scalar_subquery_error_on_multiple_rows: self.planner.scalar_subquery_error_on_multiple_rows,
        }
    }

    /// `RESET name`.
    pub fn reset(&mut self, name: &str) -> Result<()> {
        let option = canonical_name(name).ok_or_else(|| unrecognized(name))?;
        self.assigned.retain(|(name, _)| *name != option);
        let defaults = PlannerConfig::default();
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
                self.planner.scalar_subquery_error_on_multiple_rows = defaults.scalar_subquery_error_on_multiple_rows;
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
            "pivot_filter_threshold" => self.planner.pivot_filter_threshold = defaults.pivot_filter_threshold,
            other => {
                self.other.remove(other);
            }
        }
        Ok(())
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
