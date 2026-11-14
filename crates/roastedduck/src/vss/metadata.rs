//! Independent registration metadata from the pinned HNSW module.
//! These helpers do not register executable HNSW functions or fabricate results.

use roastedduck_common::ByteString;
use roastedduck_function::{ExtensionOption, PragmaFunction, PragmaFunctionSet};
use roastedduck_types::{LogicalType, Value};

/// VSS settings, with the pinned defaults, descriptions, and default SESSION scope.
pub(crate) fn options() -> [ExtensionOption; 2] {
    [
        ExtensionOption {
            name: "hnsw_enable_experimental_persistence".to_owned(),
            description: "experimental: enable creating HNSW indexes in persistent databases".to_owned(),
            input_type: LogicalType::BOOLEAN,
            default_value: Value::boolean(false),
            set_function: None,
            global_by_default: false,
        },
        ExtensionOption {
            name: "hnsw_ef_search".to_owned(),
            description: "experimental: override the ef_search parameter when scanning HNSW indexes"
                .to_owned(),
            input_type: LogicalType::BIGINT,
            // Upstream AddExtensionOption uses its default Value(), a SQL NULL.
            default_value: Value::null(LogicalType::SQLNULL),
            set_function: None,
            global_by_default: false,
        },
    ]
}

/// Signature only: the runtime must supply actual index lookup and compaction.
pub(crate) fn compact_pragma() -> PragmaFunctionSet {
    PragmaFunctionSet::single(PragmaFunction::call(
        "hnsw_compact_index",
        vec![LogicalType::VARCHAR],
    ))
}

/// Output schema of `pragma_hnsw_index_info()`, in the pinned column order.
pub(crate) fn index_info_schema() -> (Vec<ByteString>, Vec<LogicalType>) {
    let columns = [
        ("catalog_name", LogicalType::VARCHAR),
        ("schema_name", LogicalType::VARCHAR),
        ("index_name", LogicalType::VARCHAR),
        ("table_name", LogicalType::VARCHAR),
        ("metric", LogicalType::VARCHAR),
        ("dimensions", LogicalType::BIGINT),
        ("count", LogicalType::BIGINT),
        ("capacity", LogicalType::BIGINT),
        ("approx_memory_usage", LogicalType::BIGINT),
        ("levels", LogicalType::BIGINT),
        (
            "levels_stats",
            LogicalType::list(LogicalType::struct_type(
                ["nodes", "edges", "max_edges", "allocated_bytes"]
                    .into_iter()
                    .map(|name| (name.into(), LogicalType::BIGINT))
                    .collect(),
            )),
        ),
    ];
    columns.into_iter().map(|(name, ty)| (name.into(), ty)).unzip()
}
