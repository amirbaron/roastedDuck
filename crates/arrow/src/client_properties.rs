//! The connection settings that shape Arrow output (DuckDB's `main/client_properties.hpp`
//! and `common/enums/arrow_format_version.hpp`).

/// The width of the offsets of variable-size Arrow arrays (DuckDB's `ArrowOffsetSize`):
/// `arrow_large_buffer_size` selects the large (64-bit) layouts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ArrowOffsetSize {
    #[default]
    Regular,
    Large,
}

/// The Arrow format version produced (DuckDB's `ArrowFormatVersion`, set by
/// `arrow_output_version`); later versions add layouts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum ArrowFormatVersion {
    /// The base version.
    #[default]
    V1_0 = 10,
    /// Adds the 256-bit decimal type.
    V1_1 = 11,
    /// Adds the month-day-nano interval type.
    V1_2 = 12,
    /// Adds the run-end encoded layout.
    V1_3 = 13,
    /// Adds the binary and string views, the list views and variadic buffers.
    V1_4 = 14,
    /// Adds 32 and 64-bit decimals.
    V1_5 = 15,
}

impl ArrowFormatVersion {
    /// The version an `arrow_output_version` value names (`1.0` to `1.5`).
    pub fn from_setting(value: &str) -> Option<Self> {
        Some(match value {
            "1.0" => Self::V1_0,
            "1.1" => Self::V1_1,
            "1.2" => Self::V1_2,
            "1.3" => Self::V1_3,
            "1.4" => Self::V1_4,
            "1.5" => Self::V1_5,
            _ => return None,
        })
    }
}

/// The properties of a connection that results are converted with (DuckDB's
/// `ClientProperties`), captured when a result or an Arrow options object is made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientProperties {
    /// The `TimeZone` setting, written into `TIMESTAMP WITH TIME ZONE` formats.
    pub time_zone: String,
    pub arrow_offset_size: ArrowOffsetSize,
    /// `arrow_output_list_view`: LIST columns as list views.
    pub arrow_use_list_view: bool,
    /// `produce_arrow_string_view`: VARCHAR columns as string views.
    pub produce_arrow_string_view: bool,
    /// `arrow_lossless_conversion`: types without an exact Arrow counterpart as
    /// extension types.
    pub arrow_lossless_conversion: bool,
    pub arrow_output_version: ArrowFormatVersion,
}

impl Default for ClientProperties {
    fn default() -> Self {
        Self {
            time_zone: "UTC".to_owned(),
            arrow_offset_size: ArrowOffsetSize::Regular,
            arrow_use_list_view: false,
            produce_arrow_string_view: false,
            arrow_lossless_conversion: false,
            arrow_output_version: ArrowFormatVersion::V1_0,
        }
    }
}
