//! Real physical projection parameters and operator-log errors against C++ 1.5.6.
use roastedduck_common::{ByteString, Error, Result};
use roastedduck_execution::{
    PhysicalOperator,
    operators::{PhysicalDummyScan, PhysicalProjection},
};
use roastedduck_function::logging::operator_log_message;
use roastedduck_planner::{ColumnBinding, Expression};
use roastedduck_types::LogicalType;
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
pub fn compare(line: &str) -> Result<()> {
    let fields: Vec<_> = line.split('\t').collect();
    if fields.len() < 4 {
        return Err(Error::invalid_input("invalid physical oracle record"));
    }
    let name = ByteString::from(decode(fields[1])?);
    let expression = Expression::column_ref(name, LogicalType::INTEGER, ColumnBinding::new(0, 0), 0);
    let projection = PhysicalProjection::new(
        vec![LogicalType::INTEGER],
        vec![expression],
        Arc::new(PhysicalDummyScan::new(Vec::new())),
    );
    let parameters: Vec<_> = projection.params_to_string(7).into_iter().collect();
    match fields[0] {
        "params" => {
            let expected: Vec<(ByteString, ByteString)> = fields[2..]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|p| Ok((decode(p[0])?.into(), decode(p[1])?.into())))
                .collect::<Result<_>>()?;
            if parameters != expected {
                return Err(Error::internal("physical projection parameters differ from C++"));
            }
        }
        "log" => {
            let result = operator_log_message(
                "PROJECTION",
                &parameters,
                "Class",
                "Event",
                &[("rows", "1".to_owned())],
            );
            let (status, actual): (&str, ByteString) = match result {
                Ok(bytes) => ("ok", bytes),
                Err(error) => ("error", error.raw_message_bytes().into()),
            };
            if status != fields[2] || actual.as_bytes() != decode(fields[3])? {
                return Err(Error::internal("physical operator log outcome differs from C++"));
            }
        }
        _ => return Err(Error::invalid_input("unknown physical oracle kind")),
    }
    Ok(())
}
fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut count = 0;
    for line in io::stdin().lock().lines() {
        compare(&line?)?;
        count += 1;
    }
    println!("{count} C++ physical parameter/log comparisons passed");
    Ok(())
}
