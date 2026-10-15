//! `get_current_timestamp`/`now`/`transaction_timestamp` (DuckDB's `current.cpp`): the
//! start time of the current transaction.

use std::any::Any;
use std::sync::Arc;

use rustdb_common::{Error, Result};
use rustdb_function::{FunctionClient, FunctionData, ScalarFunction, ScalarFunctionSet, Stability};
use rustdb_types::{LogicalType, Timestamp, Value, Vector};

#[derive(Debug)]
struct ClientData(Arc<FunctionClient>);

impl FunctionData for ClientData {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// `get_current_timestamp()`.
pub fn current_timestamp_functions() -> ScalarFunctionSet {
    let mut function = ScalarFunction::new(
        "get_current_timestamp",
        vec![],
        LogicalType::TIMESTAMP_TZ,
        |_, state, result| -> Result<()> {
            let client = state
                .bind_data
                .as_ref()
                .and_then(|data| data.as_any().downcast_ref::<ClientData>())
                .ok_or_else(|| Error::internal("get_current_timestamp without bind data"))?;
            let value = Value::timestamp_tz(Timestamp(client.0.transaction_start_micros()));
            *result = Vector::constant(&value)?;
            Ok(())
        },
    )
    .with_bind(|_, input| {
        Ok(Some(Arc::new(ClientData(Arc::clone(input.client))) as Arc<dyn FunctionData>))
    });
    function.stability = Stability::ConsistentWithinQuery;
    let mut set = ScalarFunctionSet::new("get_current_timestamp");
    set.add(function);
    set
}
