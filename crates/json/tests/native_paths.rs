//! Real JSON reader binding/scanning through Unicode paths and hive metadata.
use roastedduck_catalog::{Catalog, TransactionManager};
use roastedduck_common::{ByteString, Error, Result};
use roastedduck_function::{FunctionClient, TableFunctionBindInput, TableScanState, cast::CastFunctionSet};
use roastedduck_planner::{ClientContext, Expression, ExpressionEvaluator, PlannerConfig};
use roastedduck_types::{DataChunk, Value};
use std::sync::Arc;
#[derive(Debug)]
struct UnusedEvaluator;
impl ExpressionEvaluator for UnusedEvaluator {
    fn evaluate_scalar(&self, _: &Expression) -> Result<Value> {
        Err(Error::internal("completion unexpectedly evaluated SQL"))
    }
}
#[test]
fn unicode_json_path_bind_scan_and_filename() -> Result<()> {
    let manager = TransactionManager::default();
    let context = ClientContext {
        catalogs: vec![Arc::new(Catalog::new("memory")?)],
        search_path: Vec::new(),
        raw_search_path: Vec::new(),
        has_default_database: true,
        view_search_path: Vec::new(),
        view_default_schema_precedence: false,
        transaction: Arc::new(manager.begin()),
        casts: Arc::new(CastFunctionSet::new()),
        evaluator: Arc::new(UnusedEvaluator),
        config: PlannerConfig::default(),
        prepared_statements: roastedduck_common::CaseInsensitiveMap::default(),
        function_client: Arc::new(FunctionClient::default()),
        dependency_recorder: Arc::default(),
        database_state: None,
        user_variables: roastedduck_common::CaseInsensitiveMap::default(),
        replacement_scans: Vec::new(),
    };

    let root = std::path::PathBuf::from(std::env::var_os("TMPDIR").expect("external test TMPDIR"))
        .join(format!("json-native-path-{}", std::process::id()));
    let dir = root.join("界=東京");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("雪.json");
    std::fs::write(&path, "{\"値\":42}\n{\"値\":7}\n").unwrap();
    let sets = roastedduck_json::json_table_functions();
    let function = &sets
        .iter()
        .find(|set| set.name.as_ref() == "read_ndjson")
        .unwrap()
        .functions[0];
    let args = [Value::try_varchar(path.as_os_str().as_encoded_bytes())?];
    let params = [
        ("filename".into(), Value::boolean(true)),
        ("hive_partitioning".into(), Value::boolean(true)),
    ];
    let bound = function.bind.as_ref().unwrap()(&TableFunctionBindInput {
        inputs: &args,
        named_parameters: &params,
        input_table_types: &[],
        input_table_names: &[],
        binder: Some(&context),
    })?;
    assert_eq!(
        bound.names,
        vec![ByteString::from("値"), "filename".into(), "界".into()]
    );
    let mut state = TableScanState::default();
    let mut chunk = DataChunk::default();
    let mut values = Vec::new();
    loop {
        let more =
            function.scan.as_ref().unwrap()(bound.bind_data.as_deref(), &[0, 1, 2], &mut state, &mut chunk)?;
        for row in 0..chunk.len() {
            values.push(chunk.column(0).get_value(row).as_i64().unwrap());
            assert_eq!(
                chunk.column(1).get_value(row).as_bytes(),
                Some(path.as_os_str().as_encoded_bytes())
            );
            assert_eq!(chunk.column(2).get_value(row).as_bytes(), Some("東京".as_bytes()));
        }
        if !more {
            break;
        }
    }
    assert_eq!(values, vec![42, 7]);
    drop(state);
    drop(bound);
    let copies = roastedduck_json::json_copy_functions();
    let options = roastedduck_common::CaseInsensitiveMap::default();
    let input = roastedduck_function::copy::CopyFromBindInput {
        options: &options,
        file_path: path.as_os_str().as_encoded_bytes(),
        format: b"json",
        binder: Some(&context),
    };
    let copy_data = copies[0].copy_from_bind.as_ref().unwrap()(
        &input,
        &["値".into()],
        &[roastedduck_types::LogicalType::INTEGER],
    )?;
    let reader = copies[0].copy_from_function.as_ref().unwrap();
    let mut state = TableScanState::default();
    let mut copied = Vec::new();
    loop {
        let more = reader.scan.as_ref().unwrap()(Some(copy_data.as_ref()), &[0], &mut state, &mut chunk)?;
        for row in 0..chunk.len() {
            copied.push(chunk.column(0).get_value(row).as_i32().unwrap());
        }
        if !more {
            break;
        }
    }
    assert_eq!(copied, vec![42, 7]);
    drop(state);
    drop(copy_data);
    std::fs::remove_dir_all(root).unwrap();
    Ok(())
}
