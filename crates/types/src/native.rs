use crate::{PhysicalType, StringT};

/// A fixed-width value stored directly in a vector's data buffer.
///
/// # Safety
/// Implementors must be plain-old-data: any bit pattern of `size_of::<Self>()` bytes that a
/// vector of the matching physical type may contain is a valid value, and the type has no
/// drop glue. Vectors reinterpret raw buffers as `[Self]`.
pub unsafe trait NativeType: Copy + Default + Send + Sync + 'static {
    /// The physical type whose flat vectors store `Self`.
    const PHYSICAL: PhysicalType;
}

macro_rules! native {
    ($($ty:ty => $phys:ident),* $(,)?) => {
        $(
            // SAFETY: primitive numeric types are POD.
            unsafe impl NativeType for $ty {
                const PHYSICAL: PhysicalType = PhysicalType::$phys;
            }
        )*
    };
}

native! {
    i8 => Int8, i16 => Int16, i32 => Int32, i64 => Int64, i128 => Int128,
    u8 => UInt8, u16 => UInt16, u32 => UInt32, u64 => UInt64, u128 => UInt128,
    f32 => Float, f64 => Double,
}

// SAFETY: `bool` vectors only ever contain 0 or 1; every writer goes through `bool` values.
unsafe impl NativeType for bool {
    const PHYSICAL: PhysicalType = PhysicalType::Bool;
}

// SAFETY: `StringT` is a 16-byte POD (length + inline bytes or prefix + pointer).
unsafe impl NativeType for StringT {
    const PHYSICAL: PhysicalType = PhysicalType::Varchar;
}
