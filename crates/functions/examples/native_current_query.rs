//! Native parser source, connection state and actual `current_query` scalar kernel.
use roastedduck_common::{Error, Result};
use roastedduck_function::{BindInput, FunctionClient, FunctionState};
use roastedduck_types::{DataChunk, LogicalType, Value, Vector};
use std::{
    io::{self, BufRead},
    sync::Arc,
};
fn decode(h: &str) -> Result<Vec<u8>> {
    h.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|p| {
            u8::from_str_radix(
                std::str::from_utf8(p).map_err(|_| Error::invalid_input("bad hex"))?,
                16,
            )
            .map_err(|_| Error::invalid_input("bad hex"))
        })
        .collect()
}
fn execute(query: &[u8]) -> Result<Value> {
    let mut statements = roastedduck_parser::parse_bytes(query)
        .map_err(|error| error.with_query_context_bytes(query).rethrown())?;
    let statement = statements
        .pop()
        .ok_or_else(|| Error::internal("missing statement"))?;
    let client = Arc::new(FunctionClient::default());
    client.set_query(&statement.query);
    if client.query() != statement.query {
        return Err(Error::internal("query storage changed bytes"));
    }
    let mut function = roastedduck_functions::scalar::system::system_functions()[0].functions[0].clone();
    let bind = function
        .bind
        .clone()
        .ok_or_else(|| Error::internal("missing bind"))?;
    let data = bind(
        &mut function,
        &BindInput {
            argument_types: &[],
            constants: &[],
            aliases: &[],
            names: &[],
            client: &client,
            binder: None,
            arguments: None,
            aggregates: None,
            lambda: None,
        },
    )?;
    let mut state = FunctionState {
        bind_data: data,
        ..FunctionState::default()
    };
    let mut vector = Vector::constant_null(LogicalType::VARCHAR);
    (function.kernel)(&DataChunk::default(), &mut state, &mut vector)?;
    Ok(vector.get_value(0))
}
pub fn compare(line: &str) -> Result<()> {
    let fields: Vec<_> = line.split('\t').collect();
    if fields.len() != 2 {
        return Err(Error::invalid_input("bad record"));
    }
    let query = decode(fields[0])?;
    let actual = execute(&query);
    let details = match &actual {
        Ok(value) => format!("value {:?}", value.as_bytes()),
        Err(error) => format!("error {:?}", error.clone().into_json().to_bytes()),
    };
    let matched = match actual {
        Ok(value) => fields[1]
            .strip_prefix("OK:")
            .is_some_and(|h| decode(h).is_ok_and(|v| Some(v.as_slice()) == value.as_bytes())),
        Err(error) => fields[1]
            .strip_prefix("ERR:")
            .is_some_and(|h| decode(h).is_ok_and(|v| v == error.into_json().to_bytes())),
    };
    if !matched {
        return Err(Error::internal(format!(
            "current_query mismatch {} {details}",
            fields[0]
        )));
    }
    Ok(())
}
fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let (mut passed, mut failed) = (0, 0);
    for l in io::stdin().lock().lines() {
        match compare(&l?) {
            Ok(()) => passed += 1,
            Err(e) => {
                failed += 1;
                eprintln!("{e}");
            }
        }
    }
    println!("passed={passed} failed={failed}");
    if failed > 0 {
        return Err("current_query differs".into());
    }
    Ok(())
}
