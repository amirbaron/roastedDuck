//! Byte-oriented hstore scalar functions from the pinned PostgreSQL extension.
use roastedduck_common::{Error, Result, byte_format};
use roastedduck_function::{FunctionState, ScalarFunction, ScalarFunctionSet};
use roastedduck_functions::scalar::executor::{Str, StrOut, binary, unary};
use roastedduck_types::{DataChunk, LogicalType, Vector};

type Pair = (Vec<u8>, Option<Vec<u8>>);

struct Parser<'a> {
    input: &'a [u8],
    position: usize,
}
impl Parser<'_> {
    fn space(byte: u8) -> bool {
        matches!(byte, b' ' | b'\t'..=b'\r')
    }
    fn skip_space(&mut self) {
        while self.input.get(self.position).is_some_and(|b| Self::space(*b)) {
            self.position += 1;
        }
    }
    fn near(&self) -> Error {
        let byte = [self.input.get(self.position).copied().unwrap_or(b'?')];
        Error::invalid_input(byte_format!(
            "syntax error in hstore, near \"{}\" at position {}",
            byte.as_slice(),
            self.position
        ))
    }
    fn end() -> Error {
        Error::invalid_input("syntax error in hstore: unexpected end of string")
    }
    fn token(&mut self, key: bool) -> Result<Option<Vec<u8>>> {
        self.skip_space();
        let Some(&first) = self.input.get(self.position) else {
            return Ok(None);
        };
        let mut result = Vec::new();
        if first == b'"' {
            self.position += 1;
            while let Some(&byte) = self.input.get(self.position) {
                self.position += 1;
                if byte == b'"' {
                    return Ok(Some(result));
                }
                if byte == b'\\' {
                    result.push(*self.input.get(self.position).ok_or_else(Self::end)?);
                    self.position += 1;
                } else {
                    result.push(byte);
                }
            }
            return Err(Self::end());
        }
        let terminator = if key { b'=' } else { b',' };
        while let Some(&byte) = self.input.get(self.position) {
            if Self::space(byte) || byte == terminator {
                break;
            }
            if byte == b'\\' {
                self.position += 1;
                result.push(*self.input.get(self.position).ok_or_else(Self::end)?);
            } else {
                result.push(byte);
            }
            self.position += 1;
        }
        if key && result.is_empty() {
            return Err(self.near());
        }
        if !key && result.eq_ignore_ascii_case(b"null") {
            Ok(None)
        } else {
            Ok(Some(result))
        }
    }
    fn parse(mut self) -> Result<Vec<Pair>> {
        let mut pairs = Vec::new();
        self.skip_space();
        while self.position < self.input.len() {
            let key = self.token(true)?.ok_or_else(Self::end)?;
            self.skip_space();
            if self.input.get(self.position..self.position + 2) != Some(b"=>") {
                return Err(self.near());
            }
            self.position += 2;
            let value = self.token(false)?;
            pairs.push((key, value));
            self.skip_space();
            if self.position == self.input.len() {
                break;
            }
            if self.input[self.position] != b',' {
                return Err(self.near());
            }
            self.position += 1;
            self.skip_space();
        }
        Ok(pairs)
    }
}

fn parse(input: &[u8]) -> Result<Vec<Pair>> {
    Parser { input, position: 0 }.parse()
}
fn escape(input: &[u8], output: &mut Vec<u8>) {
    const HEX: &[u8] = b"0123456789abcdef";
    output.push(b'"');
    for &byte in input {
        match byte {
            b'"' => output.extend_from_slice(b"\\\""),
            b'\\' => output.extend_from_slice(b"\\\\"),
            b'\x08' => output.extend_from_slice(b"\\b"),
            b'\x0c' => output.extend_from_slice(b"\\f"),
            b'\n' => output.extend_from_slice(b"\\n"),
            b'\r' => output.extend_from_slice(b"\\r"),
            b'\t' => output.extend_from_slice(b"\\t"),
            0..=0x1f => output.extend_from_slice(&[
                b'\\',
                b'u',
                b'0',
                b'0',
                HEX[usize::from(byte >> 4)],
                HEX[usize::from(byte & 15)],
            ]),
            _ => output.push(byte),
        }
    }
    output.push(b'"');
}
fn get(args: &DataChunk, _: &mut FunctionState, result: &mut Vector) -> Result<()> {
    binary::<Str, Str, StrOut>(args, result, |input, key, output| {
        if let Some((_, Some(value))) = parse(input)?.into_iter().rev().find(|(name, _)| name == key) {
            output.extend_from_slice(&value);
            Ok(true)
        } else {
            Ok(false)
        }
    })
}
fn to_json(args: &DataChunk, _: &mut FunctionState, result: &mut Vector) -> Result<()> {
    unary::<Str, StrOut>(args, result, |input, output| {
        output.push(b'{');
        for (index, (key, value)) in parse(input)?.into_iter().enumerate() {
            if index > 0 {
                output.extend_from_slice(b", ");
            }
            escape(&key, output);
            output.extend_from_slice(b": ");
            if let Some(value) = value {
                escape(&value, output);
            } else {
                output.extend_from_slice(b"null");
            }
        }
        output.push(b'}');
        Ok(true)
    })
}

pub(crate) fn functions() -> Vec<ScalarFunctionSet> {
    let mut get_set = ScalarFunctionSet::new("postgres_hstore_get");
    get_set.functions.push(ScalarFunction::new(
        "postgres_hstore_get",
        vec![LogicalType::VARCHAR; 2],
        LogicalType::VARCHAR,
        get,
    ));
    let mut json_set = ScalarFunctionSet::new("postgres_hstore_to_json");
    json_set.functions.push(ScalarFunction::new(
        "postgres_hstore_to_json",
        vec![LogicalType::VARCHAR],
        roastedduck_json::json_type(),
        to_json,
    ));
    vec![get_set, json_set]
}
