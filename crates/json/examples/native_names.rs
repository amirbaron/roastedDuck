//! JSON function/cast binding and output against actual frozen C++ relation queries.
use roastedduck_common::{ByteString, Error, Result};
use roastedduck_function::{
    BindInput, FunctionClient, FunctionState,
    cast::{CastFunctionSet, CastParameters},
};
use roastedduck_types::{DataChunk, Value, Vector};
use std::io::{self, BufRead};
use std::sync::Arc;
fn decode(hex: &str) -> Result<Vec<u8>> {
    let (pairs, rest) = hex.as_bytes().as_chunks::<2>();
    if !rest.is_empty() {
        return Err(Error::invalid_input("odd hex length"));
    }
    pairs
        .iter()
        .map(
            |p| match (char::from(p[0]).to_digit(16), char::from(p[1]).to_digit(16)) {
                (Some(a), Some(b)) => Ok((a * 16 + b) as u8),
                _ => Err(Error::invalid_input("invalid hex")),
            },
        )
        .collect()
}
fn outcome(mode: &str, name: ByteString) -> Result<ByteString> {
    let mut value = Value::struct_value(vec![(name, Value::integer(1))]);
    if mode == "list" {
        value = Value::list(value.logical_type().clone(), vec![value]);
    }
    let ty = value.logical_type().clone();
    let mut source = Vector::flat(ty.clone(), 1);
    source.set_value(0, &value)?;
    let mut result = Vector::flat(roastedduck_json::json_type(), 1);
    if mode == "cast" {
        let mut casts = CastFunctionSet::new();
        roastedduck_json::register_json_casts(&mut casts);
        casts
            .get_cast_function(&ty, &roastedduck_json::json_type())?
            .execute(&source, &mut result, 1, &mut CastParameters::new())?;
    } else {
        let functions = roastedduck_json::json_scalar_functions();
        let mut function = functions
            .iter()
            .find(|set| set.name.as_bytes() == b"to_json")
            .ok_or_else(|| Error::internal("missing to_json"))?
            .functions[0]
            .clone();
        let client = Arc::new(FunctionClient::default());
        let types = [ty];
        let constants = [None];
        let bind = Arc::clone(
            function
                .bind
                .as_ref()
                .ok_or_else(|| Error::internal("missing JSON binder"))?,
        );
        let data = bind(
            &mut function,
            &BindInput {
                argument_types: &types,
                constants: &constants,
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
        (function.kernel)(&DataChunk::new(vec![source], 1), &mut state, &mut result)?;
    }
    result.get_value(0).to_varchar_bytes()
}
pub fn compare(line: &str) -> Result<()> {
    let f: Vec<_> = line.split('\t').collect();
    if f.len() != 4 {
        return Err(Error::invalid_input("invalid JSON native record"));
    }
    let result = outcome(f[0], decode(f[1])?.into());
    let (status, bytes) = match result {
        Ok(bytes) => ("ok", bytes),
        Err(error) => ("error", error.raw_message_bytes().into()),
    };
    if status != f[2] || bytes.as_bytes() != decode(f[3])? {
        return Err(Error::internal(format!(
            "JSON native outcome differs: {} name {} ({status}), bytes {:?}",
            f[0],
            f[1],
            bytes.as_bytes()
        )));
    }
    Ok(())
}
fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut count = 0;
    for line in io::stdin().lock().lines() {
        compare(&line?)?;
        count += 1;
    }
    println!("{count} C++ native JSON function/cast comparisons passed");
    Ok(())
}
