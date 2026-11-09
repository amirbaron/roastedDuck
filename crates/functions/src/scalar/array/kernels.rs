//! Vector distance kernels shared by the `list_*` and `array_*` distance functions
//! (DuckDB's `array_kernels.hpp`).
//!
//! The reference build contracts `acc += x * y` into fused multiply-adds, which these
//! kernels reproduce with `mul_add` (the reductions are not vectorized: no reassociation).

/// A floating point element type.
pub(crate) trait Float: roastedduck_types::NativeType + Copy + PartialOrd {
    const ZERO: Self;
    const ONE: Self;
    fn mul_add(self, a: Self, b: Self) -> Self;
    fn sqrt(self) -> Self;
    fn sub(self, other: Self) -> Self;
    fn mul(self, other: Self) -> Self;
    fn div(self, other: Self) -> Self;
    fn neg(self) -> Self;
}

macro_rules! float {
    ($ty:ty) => {
        impl Float for $ty {
            const ZERO: Self = 0.0;
            const ONE: Self = 1.0;
            #[inline]
            fn mul_add(self, a: Self, b: Self) -> Self {
                <$ty>::mul_add(self, a, b)
            }
            #[inline]
            fn sqrt(self) -> Self {
                <$ty>::sqrt(self)
            }
            #[inline]
            fn sub(self, other: Self) -> Self {
                self - other
            }
            #[inline]
            fn mul(self, other: Self) -> Self {
                self * other
            }
            #[inline]
            fn div(self, other: Self) -> Self {
                self / other
            }
            #[inline]
            fn neg(self) -> Self {
                -self
            }
        }
    };
}
float!(f32);
float!(f64);

/// A fold of two equally long vectors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Fold {
    InnerProduct,
    NegativeInnerProduct,
    CosineSimilarity,
    CosineDistance,
    Distance,
}

impl Fold {
    /// Whether empty inputs have a result (DuckDB's `ALLOW_EMPTY`).
    pub(crate) const fn allows_empty(self) -> bool {
        !matches!(self, Self::CosineSimilarity | Self::CosineDistance)
    }

    pub(crate) fn apply<T: Float>(self, left: &[T], right: &[T]) -> T {
        match self {
            Self::InnerProduct => inner_product(left, right),
            Self::NegativeInnerProduct => inner_product(left, right).neg(),
            Self::CosineSimilarity => cosine_similarity(left, right),
            Self::CosineDistance => T::ONE.sub(cosine_similarity(left, right)),
            Self::Distance => {
                let mut distance = T::ZERO;
                for (&x, &y) in left.iter().zip(right) {
                    let diff = x.sub(y);
                    distance = diff.mul_add(diff, distance);
                }
                distance.sqrt()
            }
        }
    }
}

fn inner_product<T: Float>(left: &[T], right: &[T]) -> T {
    let mut result = T::ZERO;
    for (&x, &y) in left.iter().zip(right) {
        result = x.mul_add(y, result);
    }
    result
}

fn cosine_similarity<T: Float>(left: &[T], right: &[T]) -> T {
    let mut distance = T::ZERO;
    let mut norm_left = T::ZERO;
    let mut norm_right = T::ZERO;
    for (&x, &y) in left.iter().zip(right) {
        distance = x.mul_add(y, distance);
        norm_left = x.mul_add(x, norm_left);
        norm_right = y.mul_add(y, norm_right);
    }
    let similarity = distance.div(norm_left.mul(norm_right).sqrt());
    // `std::max(-1, std::min(similarity, 1))` with libc++'s comparisons: a NaN survives
    // `min` and becomes -1 in `max`.
    let upper = if T::ONE < similarity { T::ONE } else { similarity };
    if T::ONE.neg() < upper { upper } else { T::ONE.neg() }
}

/// The cross product of two 3-vectors.
pub(crate) fn cross_product<T: Float>(left: &[T], right: &[T]) -> [T; 3] {
    let (lx, ly, lz) = (left[0], left[1], left[2]);
    let (rx, ry, rz) = (right[0], right[1], right[2]);
    [
        ly.mul_add(rz, lz.mul(ry).neg()),
        lz.mul_add(rx, lx.mul(rz).neg()),
        lx.mul_add(ry, ly.mul(rx).neg()),
    ]
}
