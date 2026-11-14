use roastedduck_function::ExtensionOption;
use roastedduck_types::{LogicalType, Value};

/// Option names, descriptions, types and defaults from the pinned Azure source.
pub fn options() -> Vec<ExtensionOption> {
    [
        ("azure_storage_connection_string", "Azure connection string, used for authenticating and configuring azure requests", LogicalType::VARCHAR, Value::null(LogicalType::VARCHAR)),
        ("azure_account_name", "Azure account name, when set, the extension will attempt to automatically detect credentials", LogicalType::VARCHAR, Value::null(LogicalType::VARCHAR)),
        ("azure_credential_chain", "Ordered list of Azure credential providers, in string format separated by ';'. E.g. 'cli;workload_identity;managed_identity;env'", LogicalType::VARCHAR, Value::null(LogicalType::VARCHAR)),
        ("azure_endpoint", "Override the azure endpoint for when the Azure credential providers are used.", LogicalType::VARCHAR, Value::varchar("blob.core.windows.net")),
        ("azure_http_stats", "Include http info from the Azure Storage in the explain analyze statement.", LogicalType::BOOLEAN, Value::boolean(false)),
        ("azure_http_logging", "Enable Azure HTTP request logging into the DuckDB HTTP log. When true (default), logging follows DuckDB's HTTP log settings (see enable_logging('HTTP')). Set to false to disable Azure HTTP logging.", LogicalType::BOOLEAN, Value::boolean(true)),
        ("azure_http_logging_redact_query_params", "Semicolon-separated list of URL query parameter names to redact in HTTP logs. Defaults to 'sig'.", LogicalType::VARCHAR, Value::varchar("sig")),
        ("azure_http_logging_redact_headers", "Semicolon-separated list of header names to redact in HTTP logs (both request and response). Defaults to 'Authorization'.", LogicalType::VARCHAR, Value::varchar("Authorization")),
        ("azure_context_caching", "Enable/disable the caching of some context when performing queries. This cache is by default enable, and will for a given connection keep a local context when performing a query. If you suspect that the caching is causing some side effect you can try to disable it by setting this option to false.", LogicalType::BOOLEAN, Value::boolean(true)),
        ("azure_transport_option_type", "Underlying adapter to use with the Azure SDK. Read more about the adapter at https://github.com/Azure/azure-sdk-for-cpp/blob/main/doc/HttpTransportAdapter.md. Valid values are: default, curl", LogicalType::VARCHAR, Value::varchar("default")),
        ("azure_read_transfer_concurrency", "Maximum number of threads the Azure client can use for a single parallel read. If azure_read_transfer_chunk_size is less than azure_read_buffer_size then setting this > 1 will allow the Azure client to do concurrent requests to fill the buffer.", LogicalType::INTEGER, Value::integer(5)),
        ("azure_read_transfer_chunk_size", "Maximum size in bytes that the Azure client will read in a single request. It is recommended that this is a factor of azure_read_buffer_size.", LogicalType::BIGINT, Value::bigint(8 * 1024 * 1024)),
        ("azure_read_buffer_size", "Size of the read buffer. It is recommended that this is evenly divisible by azure_read_transfer_chunk_size.", LogicalType::UBIGINT, Value::ubigint(8 * 1024 * 1024)),
        ("azure_write_block_size", "Size in bytes of each block for Blob/DFS writes. 0 restores the default (8 MiB). Max 4000 MiB (Azure per-request limit). Azure hard-limits a blob to 50,000 blocks; increase this to write files larger than 50,000 × azure_write_block_size bytes.", LogicalType::UBIGINT, Value::ubigint(8 * 1024 * 1024)),
        ("azure_write_staged_blocks_per_commit", "Number of blocks staged before an intermediate CommitBlockList. 0 (default) disables intermediate commits; the full block list is committed on close. Non-zero values make partial writes visible sooner at the cost of extra commit RPCs. Does not affect the 50,000-block blob limit; increase azure_write_block_size for that.", LogicalType::UBIGINT, Value::ubigint(0)),
        ("azure_http_proxy", "Proxy to use when login & performing request to azure. By default it will use the HTTP_PROXY environment variable if set.", LogicalType::VARCHAR, std::env::var("HTTP_PROXY").map_or_else(|_| Value::null(LogicalType::VARCHAR), Value::varchar)),
        ("azure_proxy_user_name", "Http proxy user name if needed.", LogicalType::VARCHAR, Value::null(LogicalType::VARCHAR)),
        ("azure_proxy_password", "Http proxy password if needed.", LogicalType::VARCHAR, Value::null(LogicalType::VARCHAR)),
    ].into_iter().map(|(name, description, input_type, default_value)| ExtensionOption {
        name: name.into(), description: description.into(), input_type, default_value,
        set_function: None, global_by_default: false,
    }).collect()
}
