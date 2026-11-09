//! The public `sql_auto_complete` bind/scan path with real catalog entries.
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
fn suggestions(name: ByteString) -> Result<Vec<ByteString>> {
    let manager = TransactionManager::default();
    let context = ClientContext {
        catalogs: vec![Arc::new(Catalog::new(name)?)],
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
    let functions = roastedduck_autocomplete::autocomplete_table_functions(Arc::new(Vec::new));
    let function = &functions[0].functions[0];
    let input = [Value::varchar("USE ")];
    let parameters = [
        ("max_suggestion_count".into(), Value::ubigint(100)),
        ("max_exact_suggestion_count".into(), Value::ubigint(100)),
    ];
    let bind =
        function
            .bind
            .as_ref()
            .ok_or_else(|| Error::internal("missing completion binder"))?(&TableFunctionBindInput {
            inputs: &input,
            named_parameters: &parameters,
            input_table_types: &[],
            input_table_names: &[],
            binder: Some(&context),
        })?;
    let scan = function
        .scan
        .as_ref()
        .ok_or_else(|| Error::internal("missing completion scanner"))?;
    let mut state = TableScanState::default();
    let mut chunk = DataChunk::default();
    let mut values = Vec::new();
    loop {
        let more = scan(
            bind.bind_data.as_deref(),
            &[0, 1, 2, 3, 4],
            &mut state,
            &mut chunk,
        )?;
        for row in 0..chunk.len() {
            values.push(chunk.column(0).get_value(row).to_varchar_bytes()?);
        }
        if !more {
            break;
        }
    }
    Ok(values)
}
#[test]
fn public_completion_quotes_unicode_catalog_names() -> Result<()> {
    let values = suggestions("démo".into())?;
    assert!(
        values
            .iter()
            .any(|value| value.as_bytes() == "\"démo\".".as_bytes()),
        "{values:?}"
    );
    Ok(())
}
#[test]
fn public_completion_validates_raw_catalog_names_at_varchar_boundary() {
    let error = suggestions(b"raw\xff".as_slice().into()).unwrap_err();
    assert_eq!(
        error.raw_message_bytes(),
        b"Invalid unicode (byte sequence mismatch) detected in value construction"
    );
}
