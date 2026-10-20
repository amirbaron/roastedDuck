//! Column statistics collected while writing (DuckDB's `ColumnWriterStatistics` and its
//! numeric, floating-point, string, UUID, boolean and fixed-decimal subclasses).

/// The statistics of a column chunk as they are stored in the footer.
pub trait WriterStatistics: Send {
    /// `HasStats`.
    fn has_stats(&self) -> bool {
        false
    }
    /// The deprecated `min` (signed order); empty when not written.
    fn min(&self) -> Vec<u8> {
        Vec::new()
    }
    fn max(&self) -> Vec<u8> {
        Vec::new()
    }
    fn min_value(&self) -> Vec<u8> {
        Vec::new()
    }
    fn max_value(&self) -> Vec<u8> {
        Vec::new()
    }
    fn can_have_nan(&self) -> bool {
        false
    }
    fn has_nan(&self) -> bool {
        false
    }
    fn min_is_exact(&self) -> bool {
        true
    }
    fn max_is_exact(&self) -> bool {
        true
    }
    /// The geometry statistics, once a value was seen (`HasGeoStats` / `GetGeoStats`).
    fn geo_stats(&self) -> Option<&crate::geo::GeoStats> {
        None
    }
}

/// The statistics of GEOMETRY values (`GeoStatisticsState`): no min/max, but the types
/// and the bounding box.
#[derive(Debug, Default)]
pub struct GeoStatistics {
    pub has_stats: bool,
    pub stats: crate::geo::GeoStats,
}

impl WriterStatistics for GeoStatistics {
    fn geo_stats(&self) -> Option<&crate::geo::GeoStats> {
        self.has_stats.then_some(&self.stats)
    }
}

/// Statistics that are never written (`ColumnWriterStatistics` itself).
#[derive(Debug, Default)]
pub struct NoStatistics;

impl WriterStatistics for NoStatistics {}

/// A Parquet value with an order and a byte representation in statistics.
pub trait StatValue: Copy + PartialOrd + Send {
    /// `NumericLimits<T>::Maximum()` / `Minimum()`.
    const MAX: Self;
    const MIN: Self;
    fn to_bytes(self) -> Vec<u8>;
}

macro_rules! stat_value {
    ($($ty:ty),*) => {$(
        impl StatValue for $ty {
            const MAX: Self = <$ty>::MAX;
            const MIN: Self = <$ty>::MIN;
            fn to_bytes(self) -> Vec<u8> {
                self.to_le_bytes().to_vec()
            }
        }
    )*};
}
stat_value!(i32, i64, u32, u64);

macro_rules! stat_value_float {
    ($($ty:ty),*) => {$(
        impl StatValue for $ty {
            // `NumericLimits` of types with infinities are the infinities.
            const MAX: Self = <$ty>::INFINITY;
            const MIN: Self = <$ty>::NEG_INFINITY;
            fn to_bytes(self) -> Vec<u8> {
                self.to_le_bytes().to_vec()
            }
        }
    )*};
}
stat_value_float!(f32, f64);

/// `NumericStatisticsState<SRC, T>`: min/max of the Parquet values; the deprecated
/// `min`/`max` are only written for signed source types.
#[derive(Debug)]
pub struct NumericStatistics<T> {
    pub min: T,
    pub max: T,
    pub signed: bool,
    pub floating: bool,
    pub has_nan: bool,
}

impl<T: StatValue> NumericStatistics<T> {
    pub fn new(signed: bool, floating: bool) -> Self {
        // DuckDB's `NumericLimits<float>::Minimum()` is the lowest finite value.
        Self {
            min: T::MAX,
            max: T::MIN,
            signed,
            floating,
            has_nan: false,
        }
    }

    /// `ParquetCastOperator::HandleStats` / `FloatingPointOperator::HandleStats`.
    #[inline]
    #[allow(clippy::eq_op, reason = "NaN check")]
    pub fn update(&mut self, value: T) {
        if self.floating && value != value {
            self.has_nan = true;
            return;
        }
        if value < self.min {
            self.min = value;
        }
        if value > self.max {
            self.max = value;
        }
    }
}

impl<T: StatValue> WriterStatistics for NumericStatistics<T> {
    fn has_stats(&self) -> bool {
        self.min <= self.max
    }
    fn min(&self) -> Vec<u8> {
        if self.signed { self.min_value() } else { Vec::new() }
    }
    fn max(&self) -> Vec<u8> {
        if self.signed { self.max_value() } else { Vec::new() }
    }
    fn min_value(&self) -> Vec<u8> {
        if self.has_stats() {
            self.min.to_bytes()
        } else {
            Vec::new()
        }
    }
    fn max_value(&self) -> Vec<u8> {
        if self.has_stats() {
            self.max.to_bytes()
        } else {
            Vec::new()
        }
    }
    fn can_have_nan(&self) -> bool {
        self.floating
    }
    fn has_nan(&self) -> bool {
        self.has_nan
    }
}

/// The longest min/max string kept (`MAX_STRING_STATISTICS_SIZE`).
const MAX_STRING_STATISTICS_SIZE: usize = 256;

/// `StringStatisticsState`.
#[derive(Debug, Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "the flags of DuckDB's StringStatisticsState"
)]
pub struct StringStatistics {
    pub is_blob: bool,
    has_stats: bool,
    min_truncated: bool,
    max_truncated: bool,
    failed_truncate: bool,
    min: Vec<u8>,
    max: Vec<u8>,
    /// Enum statistics are never exact (`EnumStatisticsState`).
    pub inexact: bool,
}

impl StringStatistics {
    pub fn new(is_blob: bool) -> Self {
        Self {
            is_blob,
            ..Self::default()
        }
    }

    /// `StringStatisticsState::Update`.
    pub fn update(&mut self, value: &[u8]) {
        if self.failed_truncate {
            return;
        }
        if !self.has_stats || value < self.min.as_slice() {
            if value.len() > MAX_STRING_STATISTICS_SIZE {
                self.min = self.truncate_min(value);
                self.min_truncated = true;
            } else {
                value.clone_into(&mut self.min);
                self.min_truncated = false;
            }
        }
        if !self.has_stats || value > self.max.as_slice() {
            if value.len() > MAX_STRING_STATISTICS_SIZE {
                let Some(max) = self.try_truncate_max(value) else {
                    self.failed_truncate = true;
                    self.has_stats = false;
                    self.min.clear();
                    self.max.clear();
                    return;
                };
                self.max = max;
                self.max_truncated = true;
            } else {
                value.clone_into(&mut self.max);
                self.max_truncated = false;
            }
        }
        self.has_stats = true;
    }

    /// `TruncateMin`: a prefix (at a UTF-8 character boundary for VARCHAR).
    fn truncate_min(&self, value: &[u8]) -> Vec<u8> {
        let mut size = MAX_STRING_STATISTICS_SIZE;
        if !self.is_blob {
            while size > 0 && value[size] & 0xc0 == 0x80 {
                size -= 1;
            }
        }
        value[..size].to_vec()
    }

    /// `TryTruncateMax`: a prefix with its last incrementable byte incremented.
    fn try_truncate_max(&self, value: &[u8]) -> Option<Vec<u8>> {
        let max_byte = if self.is_blob { 0xff } else { 0x7f };
        let mut position = MAX_STRING_STATISTICS_SIZE;
        while position > 0 && value[position - 1] >= max_byte {
            position -= 1;
        }
        if position == 0 {
            return None;
        }
        let mut result = value[..position].to_vec();
        result[position - 1] += 1;
        Some(result)
    }
}

impl WriterStatistics for StringStatistics {
    fn has_stats(&self) -> bool {
        self.has_stats
    }
    fn min(&self) -> Vec<u8> {
        self.min_value()
    }
    fn max(&self) -> Vec<u8> {
        self.max_value()
    }
    fn min_value(&self) -> Vec<u8> {
        if self.has_stats {
            self.min.clone()
        } else {
            Vec::new()
        }
    }
    fn max_value(&self) -> Vec<u8> {
        if self.has_stats {
            self.max.clone()
        } else {
            Vec::new()
        }
    }
    fn min_is_exact(&self) -> bool {
        !self.inexact && !self.min_truncated
    }
    fn max_is_exact(&self) -> bool {
        !self.inexact && !self.max_truncated
    }
}

/// Statistics of fixed-length byte values ordered by `memcmp` (`UUIDStatisticsState`).
#[derive(Debug, Default)]
pub struct BytesStatistics {
    has_stats: bool,
    min: Vec<u8>,
    max: Vec<u8>,
}

impl BytesStatistics {
    pub fn update(&mut self, value: &[u8]) {
        if !self.has_stats || value < self.min.as_slice() {
            value.clone_into(&mut self.min);
        }
        if !self.has_stats || value > self.max.as_slice() {
            value.clone_into(&mut self.max);
        }
        self.has_stats = true;
    }
}

impl WriterStatistics for BytesStatistics {
    fn has_stats(&self) -> bool {
        self.has_stats
    }
    fn min(&self) -> Vec<u8> {
        self.min_value()
    }
    fn max(&self) -> Vec<u8> {
        self.max_value()
    }
    fn min_value(&self) -> Vec<u8> {
        if self.has_stats {
            self.min.clone()
        } else {
            Vec::new()
        }
    }
    fn max_value(&self) -> Vec<u8> {
        if self.has_stats {
            self.max.clone()
        } else {
            Vec::new()
        }
    }
}

/// `BooleanStatisticsState`.
#[derive(Debug)]
pub struct BooleanStatistics {
    pub min: bool,
    pub max: bool,
}

impl Default for BooleanStatistics {
    fn default() -> Self {
        Self {
            min: true,
            max: false,
        }
    }
}

impl WriterStatistics for BooleanStatistics {
    fn has_stats(&self) -> bool {
        !self.min || self.max
    }
    fn min(&self) -> Vec<u8> {
        self.min_value()
    }
    fn max(&self) -> Vec<u8> {
        self.max_value()
    }
    fn min_value(&self) -> Vec<u8> {
        if self.has_stats() {
            vec![u8::from(self.min)]
        } else {
            Vec::new()
        }
    }
    fn max_value(&self) -> Vec<u8> {
        if self.has_stats() {
            vec![u8::from(self.max)]
        } else {
            Vec::new()
        }
    }
}

/// `WriteParquetDecimal`: a 128-bit decimal as 16 big-endian two's complement bytes.
pub fn decimal_to_bytes(value: i128) -> [u8; 16] {
    value.to_be_bytes()
}

/// `FixedDecimalStatistics`.
#[derive(Debug)]
pub struct DecimalStatistics {
    pub min: i128,
    pub max: i128,
}

impl Default for DecimalStatistics {
    fn default() -> Self {
        Self {
            min: i128::MAX,
            max: i128::MIN,
        }
    }
}

impl WriterStatistics for DecimalStatistics {
    fn has_stats(&self) -> bool {
        self.min <= self.max
    }
    fn min(&self) -> Vec<u8> {
        self.min_value()
    }
    fn max(&self) -> Vec<u8> {
        self.max_value()
    }
    fn min_value(&self) -> Vec<u8> {
        if self.has_stats() {
            decimal_to_bytes(self.min).to_vec()
        } else {
            Vec::new()
        }
    }
    fn max_value(&self) -> Vec<u8> {
        if self.has_stats() {
            decimal_to_bytes(self.max).to_vec()
        } else {
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_strings_are_truncated() {
        let mut stats = StringStatistics::new(false);
        let long = vec![b'a'; 300];
        stats.update(&long);
        assert_eq!(stats.min_value().len(), 256);
        let max = stats.max_value();
        assert_eq!(max.len(), 256);
        assert_eq!(max[255], b'b');
        assert!(!stats.max_is_exact());
    }

    #[test]
    fn negative_decimals_are_twos_complement() {
        assert_eq!(decimal_to_bytes(-1), [0xff; 16]);
        assert_eq!(decimal_to_bytes(1)[15], 1);
    }
}
