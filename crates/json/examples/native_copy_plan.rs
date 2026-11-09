//! Native JSON COPY projection matches the actual C++ AST construction.
use roastedduck_common::{ByteString, CaseInsensitiveMap, Error, Result, byte_format};
use roastedduck_function::copy::CopyPlanInput;
use roastedduck_types::{LogicalType, Value};
use std::io::{self, BufRead};
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
    let f: Vec<_> = line.split('\t').collect();
    if f.len() != 3 {
        return Err(Error::invalid_input("invalid COPY oracle record"));
    }
    let names = [ByteString::from(decode(f[1])?)];
    let types = [match f[0] {
        "integer" => LogicalType::INTEGER,
        "date" => LogicalType::DATE,
        "timestamp" => LogicalType::TIMESTAMP,
        _ => return Err(Error::invalid_input("unknown COPY mode")),
    }];
    let mut options = CaseInsensitiveMap::default();
    options.insert("dateformat", vec![Value::varchar("%Y'%m")]);
    options.insert("timestampformat", vec![Value::varchar("%Y'%m")]);
    let functions = roastedduck_json::json_copy_functions();
    let plan = functions[0]
        .plan
        .as_ref()
        .ok_or_else(|| Error::internal("missing JSON copy plan"))?;
    let rewrite = plan(&CopyPlanInput {
        names: &names,
        types: &types,
        options: &options,
    })?;
    let sql = byte_format!("{}", rewrite.select_list[0]);
    if sql.as_bytes() != decode(f[2])? {
        return Err(Error::internal(format!(
            "JSON COPY differs from C++ mode {} name {}",
            f[0], f[1]
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
    println!("{count} C++ native JSON COPY projection comparisons passed");
    Ok(())
}
