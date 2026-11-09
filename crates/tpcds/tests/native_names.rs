//! Generator bind/scan with raw catalog/schema/suffix identities, at scale zero.
use roastedduck_buffer::{BufferManager, BufferManagerConfig, InMemoryBlockManager};
use roastedduck_catalog::{Catalog, CatalogType, TransactionManager};
use roastedduck_common::{ByteString, Result};
use roastedduck_execution::ScalarEvaluator;
use roastedduck_function::{FunctionClient, TableFunctionBindInput, TableScanState, cast::CastFunctionSet};
use roastedduck_planner::{ClientContext, PlannerConfig};
use roastedduck_storage::TableIoManager;
use roastedduck_types::{DataChunk, Value};
use std::sync::Arc;
#[test]
fn native_generator_names_survive_real_table_creation() -> Result<()> {
    for (catalog_name, schema, suffix) in [
        (
            ByteString::from(b"cat\xff"),
            ByteString::from(b"schema\xe9"),
            ByteString::from(b"_\xff"),
        ),
        ("cát".into(), "schéma".into(), "_é".into()),
    ] {
        let manager = TransactionManager::default();
        let transaction = Arc::new(manager.begin());
        let catalog = Arc::new(Catalog::new(catalog_name.clone())?);
        catalog.create_schema_committed(&schema)?;
        let buffer = BufferManager::new(BufferManagerConfig::default())?;
        catalog.set_io(Arc::new(TableIoManager::new(
            buffer,
            Arc::new(InMemoryBlockManager::default()),
        )))?;
        let context = ClientContext {
            catalogs: vec![Arc::clone(&catalog)],
            search_path: vec![(catalog_name.clone(), schema.clone())],
            raw_search_path: Vec::new(),
            has_default_database: true,
            view_search_path: Vec::new(),
            view_default_schema_precedence: false,
            transaction: Arc::clone(&transaction),
            casts: Arc::new(CastFunctionSet::new()),
            evaluator: Arc::new(ScalarEvaluator),
            config: PlannerConfig::default(),
            prepared_statements: roastedduck_common::CaseInsensitiveMap::default(),
            function_client: Arc::new(FunctionClient::default()),
            dependency_recorder: Arc::default(),
            database_state: None,
            user_variables: roastedduck_common::CaseInsensitiveMap::default(),
            replacement_scans: Vec::new(),
        };
        let functions = roastedduck_tpcds::tpcds_table_functions();
        let function = &functions[0].functions[0];
        let params = vec![
            ("sf".into(), Value::double(0.0)),
            (
                "catalog".into(),
                Value::bytes(roastedduck_types::LogicalType::VARCHAR, &catalog_name),
            ),
            (
                "schema".into(),
                Value::bytes(roastedduck_types::LogicalType::VARCHAR, &schema),
            ),
            (
                "suffix".into(),
                Value::bytes(roastedduck_types::LogicalType::VARCHAR, &suffix),
            ),
        ];
        let bound = function.bind.as_ref().unwrap()(&TableFunctionBindInput {
            inputs: &[],
            named_parameters: &params,
            input_table_types: &[],
            input_table_names: &[],
            binder: Some(&context),
        })?;
        assert_eq!(
            bound.bind_data.as_ref().unwrap().modified_database(),
            Some(catalog_name)
        );
        let mut chunk = DataChunk::default();
        let mut state = TableScanState::default();
        function.scan.as_ref().unwrap()(bound.bind_data.as_deref(), &[], &mut state, &mut chunk)?;
        let mut table_name = ByteString::from("call_center");
        table_name.push_bytes(&suffix);
        let entry = catalog
            .get_entry(&transaction, CatalogType::Table, &schema, &table_name)
            .unwrap();
        assert_eq!(entry.name(), &table_name);
    }
    Ok(())
}
