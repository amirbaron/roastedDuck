//! Dispatch from a runtime physical type to monomorphised code (DuckDB's
//! `switch (type.InternalType())` blocks around templated kernels).

/// Evaluates `$body` with the type alias `$T` bound to the native type of `$physical`,
/// for BOOLEAN and every integer and floating point type; anything else evaluates
/// `$other`.
macro_rules! with_numeric_type {
    ($physical:expr, $T:ident => $body:expr, _ => $other:expr $(,)?) => {{
        use roastedduck_types::PhysicalType as P;
        match $physical {
            P::Bool => {
                type $T = bool;
                $body
            }
            P::Int8 => {
                type $T = i8;
                $body
            }
            P::Int16 => {
                type $T = i16;
                $body
            }
            P::Int32 => {
                type $T = i32;
                $body
            }
            P::Int64 => {
                type $T = i64;
                $body
            }
            P::Int128 => {
                type $T = i128;
                $body
            }
            P::UInt8 => {
                type $T = u8;
                $body
            }
            P::UInt16 => {
                type $T = u16;
                $body
            }
            P::UInt32 => {
                type $T = u32;
                $body
            }
            P::UInt64 => {
                type $T = u64;
                $body
            }
            P::UInt128 => {
                type $T = u128;
                $body
            }
            P::Float => {
                type $T = f32;
                $body
            }
            P::Double => {
                type $T = f64;
                $body
            }
            _ => $other,
        }
    }};
}

/// Like [`with_numeric_type`], for the signed and unsigned integer types only.
macro_rules! with_integer_type {
    ($physical:expr, $T:ident => $body:expr, _ => $other:expr $(,)?) => {{
        use roastedduck_types::PhysicalType as P;
        match $physical {
            P::Int8 => {
                type $T = i8;
                $body
            }
            P::Int16 => {
                type $T = i16;
                $body
            }
            P::Int32 => {
                type $T = i32;
                $body
            }
            P::Int64 => {
                type $T = i64;
                $body
            }
            P::Int128 => {
                type $T = i128;
                $body
            }
            P::UInt8 => {
                type $T = u8;
                $body
            }
            P::UInt16 => {
                type $T = u16;
                $body
            }
            P::UInt32 => {
                type $T = u32;
                $body
            }
            P::UInt64 => {
                type $T = u64;
                $body
            }
            P::UInt128 => {
                type $T = u128;
                $body
            }
            _ => $other,
        }
    }};
}

pub(crate) use {with_integer_type, with_numeric_type};
