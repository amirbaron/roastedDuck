//! VARIANT statistics (DuckDB's `VariantStats` and `VariantShreddedStats`): whether the
//! column is shredded, and the statistics of its unshredded and shredded data.
//!
//! The shredded data has a type of its own: a primitive type when every value is of that
//! type, otherwise `STRUCT(typed_value <shredding>, untyped_value_index UINTEGER)`, where
//! an OBJECT shreds into a STRUCT of shredded fields and an ARRAY into a LIST of shredded
//! elements. A NULL `untyped_value_index` means the value is the typed one (or a VARIANT
//! NULL), 0 a field missing from its OBJECT, and any other value the 1-based index of the
//! value in the unshredded VARIANT of the row.

use roastedduck_common::{ByteString, byte_format as format, byte_write as write};
use std::cmp::Ordering;
use std::fmt;

use roastedduck_common::Result;
use roastedduck_types::variant::{VariantPathComponent, unshredded_type};
use roastedduck_types::{LogicalType, LogicalTypeId};

use super::numeric::NumericValue;
use super::{BaseStatistics, StatsData};

/// The field of `typed_value` in a shredded group (`VariantStats::TYPED_VALUE_INDEX`).
pub const TYPED_VALUE_INDEX: usize = 0;
/// The field of `untyped_value_index` (`VariantStats::UNTYPED_VALUE_INDEX`).
pub const UNTYPED_VALUE_INDEX: usize = 1;

/// How the values of VARIANT statistics are stored (`VariantStatsShreddingState`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum VariantShreddingState {
    /// Neither shredded nor unshredded yet: merges as the other side.
    Uninitialized = 0,
    /// Appended data, not shredded.
    NotShredded = 1,
    /// Shredded consistently.
    Shredded = 2,
    /// Merged from incompatible shreddings.
    Inconsistent = 3,
}

impl VariantShreddingState {
    const ALL: [Self; 4] = [
        Self::Uninitialized,
        Self::NotShredded,
        Self::Shredded,
        Self::Inconsistent,
    ];

    /// `EnumUtil::ToString(VariantStatsShreddingState)`.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Uninitialized => "UNINITIALIZED",
            Self::NotShredded => "NOT_SHREDDED",
            Self::Shredded => "SHREDDED",
            Self::Inconsistent => "INCONSISTENT",
        }
    }

    pub(crate) fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|state| state.name().eq_ignore_ascii_case(name))
    }

    pub(crate) fn from_u8(value: u8) -> Option<Self> {
        Self::ALL.get(usize::from(value)).copied()
    }
}

/// The type-specific statistics of a VARIANT (`VariantStatsData` with the two child
/// statistics of its `BaseStatistics`).
#[derive(Clone, Debug, PartialEq)]
pub struct VariantStats {
    pub shredding_state: VariantShreddingState,
    /// The statistics of the unshredded data, of [`unshredded_type`].
    pub unshredded: BaseStatistics,
    /// The statistics of the shredded data; present exactly when the state is
    /// [`VariantShreddingState::Shredded`].
    pub shredded: Option<BaseStatistics>,
}

impl VariantStats {
    /// `VariantStats::CreateEmpty`.
    pub(crate) fn empty() -> Self {
        Self {
            shredding_state: VariantShreddingState::Uninitialized,
            unshredded: BaseStatistics::create_empty(unshredded_type()),
            shredded: None,
        }
    }

    /// `VariantStats::CreateUnknown`: nothing is known about the values.
    pub(crate) fn unknown() -> Self {
        Self {
            shredding_state: VariantShreddingState::Inconsistent,
            unshredded: BaseStatistics::create_unknown(unshredded_type()),
            shredded: None,
        }
    }

    /// `VariantStats::IsShredded`.
    pub fn is_shredded(&self) -> bool {
        self.shredding_state == VariantShreddingState::Shredded
    }

    /// `VariantStats::GetShreddedStats`, for shredded statistics.
    pub fn shredded_stats(&self) -> Option<&BaseStatistics> {
        self.shredded.as_ref().filter(|_| self.is_shredded())
    }

    /// `VariantStats::MarkAsNotShredded`: appended values make the statistics
    /// INCONSISTENT once merged with shredded ones.
    pub fn mark_as_not_shredded(&mut self) {
        self.shredding_state = VariantShreddingState::NotShredded;
    }

    /// `VariantStats::SetShreddedStats`.
    pub fn set_shredded_stats(&mut self, stats: BaseStatistics) {
        self.shredding_state = VariantShreddingState::Shredded;
        self.shredded = Some(stats);
    }

    /// `VariantStats::Merge`.
    pub(crate) fn merge(&mut self, other: &Self) {
        use VariantShreddingState as S;
        self.unshredded.merge(&other.unshredded);
        if other.shredding_state == S::Uninitialized {
            return;
        }
        match self.shredding_state {
            S::Inconsistent => {}
            S::Uninitialized => {
                // UNINITIALIZED + ANY -> ANY.
                self.shredded = other.shredded_stats().cloned();
                self.shredding_state = other.shredding_state;
            }
            S::NotShredded => {
                if other.shredding_state != S::NotShredded {
                    self.make_inconsistent();
                }
            }
            S::Shredded => {
                let merged = match (&self.shredded, other.shredded_stats()) {
                    (Some(own), Some(theirs)) => merge_shredding(own, theirs),
                    _ => None,
                };
                match merged {
                    // SHREDDED(T1) + SHREDDED(T1) -> SHREDDED.
                    Some(merged) => self.shredded = Some(merged),
                    // SHREDDED(T1) + SHREDDED(T2) or anything else -> INCONSISTENT.
                    None => self.make_inconsistent(),
                }
            }
        }
    }

    fn make_inconsistent(&mut self) {
        self.shredding_state = VariantShreddingState::Inconsistent;
        self.shredded = None;
    }
}

impl VariantStats {
    /// `VariantStats::ToString`.
    fn write_statistics_bytes(&self, f: &mut ByteString) -> fmt::Result {
        write!(f, "shredding_state: {}", self.shredding_state.name())?;
        if let Some(shredded) = self.shredded_stats() {
            write!(
                f,
                ", shredding: {{typed_value_type: {}, stats: {{{}}}}}",
                structured_type(shredded.logical_type()),
                shredded_stats_text(shredded)
            )?;
        }
        Ok(())
    }
}

/// `ToStringInternal`: whether each level of shredded statistics is fully shredded.
fn shredded_stats_text(stats: &BaseStatistics) -> ByteString {
    let mut result = format!("fully_shredded: {}", is_fully_shredded(stats));
    let typed = typed_stats(stats);
    match typed.logical_type().id() {
        LogicalTypeId::List => {
            if let Some(child) = typed.child() {
                result.push_bytes(", child: ");
                result.push_bytes(shredded_stats_text(child));
            }
        }
        LogicalTypeId::Struct => {
            let fields = typed.logical_type().struct_children().unwrap_or_default();
            let children = typed.struct_children().unwrap_or_default();
            // `std::lexicographical_compare` of the names' (signed) chars.
            let mut order: Vec<usize> = (0..fields.len()).collect();
            order.sort_by(|&a, &b| signed_char_cmp(&fields[a].0, &fields[b].0));
            let entries: Vec<ByteString> = order
                .into_iter()
                .filter_map(|index| {
                    let child = children.get(index)?;
                    Some(format!("{}: {}", fields[index].0, shredded_stats_text(child)))
                })
                .collect();
            let _ = write!(result, ", children: {{{}}}", ByteString::join(&entries, ", "));
        }
        _ => {}
    }
    result
}

/// Compares strings by their bytes as (signed) `char`s.
fn signed_char_cmp(a: &[u8], b: &[u8]) -> Ordering {
    a.iter()
        .map(|&byte| byte as i8)
        .cmp(b.iter().map(|&byte| byte as i8))
}

/// `ToStructuredType`: the DuckDB type of a shredding, without the `typed_value` /
/// `untyped_value_index` groups and with the fields of OBJECTs sorted by name.
pub fn structured_type(shredding: &LogicalType) -> LogicalType {
    if shredding.id() != LogicalTypeId::Struct {
        return shredding.clone();
    }
    let Some((_, typed_value)) = shredding
        .struct_children()
        .and_then(|children| children.get(TYPED_VALUE_INDEX))
    else {
        return shredding.clone();
    };
    match typed_value.id() {
        LogicalTypeId::Struct => {
            let mut fields: Vec<(ByteString, LogicalType)> = typed_value
                .struct_children()
                .unwrap_or_default()
                .iter()
                .map(|(name, field)| (name.clone(), structured_type(field)))
                .collect();
            fields.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
            LogicalType::struct_type(fields)
        }
        LogicalTypeId::List => LogicalType::list(structured_type(
            typed_value.child_type().unwrap_or(&LogicalType::INVALID),
        )),
        _ => typed_value.clone(),
    }
}

/// The shredded type of a structured type (`TypeVisitor::VisitReplace` wrapping every
/// level in `STRUCT(typed_value <level>, untyped_value_index UINTEGER)`), the inverse of
/// [`structured_type`].
pub fn wrap_shredded_levels(ty: &LogicalType) -> LogicalType {
    let level = match ty.id() {
        LogicalTypeId::Struct => LogicalType::struct_type(
            ty.struct_children()
                .unwrap_or_default()
                .iter()
                .map(|(name, field)| (name.clone(), wrap_shredded_levels(field)))
                .collect(),
        ),
        LogicalTypeId::List => LogicalType::list(wrap_shredded_levels(
            ty.child_type().unwrap_or(&LogicalType::INVALID),
        )),
        _ => ty.clone(),
    };
    LogicalType::struct_type(vec![
        ("typed_value".into(), level),
        ("untyped_value_index".into(), LogicalType::UINTEGER),
    ])
}

/// `VariantStats::GetTypedStats`: the statistics of the typed values of a shredded level.
pub fn typed_stats(stats: &BaseStatistics) -> &BaseStatistics {
    if stats.logical_type().id() != LogicalTypeId::Struct {
        return stats;
    }
    stats
        .struct_children()
        .and_then(|children| children.get(TYPED_VALUE_INDEX))
        .unwrap_or(stats)
}

/// `VariantStats::GetUntypedStats`: the statistics of `untyped_value_index`, if the level
/// has one.
fn untyped_stats(stats: &BaseStatistics) -> Option<&BaseStatistics> {
    if stats.logical_type().id() != LogicalTypeId::Struct {
        return None;
    }
    stats.struct_children()?.get(UNTYPED_VALUE_INDEX)
}

/// `VariantShreddedStats::IsFullyShredded`: whether every value of the level is a typed
/// one (or a VARIANT NULL, or a missing OBJECT field).
pub fn is_fully_shredded(stats: &BaseStatistics) -> bool {
    if !stats.logical_type().is_nested() {
        return true;
    }
    let Some(untyped) = untyped_stats(stats) else {
        // No untyped values: fully shredded.
        return true;
    };
    if !typed_stats(stats).can_have_null() {
        return true;
    }
    if !untyped.can_have_no_null() {
        // Every untyped value is a VARIANT NULL.
        return true;
    }
    let Some(numeric) = untyped.numeric() else {
        return false;
    };
    match (numeric.min, numeric.max) {
        // 0 is reserved for missing values (a field absent from its OBJECT).
        (Some(NumericValue::U32(0)), Some(NumericValue::U32(0))) => true,
        _ => false,
    }
}

/// `VariantShreddedStats::FindChildStats`: the statistics of the element (by index) or
/// field (by name, case-insensitively) of a shredded level.
pub fn find_child_stats<'a>(
    stats: &'a BaseStatistics,
    component: &VariantPathComponent,
) -> Option<&'a BaseStatistics> {
    let typed = if stats.logical_type().is_nested() {
        typed_stats(stats)
    } else {
        stats
    };
    match component {
        VariantPathComponent::Index(_) => {
            if typed.logical_type().id() != LogicalTypeId::List {
                return None;
            }
            typed.child()
        }
        VariantPathComponent::Key(key) => {
            if typed.logical_type().id() != LogicalTypeId::Struct {
                return None;
            }
            let fields = typed.logical_type().struct_children()?;
            let index = fields
                .iter()
                .position(|(name, _)| name.eq_ignore_ascii_case(key))?;
            typed.struct_children()?.get(index)
        }
    }
}

/// `WrapTypedValue`: a shredded level of `typed` values with `untyped` statistics; a
/// primitive level without untyped values is the typed statistics themselves.
fn wrap_typed_value(typed: BaseStatistics, untyped: Option<BaseStatistics>) -> BaseStatistics {
    if untyped.is_none() && !typed.logical_type().is_nested() {
        return typed;
    }
    let mut fields = vec![("typed_value".into(), typed.logical_type().clone())];
    if let Some(untyped) = &untyped {
        fields.push(("untyped_value_index".into(), untyped.logical_type().clone()));
    }
    let mut wrapped = BaseStatistics::create_empty(LogicalType::struct_type(fields));
    if let Some(children) = wrapped.struct_children_mut() {
        children[TYPED_VALUE_INDEX] = typed;
        if let Some(untyped) = untyped {
            children[UNTYPED_VALUE_INDEX] = untyped;
        }
    }
    wrapped
}

/// `VariantStats::MergeShredding`: the statistics of two shredded levels merged, or
/// `None` when their shreddings are incompatible. OBJECTs keep the fields both shred on
/// the same type.
fn merge_shredding(stats: &BaseStatistics, other: &BaseStatistics) -> Option<BaseStatistics> {
    let typed = typed_stats(stats);
    let other_typed = typed_stats(other);
    let untyped = match (untyped_stats(stats), untyped_stats(other)) {
        (Some(own), Some(theirs)) => {
            let mut merged = own.clone();
            merged.merge(theirs);
            Some(merged)
        }
        (own, theirs) => own.or(theirs).cloned(),
    };
    let typed_type = typed.logical_type();
    let other_type = other_typed.logical_type();
    match typed_type.id() {
        LogicalTypeId::Struct => {
            if other_type.id() != LogicalTypeId::Struct {
                return None;
            }
            let fields = typed_type.struct_children().unwrap_or_default();
            let other_fields = other_type.struct_children().unwrap_or_default();
            let field_stats = typed.struct_children().unwrap_or_default();
            let other_field_stats = other_typed.struct_children().unwrap_or_default();
            let mut new_fields = Vec::new();
            let mut new_stats = Vec::new();
            for ((name, field_type), field) in fields.iter().zip(field_stats) {
                // The first field of `other` with this name, case-insensitively.
                let Some(other_index) = other_fields
                    .iter()
                    .position(|(other_name, _)| other_name.eq_ignore_ascii_case(name))
                else {
                    continue;
                };
                if other_fields[other_index].1.id() != field_type.id() {
                    continue;
                }
                let Some(merged) = other_field_stats
                    .get(other_index)
                    .and_then(|other_field| merge_shredding(field, other_field))
                else {
                    continue;
                };
                new_fields.push((name.clone(), merged.logical_type().clone()));
                new_stats.push(merged);
            }
            if new_fields.is_empty() {
                // No fields left: demoted to unshredded.
                return None;
            }
            let mut new_typed = BaseStatistics::create_empty(LogicalType::struct_type(new_fields));
            if let Some(children) = new_typed.struct_children_mut() {
                for (child, stats) in children.iter_mut().zip(new_stats) {
                    *child = stats;
                }
            }
            new_typed.combine_validity(typed, other_typed);
            Some(wrap_typed_value(new_typed, untyped))
        }
        LogicalTypeId::List => {
            if other_type.id() != LogicalTypeId::List {
                return None;
            }
            let merged_child = merge_shredding(typed.child()?, other_typed.child()?)?;
            let mut new_typed =
                BaseStatistics::create_empty(LogicalType::list(merged_child.logical_type().clone()));
            new_typed.combine_validity(typed, other_typed);
            if let Some(child) = new_typed.child_mut() {
                *child = merged_child;
            }
            Some(wrap_typed_value(new_typed, untyped))
        }
        _ => {
            if typed_type.id() != other_type.id() {
                return None;
            }
            let mut merged = typed.clone();
            merged.merge(other_typed);
            Some(wrap_typed_value(merged, untyped))
        }
    }
}

impl BaseStatistics {
    /// `VariantStats::CreateShredded`: empty VARIANT statistics of data shredded on
    /// `shredded_type`.
    pub fn create_shredded_variant(shredded_type: LogicalType) -> Self {
        let mut stats = Self::create_empty(LogicalType::variant());
        if let Some(variant) = stats.variant_mut() {
            variant.set_shredded_stats(Self::create_empty(shredded_type));
        }
        stats
    }

    /// The VARIANT part of VARIANT statistics.
    pub fn variant(&self) -> Option<&VariantStats> {
        match &self.data {
            StatsData::Variant(variant) => Some(variant),
            _ => None,
        }
    }

    pub fn variant_mut(&mut self) -> Option<&mut VariantStats> {
        match &mut self.data {
            StatsData::Variant(variant) => Some(variant),
            _ => None,
        }
    }

    /// `VariantStats::WrapExtractedFieldAsVariant`: these VARIANT statistics with the
    /// shredded statistics of an extracted field instead of the whole value's.
    #[must_use]
    pub fn wrap_extracted_field_as_variant(&self, field: &Self) -> Self {
        let mut result = self.clone();
        if let Some(variant) = result.variant_mut() {
            variant.shredded = Some(field.clone());
        }
        result
    }

    /// `CombineValidity`: the NULL flags of either of two statistics.
    pub fn combine_validity(&mut self, left: &Self, right: &Self) {
        self.has_null = left.has_null || right.has_null;
        self.has_no_null = left.has_no_null || right.has_no_null;
    }
}

/// `VariantStats::Serialize`.
pub(super) fn write_variant_stats(
    serializer: &mut dyn roastedduck_serde::Serializer,
    variant: &VariantStats,
) -> Result<()> {
    let state = variant.shredding_state;
    serializer.on_property_begin(200, "shredding_state");
    roastedduck_serde::write_enum(serializer, state.name(), |s| s.write_u8(state as u8));
    serializer.on_property_end();
    serializer.write_object_property(225, "unshredded_stats", |s| {
        variant.unshredded.serialize_fields(s)
    })?;
    if let Some(shredded) = variant.shredded_stats() {
        serializer.write_property(230, "shredded_type", shredded.logical_type())?;
        serializer.write_object_property(235, "shredded_stats", |s| shredded.serialize_fields(s))?;
    }
    Ok(())
}

/// `VariantStats::Deserialize`.
pub(super) fn read_variant_stats(
    deserializer: &mut dyn roastedduck_serde::Deserializer,
    variant: &mut VariantStats,
) -> Result<()> {
    deserializer.on_property_begin(200, "shredding_state")?;
    variant.shredding_state = roastedduck_serde::read_enum(
        deserializer,
        "VariantStatsShreddingState",
        VariantShreddingState::from_name,
        |d| Ok(VariantShreddingState::from_u8(d.read_u8()?)),
    )?;
    deserializer.on_property_end()?;
    let unshredded = unshredded_type();
    variant.unshredded = deserializer.read_object_property(225, "unshredded_stats", |d| {
        BaseStatistics::deserialize_fields(d, &unshredded)
    })?;
    if !variant.is_shredded() {
        return Ok(());
    }
    let shredded_type: LogicalType = deserializer.read_property(230, "shredded_type")?;
    variant.shredded = Some(deserializer.read_object_property(235, "shredded_stats", |d| {
        BaseStatistics::deserialize_fields(d, &shredded_type)
    })?);
    Ok(())
}

impl VariantStats {
    pub fn to_bytes(&self) -> ByteString {
        let mut out = ByteString::new();
        let _ = self.write_statistics_bytes(&mut out);
        out
    }
}
impl roastedduck_common::byte_format::NativeByteFormat for VariantStats {
    fn append_native_bytes(&self, output: &mut ByteString) {
        let _ = self.write_statistics_bytes(output);
    }
}
impl fmt::Display for VariantStats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let bytes = self.to_bytes();
        f.write_str(bytes.to_str().map_err(|_| fmt::Error)?)
    }
}
