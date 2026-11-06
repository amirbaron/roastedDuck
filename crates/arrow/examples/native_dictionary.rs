//! Exact Arrow ENUM dictionary buffer bytes, offsets and indices against C++.
#[path = "native_names.rs"]
#[allow(dead_code, reason = "reuse hex decoder")]
pub mod schema;
use rustdb_arrow::{ClientProperties, ffi::ArrowArrayWrapper, to_arrow_array};
use rustdb_common::{ByteString, Error, Result};
use rustdb_types::{DataChunk, LogicalType, Value, Vector};
use std::io::{self, BufRead};
pub fn compare(line: &str) -> Result<()> {
    let f: Vec<_> = line.split('\t').collect();
    if f.len() != 5 {
        return Err(Error::invalid_input("invalid Arrow dictionary record"));
    }
    let name = ByteString::from(schema::decode(f[0])?);
    let mut tail = name.clone();
    tail.push_bytes("_tail");
    let ty = LogicalType::enum_type_bytes(vec![name, tail]);
    let mut vector = Vector::flat(ty.clone(), 2);
    vector.set_value(0, &Value::enum_value(ty.clone(), 0))?;
    vector.set_value(1, &Value::enum_value(ty, 1))?;
    let output = ArrowArrayWrapper {
        arrow_array: to_arrow_array(&DataChunk::new(vec![vector], 2), &ClientProperties::default())?,
    };
    // SAFETY: this owned array was just exported with one enum column and two values.
    let child = unsafe { &**output.arrow_array.children };
    // SAFETY: ENUM export always has a live two-label dictionary with three string buffers.
    let dictionary = unsafe { &*child.dictionary };
    let buffers = unsafe { std::slice::from_raw_parts(dictionary.buffers, 3) };
    // SAFETY: dictionary has length2, with length+1 i32 offsets.
    let offsets = unsafe { std::slice::from_raw_parts(buffers[1].cast::<i32>(), 3) };
    let len = usize::try_from(offsets[2]).map_err(|_| Error::internal("negative dictionary size"))?;
    // SAFETY: final offset is the length of this export's owned data buffer.
    let data = unsafe { std::slice::from_raw_parts(buffers[2].cast::<u8>(), len) };
    // SAFETY: enum has two labels and is exported as two UInt8 indices.
    let indices = unsafe { std::slice::from_raw_parts((*child.buffers.add(1)).cast::<u8>(), 2) };
    let expected_offsets = f[2]
        .split(',')
        .map(str::parse::<i32>)
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|_| Error::invalid_input("invalid offsets"))?;
    if dictionary.length.to_string() != f[1]
        || offsets != expected_offsets
        || data != schema::decode(f[3])?
        || indices != schema::decode(f[4])?
    {
        return Err(Error::internal("Arrow dictionary differs from C++"));
    }
    Ok(())
}
fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut count = 0;
    for line in io::stdin().lock().lines() {
        compare(&line?)?;
        count += 1;
    }
    println!("{count} C++ Arrow ENUM dictionary buffer comparisons passed");
    Ok(())
}
