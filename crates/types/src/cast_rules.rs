//! Implicit cast costs and type unification (DuckDB's `CastRules` and `MaxLogicalType`).
//!
//! These rules only look at types, so they live next to [`LogicalType`]; the binder uses
//! them for overload resolution and to find the common type of expressions.

use crate::{LogicalType, LogicalTypeId, Value};

/// The preference of a target type when several implicit casts are possible
/// (DuckDB's `TargetTypeCost`). Lower is preferred.
fn target_type_cost(ty: &LogicalType) -> i64 {
    use LogicalTypeId as L;
    match ty.id() {
        L::BigInt => 101,
        L::Integer => 102,
        L::HugeInt => 103,
        L::Double => 104,
        L::Decimal => 105,
        L::Bignum => 106,
        L::TimestampNs => 119,
        L::Timestamp => 120,
        L::TimestampMs => 121,
        L::TimestampSec => 122,
        L::TimestampTz => 123,
        L::Varchar => 149,
        L::Struct | L::Map | L::List | L::Union | L::Array => 160,
        // `ANY` without a custom cast score.
        L::Any => 5,
        // Anything can be cast to a template type, but anything else is preferred.
        L::Template => 1_000_000,
        _ => 110,
    }
}

/// The targets a non-nested source type can be implicitly cast to.
fn implicit_targets(from: LogicalTypeId) -> &'static [LogicalTypeId] {
    use LogicalTypeId as L;
    match from {
        L::TinyInt => &[
            L::SmallInt,
            L::Integer,
            L::BigInt,
            L::HugeInt,
            L::Float,
            L::Double,
            L::Decimal,
            L::Bignum,
        ],
        L::SmallInt => &[
            L::Integer,
            L::BigInt,
            L::HugeInt,
            L::Float,
            L::Double,
            L::Decimal,
            L::Bignum,
        ],
        L::Integer => &[L::BigInt, L::HugeInt, L::Float, L::Double, L::Decimal, L::Bignum],
        L::BigInt => &[L::Float, L::Double, L::HugeInt, L::Decimal, L::Bignum],
        L::UTinyInt => &[
            L::USmallInt,
            L::UInteger,
            L::UBigInt,
            L::SmallInt,
            L::Integer,
            L::BigInt,
            L::HugeInt,
            L::UHugeInt,
            L::Float,
            L::Double,
            L::Decimal,
            L::Bignum,
        ],
        L::USmallInt => &[
            L::UInteger,
            L::UBigInt,
            L::Integer,
            L::BigInt,
            L::HugeInt,
            L::UHugeInt,
            L::Float,
            L::Double,
            L::Decimal,
            L::Bignum,
        ],
        L::UInteger => &[
            L::UBigInt,
            L::BigInt,
            L::UHugeInt,
            L::HugeInt,
            L::Float,
            L::Double,
            L::Decimal,
            L::Bignum,
        ],
        L::UBigInt => &[
            L::Float,
            L::Double,
            L::UHugeInt,
            L::HugeInt,
            L::Decimal,
            L::Bignum,
        ],
        L::HugeInt | L::UHugeInt => &[L::Float, L::Double, L::Decimal, L::Bignum],
        L::Float => &[L::Bignum, L::Double],
        L::Double => &[L::Bignum],
        L::Decimal => &[L::Float, L::Double],
        L::Date => &[
            L::Timestamp,
            L::TimestampTz,
            L::TimestampMs,
            L::TimestampNs,
            L::TimestampSec,
        ],
        L::Enum => &[L::Varchar],
        L::TimestampSec => &[L::Timestamp, L::TimestampMs, L::TimestampNs],
        L::TimestampMs => &[L::Timestamp, L::TimestampNs],
        L::TimestampNs => &[L::Timestamp],
        L::Timestamp => &[L::TimestampNs, L::TimestampTz],
        L::Bignum => &[L::Double],
        _ => &[],
    }
}

/// Whether an `INTEGER_LITERAL` fits into `target` (DuckDB's `IntegerLiteral::FitsInType`).
fn integer_literal_fits(constant: &Value, target: &LogicalType) -> bool {
    use LogicalTypeId as L;
    let Some(value) = constant.as_i128() else {
        // Only a UHUGEINT constant does not fit an i128.
        return matches!(target.id(), L::Float | L::Double | L::UHugeInt);
    };
    match target.id() {
        L::Float | L::Double | L::HugeInt => true,
        L::TinyInt => i8::try_from(value).is_ok(),
        L::SmallInt => i16::try_from(value).is_ok(),
        L::Integer => i32::try_from(value).is_ok(),
        L::BigInt => i64::try_from(value).is_ok(),
        L::UTinyInt => u8::try_from(value).is_ok(),
        L::USmallInt => u16::try_from(value).is_ok(),
        L::UInteger => u32::try_from(value).is_ok(),
        L::UBigInt => u64::try_from(value).is_ok(),
        L::UHugeInt => value >= 0,
        _ => false,
    }
}

fn contains_template(ty: &LogicalType) -> bool {
    if ty.id() == LogicalTypeId::Template {
        return true;
    }
    ty.child_type().is_some_and(contains_template)
        || ty
            .array_child()
            .is_some_and(|(child, _)| contains_template(child))
        || ty
            .struct_children()
            .is_some_and(|children| children.iter().any(|(_, child)| contains_template(child)))
}

/// The cost of implicitly casting `from` to `to`, or `-1` if no implicit cast exists
/// (DuckDB's `CastRules::ImplicitCast`). The binder picks the overload with the lowest
/// total cost.
pub fn implicit_cast_cost(from: &LogicalType, to: &LogicalType) -> i64 {
    use LogicalTypeId as L;
    if from.id() == L::SqlNull && to.id() == L::Template {
        // Prefer the TEMPLATE type for NULL casts, as it is the most generic.
        return 5;
    }
    if from.id() == L::SqlNull || to.id() == L::Any || to.id() == L::Template {
        return target_type_cost(to);
    }
    if from.id() == L::Any && contains_template(to) {
        return target_type_cost(to);
    }
    if from.id() == L::Unknown {
        // Parameters can be cast to anything for free.
        return 0;
    }
    if from.id() == L::StringLiteral {
        if !to.is_valid() {
            return -1;
        }
        return if to.id() == L::Varchar && to.alias().is_none() {
            1
        } else {
            20
        };
    }
    if let (L::IntegerLiteral, Some(value), Some(literal_type)) = (
        from.id(),
        from.integer_literal_value(),
        from.integer_literal_type(),
    ) {
        if literal_type.id() == to.id() {
            return 0;
        }
        if integer_literal_fits(value, to) {
            // Prefer BIGINT, INTEGER, ... to avoid ties.
            return target_type_cost(to) - 90;
        }
        return implicit_cast_cost(literal_type, to);
    }
    if from.alias() != to.alias() {
        // Aliased types (e.g. `JSON`) only cast implicitly to the same alias.
        return -1;
    }
    match (from.id(), to.id()) {
        (L::List, L::List) => {
            let (Some(from_child), Some(to_child)) = (from.child_type(), to.child_type()) else {
                return -1;
            };
            let cost = implicit_cast_cost(from_child, to_child);
            // LIST[X] -> LIST[VARCHAR] is preferred over LIST[X] -> VARCHAR.
            return if cost >= 1 { cost - 1 } else { cost };
        }
        (L::Array, L::Array) => {
            let (Some((from_child, from_size)), Some((to_child, to_size))) =
                (from.array_child(), to.array_child())
            else {
                return -1;
            };
            if from_size != to_size && to_size != 0 {
                return -1;
            }
            let cost = implicit_cast_cost(from_child, to_child);
            return if cost >= 100 { cost - 1 } else { cost };
        }
        (L::Array, L::List) => {
            let (Some((from_child, _)), Some(to_child)) = (from.array_child(), to.child_type()) else {
                return -1;
            };
            let cost = implicit_cast_cost(from_child, to_child);
            // ARRAY -> ARRAY is preferred over ARRAY -> LIST.
            return if cost < 0 { -1 } else { cost + 1 };
        }
        (L::List, L::Array) if to.array_child().is_some_and(|(_, size)| size != 0) => {
            let (Some(from_child), Some((to_child, _))) = (from.child_type(), to.array_child()) else {
                return -1;
            };
            return implicit_cast_cost(from_child, to_child);
        }
        (L::Union, L::Union) => return union_to_union_cost(from, to),
        (L::Struct, L::Struct) => return struct_to_struct_cost(from, to),
        _ => {}
    }
    if from.id() == to.id() {
        return 0;
    }
    if to.id() == L::Union {
        // Anything can be cast to a union that has a member it can be cast to; the cheapest
        // (most specific) member wins.
        let Some(members) = to.union_members() else {
            return -1;
        };
        return members
            .iter()
            .map(|(_, member)| implicit_cast_cost(from, member))
            .filter(|&cost| cost != -1)
            .min()
            .unwrap_or(-1);
    }
    if from.id() == L::Variant {
        return target_type_cost(to);
    }
    if implicit_targets(from.id()).contains(&to.id()) {
        target_type_cost(to)
    } else {
        -1
    }
}

/// UNION -> UNION: every source member must exist in the target (case-insensitively);
/// the most expensive member cast determines the cost.
fn union_to_union_cost(from: &LogicalType, to: &LogicalType) -> i64 {
    let Some(to_members) = to.union_members() else {
        // Unresolved target unions are left to the cast itself.
        return 0;
    };
    let Some(from_members) = from.union_members() else {
        return -1;
    };
    let mut cost = -1;
    for (from_name, from_type) in from_members {
        let Some((_, to_type)) = to_members
            .iter()
            .find(|(to_name, _)| to_name.eq_ignore_ascii_case(from_name))
        else {
            return -1;
        };
        cost = cost.max(implicit_cast_cost(from_type, to_type));
    }
    cost
}

/// STRUCT -> STRUCT: fields are matched by name (case-insensitively) when both sides are
/// named, by position otherwise; the most expensive field cast determines the cost.
fn struct_to_struct_cost(from: &LogicalType, to: &LogicalType) -> i64 {
    let Some(target) = to.struct_children() else {
        return 0;
    };
    let Some(source) = from.struct_children() else {
        return -1;
    };
    if source.len() != target.len() {
        return -1;
    }
    let mut cost = -1;
    if !from.is_unnamed_struct() && !to.is_unnamed_struct() {
        let mut remaining: Vec<Option<&(String, LogicalType)>> = Vec::with_capacity(target.len());
        for (i, field) in target.iter().enumerate() {
            if target[..i]
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case(&field.0))
            {
                // Duplicate name in the target struct.
                return -1;
            }
            remaining.push(Some(field));
        }
        for (name, source_type) in source {
            let Some(slot) = remaining
                .iter_mut()
                .find(|slot| slot.is_some_and(|(target_name, _)| target_name.eq_ignore_ascii_case(name)))
            else {
                return -1;
            };
            let Some((_, target_type)) = slot.take() else {
                return -1;
            };
            let child_cost = implicit_cast_cost(source_type, target_type);
            if child_cost == -1 {
                return -1;
            }
            cost = cost.max(child_cost);
        }
    } else {
        for ((_, source_type), (_, target_type)) in source.iter().zip(target) {
            let child_cost = implicit_cast_cost(source_type, target_type);
            if child_cost == -1 {
                return -1;
            }
            cost = cost.max(child_cost);
        }
    }
    cost
}

/// Whether to fail or force a result when two types cannot be unified.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Unify {
    Try,
    Force,
}

/// Widens a DECIMAL so that it also holds every value of the integral type `other`
/// (DuckDB's `DecimalSizeCheck`).
fn decimal_size_check(other: &LogicalType, decimal: &LogicalType) -> LogicalType {
    let (Some((width, scale)), Some((other_width, _))) =
        (decimal.decimal_properties(), other.numeric_decimal_properties())
    else {
        return decimal.clone();
    };
    if other_width > width - scale {
        let new_width = (other_width + scale).min(LogicalType::MAX_DECIMAL_WIDTH);
        return LogicalType::decimal(new_width, scale);
    }
    decimal.clone()
}

/// The common type of two numeric types neither of which casts implicitly to the other,
/// e.g. INTEGER and UINTEGER (DuckDB's `CombineNumericTypes`).
fn combine_numeric_types(left: &LogicalType, right: &LogicalType) -> Option<LogicalType> {
    use LogicalTypeId as L;
    if left.id() > right.id() {
        return combine_numeric_types(right, left);
    }
    let (l, r) = (left.id(), right.id());
    Some(if l == L::BigInt || r == L::UBigInt {
        LogicalType::HUGEINT
    } else if l == L::Integer || r == L::UInteger {
        LogicalType::BIGINT
    } else if l == L::SmallInt || r == L::USmallInt {
        LogicalType::INTEGER
    } else if l == L::TinyInt || r == L::UTinyInt {
        LogicalType::SMALLINT
    } else if l == L::UHugeInt || r == L::HugeInt {
        LogicalType::DOUBLE
    } else {
        return None;
    })
}

/// STRING/INTEGER literals take their natural type once they meet another type.
fn normalize(ty: &LogicalType) -> LogicalType {
    match ty.id() {
        LogicalTypeId::StringLiteral => LogicalType::VARCHAR,
        LogicalTypeId::IntegerLiteral => ty.integer_literal_type().cloned().unwrap_or(LogicalType::BIGINT),
        _ => ty.clone(),
    }
}

fn unify_child(left: &LogicalType, right: &LogicalType, mode: Unify) -> Option<LogicalType> {
    match mode {
        Unify::Try => unify(left, right, mode),
        Unify::Force => Some(LogicalType::force_max_logical_type(left, right)),
    }
}

fn combine_unequal(left: &LogicalType, right: &LogicalType, mode: Unify) -> Option<LogicalType> {
    use LogicalTypeId as L;
    if right.id() == L::Variant {
        return Some(right.clone());
    }
    if left.id() == L::Variant {
        return Some(left.clone());
    }
    for other in [L::SqlNull, L::Unknown] {
        if left.id() == other {
            return Some(normalize(right));
        }
        if right.id() == other {
            return Some(normalize(left));
        }
    }
    // ENUMs follow the VARCHAR rules.
    if left.id() == L::Enum {
        return unify_child(&LogicalType::VARCHAR, right, mode);
    }
    if right.id() == L::Enum {
        return unify_child(left, &LogicalType::VARCHAR, mode);
    }
    if left.id() == L::StringLiteral {
        return Some(normalize(right));
    }
    if right.id() == L::StringLiteral {
        return Some(normalize(left));
    }
    let left_to_right = implicit_cast_cost(left, right);
    let right_to_left = implicit_cast_cost(right, left);
    if left_to_right >= 0 && (left_to_right < right_to_left || right_to_left < 0) {
        return Some(if right.id() == L::Decimal {
            decimal_size_check(left, right)
        } else {
            right.clone()
        });
    }
    if right_to_left >= 0 {
        return Some(if left.id() == L::Decimal {
            decimal_size_check(right, left)
        } else {
            left.clone()
        });
    }
    if left.id() == L::IntegerLiteral {
        return unify_child(&normalize(left), right, mode);
    }
    if right.id() == L::IntegerLiteral {
        return unify_child(left, &normalize(right), mode);
    }
    if left.is_numeric() && right.is_numeric() {
        return combine_numeric_types(left, right);
    }
    if left.id() == L::Boolean && right.is_integral() {
        return Some(right.clone());
    }
    if right.id() == L::Boolean && left.is_integral() {
        return Some(left.clone());
    }
    None
}

fn combine_structs(left: &LogicalType, right: &LogicalType, mode: Unify) -> Option<LogicalType> {
    let left_children = left.struct_children()?;
    let right_children = right.struct_children()?;
    let left_unnamed = left.is_unnamed_struct();
    let mut children = Vec::new();
    if left_unnamed || right.is_unnamed_struct() {
        // Positional: both sides need the same number of fields.
        if left_children.len() != right_children.len() {
            return None;
        }
        for ((left_name, left_type), (right_name, right_type)) in left_children.iter().zip(right_children) {
            let child = unify_child(left_type, right_type, mode)?;
            let name = if left_unnamed { right_name } else { left_name };
            children.push((name.clone(), child));
        }
        return Some(LogicalType::struct_type(children));
    }
    // By name: the super-set of both field lists, left fields first.
    let mut right_used = vec![false; right_children.len()];
    for (name, left_type) in left_children {
        match right_children
            .iter()
            .position(|(right_name, _)| right_name == name)
        {
            Some(index) => {
                right_used[index] = true;
                children.push((
                    name.clone(),
                    unify_child(left_type, &right_children[index].1, mode)?,
                ));
            }
            None => children.push((name.clone(), left_type.clone())),
        }
    }
    for (used, child) in right_used.into_iter().zip(right_children) {
        if !used {
            children.push(child.clone());
        }
    }
    Some(LogicalType::struct_type(children))
}

fn combine_equal(left: &LogicalType, right: &LogicalType, mode: Unify) -> Option<LogicalType> {
    use LogicalTypeId as L;
    match left.id() {
        L::StringLiteral => Some(LogicalType::VARCHAR),
        L::IntegerLiteral => unify_child(&normalize(left), &normalize(right), mode),
        L::Enum => Some(if left == right {
            left.clone()
        } else {
            LogicalType::VARCHAR
        }),
        L::Decimal => {
            let ((left_width, left_scale), (right_width, right_scale)) =
                (left.decimal_properties()?, right.decimal_properties()?);
            let extra_width = (left_width - left_scale).max(right_width - right_scale);
            let mut scale = left_scale.max(right_scale);
            let mut width = extra_width + scale;
            if width > LogicalType::MAX_DECIMAL_WIDTH {
                // The scale is truncated so that the integral digits always fit.
                width = LogicalType::MAX_DECIMAL_WIDTH;
                scale = width - extra_width;
            }
            Some(LogicalType::decimal(width, scale))
        }
        L::List => Some(LogicalType::list(unify_child(
            left.child_type()?,
            right.child_type()?,
            mode,
        )?)),
        L::Array => {
            let ((left_child, left_size), (right_child, right_size)) =
                (left.array_child()?, right.array_child()?);
            Some(LogicalType::array(
                unify_child(left_child, right_child, mode)?,
                left_size.max(right_size),
            ))
        }
        L::Map => {
            let entry = unify_child(left.child_type()?, right.child_type()?, mode)?;
            Some(LogicalType::with_info(
                L::Map,
                crate::TypeInfo::List(crate::ListTypeInfo { child_type: entry }),
            ))
        }
        L::Struct => combine_structs(left, right, mode),
        L::Union => {
            let left_count = left.union_members().map_or(0, <[_]>::len);
            let right_count = right.union_members().map_or(0, <[_]>::len);
            Some(if right_count > left_count {
                right.clone()
            } else {
                left.clone()
            })
        }
        _ => Some(left.clone()),
    }
}

fn unify(left: &LogicalType, right: &LogicalType, mode: Unify) -> Option<LogicalType> {
    // Aliased types are always preferred.
    if left.alias().is_some() {
        return Some(left.clone());
    }
    if right.alias().is_some() {
        return Some(right.clone());
    }
    if left.id() == right.id() {
        combine_equal(left, right, mode)
    } else {
        combine_unequal(left, right, mode)
    }
}

/// DuckDB's `GetLogicalTypeScore`: the tie-breaker of [`LogicalType::force_max_logical_type`].
fn logical_type_score(ty: &LogicalType) -> u32 {
    use LogicalTypeId as L;
    match ty.id() {
        L::Invalid
        | L::SqlNull
        | L::Unknown
        | L::Any
        | L::Template
        | L::StringLiteral
        | L::IntegerLiteral => 0,
        L::Boolean => 10,
        L::UTinyInt => 11,
        L::TinyInt => 12,
        L::USmallInt => 13,
        L::SmallInt => 14,
        L::UInteger => 15,
        L::Integer => 16,
        L::UBigInt => 17,
        L::BigInt => 18,
        L::UHugeInt => 19,
        L::HugeInt => 20,
        L::Decimal => 21,
        L::Float => 22,
        L::Double => 23,
        L::Time | L::TimeTz => 50,
        L::TimeNs => 51,
        L::Date => 52,
        L::TimestampSec => 53,
        L::TimestampMs => 54,
        L::Timestamp | L::TimestampTz => 55,
        L::TimestampNs => 56,
        L::Interval => 58,
        L::Char => 75,
        L::Varchar => 77,
        L::Enum => 78,
        L::Bit => 100,
        L::Blob => 101,
        L::Uuid => 102,
        L::Bignum => 103,
        L::Geometry => 104,
        L::Struct => 125,
        L::List | L::Array => 126,
        L::Map => 127,
        L::Variant | L::Union | L::Table => 150,
        L::Lambda | L::AggregateState | L::Pointer | L::Validity | L::Unbound | L::Type => 1000,
    }
}

impl LogicalType {
    /// The type both `left` and `right` can be implicitly cast to, if any
    /// (DuckDB's `TryGetMaxLogicalType` without the `old_implicit_casting` setting).
    pub fn try_max_logical_type(left: &LogicalType, right: &LogicalType) -> Option<LogicalType> {
        unify(left, right, Unify::Try)
    }

    /// Like [`LogicalType::try_max_logical_type`], but always produces a type, falling back
    /// to the "larger" of the two (DuckDB's `ForceMaxLogicalType`).
    pub fn force_max_logical_type(left: &LogicalType, right: &LogicalType) -> LogicalType {
        unify(left, right, Unify::Force).unwrap_or_else(|| {
            if logical_type_score(left) < logical_type_score(right) {
                right.clone()
            } else {
                left.clone()
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_costs_follow_duckdb() {
        assert_eq!(
            implicit_cast_cost(&LogicalType::INTEGER, &LogicalType::BIGINT),
            101
        );
        assert_eq!(
            implicit_cast_cost(&LogicalType::INTEGER, &LogicalType::DOUBLE),
            104
        );
        assert_eq!(
            implicit_cast_cost(&LogicalType::BIGINT, &LogicalType::INTEGER),
            -1
        );
        assert_eq!(
            implicit_cast_cost(&LogicalType::INTEGER, &LogicalType::INTEGER),
            0
        );
        assert_eq!(
            implicit_cast_cost(&LogicalType::STRING_LITERAL, &LogicalType::VARCHAR),
            1
        );
        assert_eq!(
            implicit_cast_cost(&LogicalType::STRING_LITERAL, &LogicalType::DATE),
            20
        );
        assert_eq!(implicit_cast_cost(&LogicalType::SQLNULL, &LogicalType::DATE), 110);
        let literal = LogicalType::integer_literal(Value::integer(5));
        assert_eq!(implicit_cast_cost(&literal, &LogicalType::INTEGER), 0);
        assert_eq!(implicit_cast_cost(&literal, &LogicalType::TINYINT), 20);
        assert_eq!(implicit_cast_cost(&literal, &LogicalType::BIGINT), 11);
        let big = LogicalType::integer_literal(Value::integer(1000));
        assert_eq!(implicit_cast_cost(&big, &LogicalType::TINYINT), -1);
        assert_eq!(
            implicit_cast_cost(
                &LogicalType::list(LogicalType::INTEGER),
                &LogicalType::list(LogicalType::VARCHAR)
            ),
            -1
        );
        assert_eq!(
            implicit_cast_cost(
                &LogicalType::list(LogicalType::INTEGER),
                &LogicalType::list(LogicalType::BIGINT)
            ),
            100
        );
    }

    #[test]
    fn aliases_only_cast_to_themselves() {
        let json = LogicalType::VARCHAR.with_alias("JSON");
        assert_eq!(implicit_cast_cost(&json, &LogicalType::VARCHAR), -1);
        assert_eq!(implicit_cast_cost(&json, &json), 0);
        assert_eq!(implicit_cast_cost(&LogicalType::STRING_LITERAL, &json), 20);
        assert_eq!(
            LogicalType::try_max_logical_type(&LogicalType::VARCHAR, &json),
            Some(json)
        );
    }

    #[test]
    fn max_logical_type_unifies() {
        let max = LogicalType::try_max_logical_type;
        assert_eq!(
            max(&LogicalType::INTEGER, &LogicalType::BIGINT),
            Some(LogicalType::BIGINT)
        );
        assert_eq!(
            max(&LogicalType::INTEGER, &LogicalType::UINTEGER),
            Some(LogicalType::BIGINT)
        );
        assert_eq!(
            max(&LogicalType::SQLNULL, &LogicalType::DATE),
            Some(LogicalType::DATE)
        );
        assert_eq!(
            max(&LogicalType::INTEGER, &LogicalType::decimal(4, 2)),
            Some(LogicalType::decimal(12, 2))
        );
        assert_eq!(
            max(&LogicalType::decimal(10, 2), &LogicalType::decimal(5, 4)),
            Some(LogicalType::decimal(12, 4))
        );
        assert_eq!(max(&LogicalType::DATE, &LogicalType::INTEGER), None);
        assert_eq!(
            LogicalType::force_max_logical_type(&LogicalType::DATE, &LogicalType::INTEGER),
            LogicalType::DATE
        );
    }
}
