//! DuckDB's `icu-current.cpp`: `get_current_time()` (`current_time`) and
//! `current_date()` / `today()` in the session's time zone.

use std::any::Any;
use std::sync::Arc;

use rustdb_common::{Error, Result};
use rustdb_function::{
    FunctionClient, FunctionData, FunctionState, ScalarFunction, ScalarFunctionSet, Stability,
};
use rustdb_types::{LogicalType, Time, TimeTz, Timestamp, Value, Vector};

use crate::icu_datefunc::BindData;
use crate::icu_makedate;
use crate::icu_timezone::to_time_tz;

/// The connection whose transaction and settings the functions read (DuckDB reads them
/// from the expression state's context).
#[derive(Debug)]
struct ClientData(Arc<FunctionClient>);

impl FunctionData for ClientData {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn client(state: &FunctionState) -> Result<Arc<FunctionClient>> {
    state
        .bind_data
        .as_ref()
        .and_then(|data| data.as_any().downcast_ref::<ClientData>())
        .map(|data| Arc::clone(&data.0))
        .ok_or_else(|| Error::internal("ICU current function without its connection"))
}

/// `GetTransactionTimestamp`.
fn transaction_timestamp(client: &FunctionClient) -> Timestamp {
    Timestamp(client.transaction_start_micros())
}

/// `CurrentTimeFunction`.
fn current_time(state: &FunctionState) -> Result<Value> {
    let client = client(state)?;
    let instant = transaction_timestamp(&client);
    let mut data = BindData::from_client(&client);
    let result_time = to_time_tz(&mut data.calendar, instant)?.unwrap_or_else(|| TimeTz::new(Time(0), 0));
    Ok(Value::time_tz(result_time))
}

/// `CurrentDateFunction`.
fn current_date(state: &FunctionState) -> Result<Value> {
    let client = client(state)?;
    let instant = transaction_timestamp(&client);
    Ok(Value::date(icu_makedate::to_date(&client, instant)?))
}

/// A zero-argument function consistent within a query, computed from the connection.
fn current_function(
    name: &str,
    result_type: LogicalType,
    value: fn(&FunctionState) -> Result<Value>,
) -> ScalarFunction {
    let mut function = ScalarFunction::new(name, vec![], result_type, move |_, state, result| {
        *result = Vector::constant(&value(state)?)?;
        Ok(())
    })
    .with_bind(|_, input| {
        Ok(Some(
            Arc::new(ClientData(Arc::clone(input.client))) as Arc<dyn FunctionData>
        ))
    });
    function.stability = Stability::ConsistentWithinQuery;
    function
}

/// `RegisterICUCurrentFunctions`: `get_current_time`, `current_date` and `today`.
pub(crate) fn current_functions() -> Vec<ScalarFunctionSet> {
    let mut current_time_set = ScalarFunctionSet::new("get_current_time");
    current_time_set.add(current_function(
        "get_current_time",
        LogicalType::TIME_TZ,
        current_time,
    ));

    let mut current_date_set = ScalarFunctionSet::new("current_date");
    current_date_set.add(current_function("current_date", LogicalType::DATE, current_date));

    let mut today = ScalarFunctionSet::new("today");
    today.add(current_function("today", LogicalType::DATE, current_date));
    vec![current_time_set, current_date_set, today]
}
