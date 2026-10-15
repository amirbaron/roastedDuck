//! `random`, `setseed` and the UUID generators (DuckDB's
//! `extension/core_functions/scalar/random`).

use std::any::Any;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use rustdb_common::{Error, Result};
use rustdb_function::random::RandomEngine;
use rustdb_function::{
    FunctionClient, FunctionData, FunctionState, ScalarFunction, ScalarFunctionSet, Stability,
};
use rustdb_types::{DataChunk, LogicalType, Timestamp, Uuid, Vector};

use crate::scalar::executor::unary;

/// The calling connection, kept by `random`, `uuid` and `setseed` (DuckDB's
/// `SetseedBindData`, and the context `RandomInitLocalState` reads).
#[derive(Debug)]
struct ClientBindData {
    client: Arc<FunctionClient>,
}

impl FunctionData for ClientBindData {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn client(state: &FunctionState) -> Result<&Arc<FunctionClient>> {
    state
        .bind_data
        .as_ref()
        .and_then(|data| data.as_any().downcast_ref::<ClientBindData>())
        .map(|data| &data.client)
        .ok_or_else(|| Error::internal("random function executed without bind data"))
}

/// The per-thread engine of a `random`/`uuid` call, seeded from the connection's engine
/// on first use (DuckDB's `RandomInitLocalState`).
fn local_engine(state: &mut FunctionState) -> Result<&mut RandomEngine> {
    if state.local.as_ref().is_none_or(|local| !local.is::<RandomEngine>()) {
        let seed = client(state)?.random_engine().next_random_integer64();
        state.local = Some(Box::new(RandomEngine::new(seed)));
    }
    state
        .local
        .as_mut()
        .and_then(|local| local.downcast_mut::<RandomEngine>())
        .ok_or_else(|| Error::internal("random function state has the wrong type"))
}

/// A volatile function without arguments that keeps the connection for its kernel.
fn generator(
    name: &str,
    return_type: LogicalType,
    kernel: impl Fn(&DataChunk, &mut FunctionState, &mut Vector) -> Result<()> + Send + Sync + 'static,
) -> ScalarFunctionSet {
    let mut function = ScalarFunction::new(name, vec![], return_type, kernel).with_bind(|_, input| {
        Ok(Some(Arc::new(ClientBindData {
            client: Arc::clone(input.client),
        }) as Arc<dyn FunctionData>))
    });
    function.stability = Stability::Volatile;
    let mut set = ScalarFunctionSet::new(name);
    set.add(function);
    set
}

/// Fills `result` with one generated value per row.
fn generate<T: rustdb_types::NativeType>(
    args: &DataChunk,
    state: &mut FunctionState,
    result: &mut Vector,
    mut next: impl FnMut(&mut RandomEngine) -> T,
) -> Result<()> {
    let engine = local_engine(state)?;
    let count = args.len();
    result.reset_flat(count);
    for value in &mut result.values_mut::<T>()[..count] {
        *value = next(engine);
    }
    Ok(())
}

/// `random()` (DuckDB's `RandomFun`).
pub fn random_functions() -> ScalarFunctionSet {
    generator("random", LogicalType::DOUBLE, |args, state, result| {
        generate::<f64>(args, state, result, RandomEngine::next_random)
    })
}

/// A random version 4 UUID (DuckDB's `UUIDv4::GenerateRandomUUID`, which stores the bytes
/// without flipping the top bit).
fn uuid_v4(engine: &mut RandomEngine) -> Uuid {
    let mut bytes = [0u8; 16];
    for chunk in bytes.chunks_exact_mut(4) {
        chunk.copy_from_slice(&engine.next_random_integer().to_le_bytes());
    }
    bytes[8] = (bytes[8] & 0xBF) | 0x80;
    bytes[6] = (bytes[6] & 0x4F) | 0x40;
    Uuid(i128::from_be_bytes(bytes))
}

/// A random version 7 UUID with the current time (DuckDB's `UUIDv7::GenerateRandomUUID`).
#[allow(clippy::cast_possible_truncation, reason = "byte extraction")]
fn uuid_v7(engine: &mut RandomEngine) -> Uuid {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as u64);
    let mut bytes = [0u8; 16];
    bytes[..6].copy_from_slice(&millis.to_be_bytes()[2..]);
    let a = engine.next_random_integer().to_be_bytes();
    let b = engine.next_random_integer().to_be_bytes();
    let c = engine.next_random_integer().to_be_bytes();
    bytes[6..10].copy_from_slice(&a);
    bytes[10..14].copy_from_slice(&b);
    bytes[14..16].copy_from_slice(&c[..2]);
    bytes[6] = (bytes[6] & 0x0f) | (7 << 4);
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_u128(u128::from_be_bytes(bytes))
}

/// `uuid()`/`gen_random_uuid()`/`uuidv4()` (DuckDB's `UUIDv4Fun`).
pub fn uuid_functions(name: &str) -> ScalarFunctionSet {
    generator(name, LogicalType::UUID, |args, state, result| {
        generate::<Uuid>(args, state, result, uuid_v4)
    })
}

/// `uuidv7()` (DuckDB's `UUIDv7Fun`).
pub fn uuidv7_functions() -> ScalarFunctionSet {
    generator("uuidv7", LogicalType::UUID, |args, state, result| {
        generate::<Uuid>(args, state, result, uuid_v7)
    })
}

/// `setseed(x)`: reseeds the connection's engine (DuckDB's `SetseedFun`).
pub fn setseed_functions() -> ScalarFunctionSet {
    const HALF_MAX: f64 = (u32::MAX / 2) as f64;
    let mut function = ScalarFunction::new(
        "setseed",
        vec![LogicalType::DOUBLE],
        LogicalType::SQLNULL,
        |args, state, result| {
            let seeds = args.column(0).flattened(args.len());
            let client = client(state)?;
            let mut engine = client.random_engine();
            for &seed in &seeds.values::<f64>()[..args.len()] {
                if !(-1.0..=1.0).contains(&seed) {
                    return Err(Error::invalid_input(
                        "SETSEED accepts seed values between -1.0 and 1.0, inclusive",
                    ));
                }
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "LossyNumericCast<uint32_t>"
                )]
                let normalized = ((seed + 1.0) * HALF_MAX) as u32;
                engine.set_seed(u64::from(normalized));
            }
            *result = Vector::constant_null(LogicalType::SQLNULL);
            Ok(())
        },
    )
    .with_bind(|_, input| {
        Ok(Some(Arc::new(ClientBindData {
            client: Arc::clone(input.client),
        }) as Arc<dyn FunctionData>))
    });
    function.stability = Stability::Volatile;
    let mut set = ScalarFunctionSet::new("setseed");
    set.add(function);
    set
}

/// `uuid_extract_version(uuid)`: the version digit of the text form, as DuckDB computes
/// it (`uuid[14] - '0'`, so hex letters give 49..54).
pub fn uuid_extract_version_functions() -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new("uuid_extract_version");
    set.add(ScalarFunction::new(
        "uuid_extract_version",
        vec![LogicalType::UUID],
        LogicalType::UINTEGER,
        |args, _, result| {
            let mut text = Vec::with_capacity(Uuid::STRING_SIZE);
            unary::<Uuid, u32>(args, result, |uuid, out| {
                text.clear();
                uuid.write_to(&mut text);
                *out = u32::from(text[14].wrapping_sub(b'0'));
                Ok(true)
            })
        },
    ));
    set
}

/// `uuid_extract_timestamp(uuid)`: the millisecond timestamp of a version 7 UUID.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "bit extraction")]
pub fn uuid_extract_timestamp_functions() -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new("uuid_extract_timestamp");
    set.add(ScalarFunction::new(
        "uuid_extract_timestamp",
        vec![LogicalType::UUID],
        LogicalType::TIMESTAMP_TZ,
        |args, _, result| {
            unary::<Uuid, Timestamp>(args, result, |uuid, out| {
                let upper = (uuid.0 >> 64) as i64;
                let version = ((upper >> 8) as u8 & 0xf0) >> 4;
                if version != 7 {
                    return Err(Error::invalid_input(format!(
                        "Given UUID is with version {version}, not version 7."
                    )));
                }
                let millis = (upper ^ i64::MIN) >> 16;
                *out = Timestamp(millis.wrapping_mul(1000));
                Ok(true)
            })
        },
    ));
    set
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_uuids_have_their_version() {
        let mut engine = RandomEngine::new(1);
        let mut text = Vec::new();
        uuid_v4(&mut engine).write_to(&mut text);
        assert_eq!(text[14], b'4');
        text.clear();
        uuid_v7(&mut engine).write_to(&mut text);
        assert_eq!(text[14], b'7');
    }
}
