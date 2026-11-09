//! The transform functions of DuckDB's `transform_common.cpp`: identifiers, literals and
//! type names. Types are built as `TypeExpression`s and wrapped as UNBOUND types, which
//! hold the expression as an [`UnboundTypeInfo`].

use roastedduck_function::cast::ValueCast;
use roastedduck_types::{TypeInfo, TypeParameter, TypeParameterValue, UnboundTypeInfo};

use super::prelude::*;

/// `Decimal::MAX_WIDTH_DECIMAL`.
const MAX_WIDTH_DECIMAL: usize = 38;

/// `TypeExpression(catalog, schema, name, children)`.
pub(super) fn type_expression(
    catalog: impl Into<ByteString>,
    schema: impl Into<ByteString>,
    name: impl Into<ByteString>,
    children: Vec<ParsedExpression>,
) -> ParsedExpression {
    ParsedExpression::new(
        ExpressionType::Type,
        ParsedExpressionKind::Type(Box::new(TypeExpression {
            catalog: catalog.into(),
            schema: schema.into(),
            type_name: name.into(),
            children,
        })),
    )
}

/// `TypeExpression(name, children)`.
pub(super) fn simple_type_expression(name: &str, children: Vec<ParsedExpression>) -> ParsedExpression {
    type_expression("", "", name, children)
}

/// `LogicalType::UNBOUND(type_expression)`: the type expression as the parse tree's
/// [`UnboundTypeInfo`].
pub(super) fn unbound_type(expression: ParsedExpression) -> Result<LogicalType> {
    Ok(LogicalType::with_info(
        LogicalTypeId::Unbound,
        TypeInfo::Unbound(unbound_type_info(expression)?),
    ))
}

/// The [`UnboundTypeInfo`] of a type expression: its constant children are type
/// parameters, its type children nested types; aliases name the parameters.
fn unbound_type_info(expression: ParsedExpression) -> Result<UnboundTypeInfo> {
    let query_location = expression.query_location;
    let ParsedExpressionKind::Type(ty) = expression.kind else {
        return Err(Error::internal(format!(
            "Expected a type expression, found {:?}",
            expression.expression_class()
        )));
    };
    let parameters = ty
        .children
        .into_iter()
        .map(|child| {
            let name = child.alias.clone();
            let query_location = child.query_location;
            let value = match child.kind {
                ParsedExpressionKind::Constant(constant) => TypeParameterValue::Constant(constant.value),
                ParsedExpressionKind::Type(_) => TypeParameterValue::Type(unbound_type_info(child)?),
                _ => {
                    return Err(Error::parser("Expected a constant or a type as type parameter"));
                }
            };
            Ok(TypeParameter {
                name,
                value,
                query_location,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(UnboundTypeInfo {
        catalog: ty.catalog,
        schema: ty.schema,
        name: ty.type_name,
        parameters,
        query_location,
    })
}

/// `UnboundType::GetTypeExpression(type)->Copy()`: the type expression of an UNBOUND
/// type.
pub(super) fn type_expression_of(ty: &LogicalType) -> Result<ParsedExpression> {
    match ty.info() {
        Some(TypeInfo::Unbound(info)) => Ok(type_expression_from_info(info)),
        _ => Err(Error::internal(format!("Expected an UNBOUND type, found {ty}"))),
    }
}

/// The type expression an [`UnboundTypeInfo`] stands for.
fn type_expression_from_info(info: &UnboundTypeInfo) -> ParsedExpression {
    let children = info
        .parameters
        .iter()
        .map(|parameter| {
            let mut child = match &parameter.value {
                TypeParameterValue::Constant(value) => ParsedExpression::constant(value.clone()),
                TypeParameterValue::Type(inner) => type_expression_from_info(inner),
            };
            child.alias.clone_from(&parameter.name);
            child.query_location = parameter.query_location;
            child
        })
        .collect();
    let mut expression = type_expression(&info.catalog, &info.schema, &info.name, children);
    expression.query_location = info.query_location;
    expression
}

/// The value of a constant expression, or the parser error `message`.
fn constant_value(expression: &ParsedExpression, message: &str) -> Result<Value> {
    match &expression.kind {
        ParsedExpressionKind::Constant(constant) => Ok(constant.value.clone()),
        _ => Err(Error::parser(message.to_owned())),
    }
}

/// `Value::GetValue<int64_t>`.
fn bigint_value(value: &Value) -> Result<i64> {
    value
        .default_cast_as(&LogicalType::BIGINT)?
        .as_i64()
        .ok_or_else(|| Error::internal("BIGINT value without payload"))
}

/// `TryNegateValue`: `-value` for an integral or double constant, widening a 32-bit
/// or 64-bit minimum; `None` if it cannot be negated.
pub(super) fn try_negate_value(value: &Value) -> Option<Value> {
    match value.logical_type().id() {
        LogicalTypeId::Integer => {
            let raw = value.as_i32()?;
            Some(
                raw.checked_neg()
                    .map_or_else(|| Value::bigint(-i64::from(raw)), Value::integer),
            )
        }
        LogicalTypeId::BigInt => {
            let raw = value.as_i64()?;
            Some(
                raw.checked_neg()
                    .map_or_else(|| Value::hugeint(-i128::from(raw)), Value::bigint),
            )
        }
        LogicalTypeId::HugeInt => value.as_i128()?.checked_neg().map(Value::hugeint),
        LogicalTypeId::UHugeInt => {
            let raw = value.as_u128()?;
            // |i128::MIN| is the largest UHUGEINT a HUGEINT can hold negated.
            let abs_min_hugeint = i128::MAX.unsigned_abs() + 1;
            match raw.cmp(&abs_min_hugeint) {
                std::cmp::Ordering::Equal => Some(Value::hugeint(i128::MIN)),
                std::cmp::Ordering::Less => Some(Value::hugeint(-(raw as i128))),
                std::cmp::Ordering::Greater => None,
            }
        }
        LogicalTypeId::Double => Some(Value::double(-value.as_f64()?)),
        _ => None,
    }
}

/// `ConvertNumberToValue`: the constant of a numeric literal: the smallest integer type
/// that holds it, a DECIMAL if it has a decimal point and at most 38 digits, else a
/// DOUBLE.
pub(super) fn convert_number_to_value(text: &str) -> Result<ParsedExpression> {
    let bytes = text.as_bytes();
    let mut try_cast_as_integer = true;
    let mut try_cast_as_decimal = true;
    let mut decimal_position = None;
    let mut num_underscores = 0;
    let mut num_integer_underscores = 0;
    for (i, &byte) in bytes.iter().enumerate() {
        match byte {
            b'.' => {
                // A decimal point: a decimal or a double.
                try_cast_as_integer = false;
                decimal_position = Some(i);
            }
            b'e' | b'E' => {
                // An exponent: a double.
                try_cast_as_integer = false;
                try_cast_as_decimal = false;
            }
            b'_' => {
                num_underscores += 1;
                if decimal_position.is_none() {
                    num_integer_underscores += 1;
                }
            }
            _ => {}
        }
    }
    let varchar = Value::varchar(text);
    if try_cast_as_integer {
        for ty in [
            LogicalType::INTEGER,
            LogicalType::BIGINT,
            LogicalType::HUGEINT,
            LogicalType::UHUGEINT,
        ] {
            if let Ok(value) = varchar.try_cast(&ty) {
                return Ok(ParsedExpression::constant(value));
            }
        }
    }
    let negative = bytes.first() == Some(&b'-');
    let decimal_offset = if negative { 3 } else { 2 };
    if try_cast_as_decimal
        && let Some(position) = decimal_position
        && bytes.len() - num_underscores < MAX_WIDTH_DECIMAL + decimal_offset
    {
        // The width and scale follow from the position of the decimal point.
        let mut width = bytes.len() - 1 - num_underscores;
        let scale = width + num_integer_underscores - position;
        if negative {
            width -= 1;
        }
        if width <= MAX_WIDTH_DECIMAL {
            let value = varchar.default_cast_as(&LogicalType::decimal(width as u8, scale as u8))?;
            return Ok(ParsedExpression::constant(value));
        }
    }
    // A decimal point with too many digits, or an integer too big for any integer type.
    let value = varchar
        .try_cast(&LogicalType::DOUBLE)
        .map_err(|_| Error::conversion(format!("Could not convert string \"{text}\" to DOUBLE")))?;
    Ok(ParsedExpression::constant(value))
}

impl PegTransformer {
    /// `TransformIdentifierOrKeyword`: the text of an identifier or keyword rule.
    pub(super) fn transform_identifier_or_keyword(&mut self, parse_result: &ParseResult) -> Result<String> {
        match &parse_result.kind {
            ParseResultKind::Identifier(identifier) => return Ok(identifier.clone()),
            ParseResultKind::Keyword(keyword) => return Ok(keyword.clone()),
            ParseResultKind::Choice { result, .. } => return self.transform(result),
            ParseResultKind::List(children) => {
                for child in children {
                    match &child.kind {
                        ParseResultKind::List(grandchildren) if grandchildren.is_empty() => {}
                        ParseResultKind::Choice { result, .. } => {
                            return match &result.kind {
                                ParseResultKind::Identifier(identifier) => Ok(identifier.clone()),
                                ParseResultKind::Keyword(keyword) => Ok(keyword.clone()),
                                _ => self.transform(result),
                            };
                        }
                        ParseResultKind::Identifier(identifier) => return Ok(identifier.clone()),
                        _ => {
                            return Err(Error::internal(format!(
                                "Unexpected IdentifierOrKeyword type encountered {}.",
                                child.result_type().name()
                            )));
                        }
                    }
                }
            }
            _ => {}
        }
        Err(Error::parser(
            "Unexpected ParseResult type in identifier transformation.",
        ))
    }

    /// `TransformType`: an UNBOUND type, with its array bounds (`INT[]`, `INT[3]`).
    pub(super) fn transform_type(&mut self, parse_result: &ParseResult) -> Result<LogicalType> {
        let list = parse_result.list()?;
        let mut ty: ParsedExpression = self.transform(list.list(0)?.choice(0)?)?;
        if let Some(array_bounds) = list.optional(1)? {
            for array_bound in array_bounds.repeat()? {
                let array_size: i64 = self.transform(array_bound)?;
                ty = if array_size < 0 {
                    simple_type_expression("list", vec![ty])
                } else {
                    simple_type_expression(
                        "array",
                        vec![ty, ParsedExpression::constant(Value::bigint(array_size))],
                    )
                };
            }
        }
        unbound_type(ty)
    }

    /// `TransformArrayBounds`: the size of `[n]`, or -1 for a list (`[]` / `ARRAY`).
    pub(super) fn transform_array_bounds(&mut self, parse_result: &ParseResult) -> Result<i64> {
        let bound = parse_result.list()?.choice(0)?;
        if bound.name.is_empty() {
            return Ok(-1);
        }
        self.transform(bound)
    }

    /// `TransformArrayKeyword`: `ARRAY` is a list.
    pub(super) fn transform_array_keyword(&mut self, _parse_result: &ParseResult) -> Result<i64> {
        Ok(-1)
    }

    /// `TransformSquareBracketsArray`: `[n]`, or -1 for `[]`.
    pub(super) fn transform_square_brackets_array(&mut self, parse_result: &ParseResult) -> Result<i64> {
        let Some(size) = parse_result.list()?.optional(1)? else {
            return Ok(-1);
        };
        let number: ParsedExpression = self.transform(size)?;
        let value = constant_value(&number, "Expected a constant number as array size")?;
        if !value.logical_type().is_integral() {
            return Err(Error::binder(format!(
                "Expected an integer as array bound instead of {}",
                value.to_varchar()?
            )));
        }
        let size = bigint_value(&value)?;
        if size < 0 {
            return Err(Error::parser("Array size must be greater than 0"));
        }
        Ok(size)
    }

    /// `TransformTimeType`: `TIME` / `TIMESTAMP [(precision)] [WITH[OUT] TIME ZONE]`.
    pub(super) fn transform_time_type(&mut self, parse_result: &ParseResult) -> Result<ParsedExpression> {
        let list = parse_result.list()?;
        let ty: LogicalTypeId = self.transform_child(list, 0)?;
        let modifiers: Vec<ParsedExpression> = self.transform_optional(list, 1)?.unwrap_or_default();
        let with_timezone: bool = self.transform_optional(list, 2)?.unwrap_or(false);
        let named = |ty: LogicalType| Ok(simple_type_expression(&ty.to_string(), Vec::new()));
        match ty {
            LogicalTypeId::Time => {
                if !modifiers.is_empty() {
                    return Err(Error::parser("Type TIME does not allow any modifiers"));
                }
                named(if with_timezone {
                    LogicalType::TIME_TZ
                } else {
                    LogicalType::TIME
                })
            }
            LogicalTypeId::Timestamp => {
                let Some(precision) = modifiers.first() else {
                    return named(if with_timezone {
                        LogicalType::TIMESTAMP_TZ
                    } else {
                        LogicalType::TIMESTAMP
                    });
                };
                if modifiers.len() > 1 {
                    return Err(Error::parser("TIMESTAMP only supports a single modifier"));
                }
                let precision = constant_value(
                    precision,
                    "Expected a constant expression for timestamp precision",
                )?;
                let precision = bigint_value(&precision)?;
                if precision > 10 {
                    return Err(Error::parser(
                        "TIMESTAMP only supports until nano-second precision (9)",
                    ));
                }
                if precision < 0 {
                    return Err(Error::parser(
                        "TIMESTAMP precision should be between 0 and 10 (inclusive)",
                    ));
                }
                named(match precision {
                    0 => LogicalType::TIMESTAMP_S,
                    1..=3 => LogicalType::TIMESTAMP_MS,
                    // Microseconds, the default TIMESTAMP.
                    4..=6 => LogicalType::TIMESTAMP,
                    _ => LogicalType::TIMESTAMP_NS,
                })
            }
            _ => Err(Error::parser("Unexpected time type encountered")),
        }
    }

    /// `TransformTimeZone`: whether `WITH TIME ZONE` (rather than `WITHOUT`).
    pub(super) fn transform_time_zone(&mut self, parse_result: &ParseResult) -> Result<bool> {
        self.transform_child(parse_result.list()?, 0)
    }

    /// `TransformWithOrWithout`.
    pub(super) fn transform_with_or_without(&mut self, parse_result: &ParseResult) -> Result<bool> {
        Self::transform_enum(parse_result.list()?.child(0)?)
    }

    /// `TransformTimeOrTimestamp`.
    pub(super) fn transform_time_or_timestamp(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<LogicalTypeId> {
        Self::transform_enum(parse_result.list()?.choice(0)?)
    }

    /// `TransformNumericType`.
    pub(super) fn transform_numeric_type(&mut self, parse_result: &ParseResult) -> Result<ParsedExpression> {
        self.transform(parse_result.list()?.choice(0)?)
    }

    /// `TransformSimpleNumericType`: `INT`, `BIGINT`, `DOUBLE`, ...
    pub(super) fn transform_simple_numeric_type(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        let numeric: String = Self::transform_enum(parse_result.list()?.choice(0)?)?;
        Ok(simple_type_expression(&numeric, Vec::new()))
    }

    /// `TransformDecimalNumericType`.
    pub(super) fn transform_decimal_numeric_type(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        self.transform(parse_result.list()?.choice(0)?)
    }

    /// `TransformFloatType`: `FLOAT [(n)]` is a FLOAT whatever the precision.
    pub(super) fn transform_float_type(&mut self, _parse_result: &ParseResult) -> Result<ParsedExpression> {
        Ok(simple_type_expression("FLOAT", Vec::new()))
    }

    /// `TransformDecimalType`: `DECIMAL` / `DEC` / `NUMERIC [(width, scale)]`.
    pub(super) fn transform_decimal_type(&mut self, parse_result: &ParseResult) -> Result<ParsedExpression> {
        let modifiers: Vec<ParsedExpression> = self
            .transform_optional(parse_result.list()?, 1)?
            .unwrap_or_default();
        Ok(simple_type_expression("DECIMAL", modifiers))
    }

    /// `TransformTypeModifiers`: the constants in `(...)` after a type name.
    pub(super) fn transform_type_modifiers(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<ParsedExpression>> {
        let mut result = Vec::new();
        if let Some(modifiers) = parse_result.list()?.list(0)?.child(1)?.optional()? {
            for modifier in modifiers.list_elements()? {
                let expression: ParsedExpression = self.transform(modifier)?;
                if !matches!(expression.kind, ParsedExpressionKind::Constant(_)) {
                    return Err(Error::parser("Expected a constant as type modifier"));
                }
                result.push(expression);
            }
        }
        Ok(result)
    }

    /// `TransformSimpleType`: a (qualified) type name or a character type, with its
    /// modifiers.
    pub(super) fn transform_simple_type(&mut self, parse_result: &ParseResult) -> Result<ParsedExpression> {
        let list = parse_result.list()?;
        let children: Vec<ParsedExpression> = self.transform_optional(list, 1)?.unwrap_or_default();
        let type_or_character = list.list(0)?.choice(0)?;
        match type_or_character.name {
            "QualifiedTypeName" => {
                let mut name: QualifiedName = self.transform(type_or_character)?;
                if name.schema.is_empty() {
                    name.schema = std::mem::take(&mut name.catalog);
                }
                Ok(type_expression(&name.catalog, &name.schema, &name.name, children))
            }
            "CharacterType" => self.transform(type_or_character),
            other => Err(Error::internal(format!(
                "Unexpected rule {other} encountered in SimpleType"
            ))),
        }
    }

    /// `TransformQualifiedTypeName`: `[catalog.][schema.]name`.
    pub(super) fn transform_qualified_type_name(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<QualifiedName> {
        let list = parse_result.list()?;
        let mut parts = Vec::new();
        if let Some(qualifiers) = list.optional(0)? {
            for qualifier in qualifiers.repeat()? {
                parts.push(qualifier.list()?.identifier(0)?.to_owned());
            }
        }
        if list.child(1)?.result_type() == ParseResultType::Identifier {
            parts.push(list.identifier(1)?.to_owned());
        } else {
            // DuckDB reads the third child here.
            parts.push(self.transform_child(list, 2)?);
        }
        QualifiedName::from_parts(parts)
    }

    /// `TransformCharacterType`: every character type is a VARCHAR.
    pub(super) fn transform_character_type(
        &mut self,
        _parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Ok(simple_type_expression("VARCHAR", Vec::new()))
    }

    /// `TransformMapType`: `MAP(key, value)`.
    pub(super) fn transform_map_type(&mut self, parse_result: &ParseResult) -> Result<ParsedExpression> {
        let types = parse_result.list()?.child(1)?.parens_content()?.list_elements()?;
        let [key, value] = types.as_slice() else {
            return Err(Error::parser(
                "Map type needs exactly two entries, key and value type.",
            ));
        };
        let key_type: LogicalType = self.transform(key)?;
        let value_type: LogicalType = self.transform(value)?;
        Ok(simple_type_expression(
            "MAP",
            vec![type_expression_of(&key_type)?, type_expression_of(&value_type)?],
        ))
    }

    /// `TransformRowType`: `STRUCT(name type, ...)` / `ROW(...)`.
    pub(super) fn transform_row_type(&mut self, parse_result: &ParseResult) -> Result<ParsedExpression> {
        let fields: Vec<(String, LogicalType)> = self.transform_child(parse_result.list()?, 1)?;
        Ok(simple_type_expression("STRUCT", named_type_expressions(fields)?))
    }

    /// `TransformGeometryType`: `GEOMETRY [(crs)]`.
    pub(super) fn transform_geometry_type(&mut self, parse_result: &ParseResult) -> Result<ParsedExpression> {
        let Some(crs) = parse_result.list()?.optional(1)? else {
            return Ok(simple_type_expression("GEOMETRY", Vec::new()));
        };
        let crs: ParsedExpression = self.transform(crs.parens_content()?)?;
        if !matches!(crs.kind, ParsedExpressionKind::Constant(_)) {
            return Err(Error::parser("Expected a constant as type modifier"));
        }
        Ok(simple_type_expression("GEOMETRY", vec![crs]))
    }

    /// `TransformVariantType`.
    pub(super) fn transform_variant_type(&mut self, _parse_result: &ParseResult) -> Result<ParsedExpression> {
        Ok(simple_type_expression("VARIANT", Vec::new()))
    }

    /// `TransformUnionType`: `UNION(name type, ...)`.
    pub(super) fn transform_union_type(&mut self, parse_result: &ParseResult) -> Result<ParsedExpression> {
        let members: Vec<(String, LogicalType)> = self.transform_child(parse_result.list()?, 1)?;
        Ok(simple_type_expression("UNION", named_type_expressions(members)?))
    }

    /// `TransformColIdTypeList`: `(name type, ...)`.
    pub(super) fn transform_col_id_type_list(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<(String, LogicalType)>> {
        parse_result
            .list()?
            .child(0)?
            .parens_content()?
            .list_elements()?
            .into_iter()
            .map(|field| self.transform(field))
            .collect()
    }

    /// `TransformColIdType`: `name type`.
    pub(super) fn transform_col_id_type(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<(String, LogicalType)> {
        let list = parse_result.list()?;
        let name: String = self.transform_child(list, 0)?;
        let ty: LogicalType = self.transform_child(list, 1)?;
        Ok((name, ty))
    }

    /// `TransformBitType`: `BIT [VARYING] [(n)]`.
    pub(super) fn transform_bit_type(&mut self, _parse_result: &ParseResult) -> Result<ParsedExpression> {
        Ok(simple_type_expression("BIT", Vec::new()))
    }

    /// `TransformIntervalType`.
    pub(super) fn transform_interval_type(
        &mut self,
        _parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Ok(simple_type_expression("INTERVAL", Vec::new()))
    }

    /// `TransformIntervalInterval`: `INTERVAL [unit]` is the type the unit is counted
    /// in, or an INTERVAL.
    pub(super) fn transform_interval_interval(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        let Some(interval) = parse_result.list()?.optional(1)? else {
            return Ok(simple_type_expression("INTERVAL", Vec::new()));
        };
        // DuckDB transforms the unit (a `DatePartSpecifier`) as a `LogicalType`: the
        // transform runs, then its result has the wrong type.
        let _: DatePartSpecifier = self.transform(interval)?;
        Err(Error::internal(format!(
            "Transformer for rule '{}' returned an unexpected type.",
            interval.name
        )))
    }

    /// `TransformInterval`: the unit of an interval.
    pub(super) fn transform_interval(&mut self, parse_result: &ParseResult) -> Result<DatePartSpecifier> {
        let choice = parse_result.list()?.child(0)?;
        if choice.name.eq_ignore_ascii_case("IntervalToInterval") {
            self.transform(choice.choice()?)
        } else {
            Self::transform_enum(choice)
        }
    }

    /// `TransformIntervalToInterval`: `YEAR TO MONTH` and the like are not supported.
    pub(super) fn transform_interval_to_interval(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<DatePartSpecifier> {
        let units = parse_result.list()?.choice(0)?.list()?;
        let first: DatePartSpecifier = Self::transform_enum(units.child(0)?)?;
        let second: DatePartSpecifier = Self::transform_enum(units.child(2)?)?;
        Err(Error::parser(format!(
            "{} TO {} is not supported",
            first.enum_name(),
            second.enum_name()
        )))
    }

    /// `TransformNumberLiteral`.
    pub(super) fn transform_number_literal(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        convert_number_to_value(parse_result.number()?)
    }

    /// `TransformSetofType`: `SETOF type` is the type.
    pub(super) fn transform_setof_type(&mut self, parse_result: &ParseResult) -> Result<ParsedExpression> {
        let ty: LogicalType = self.transform_child(parse_result.list()?, 1)?;
        type_expression_of(&ty)
    }

    /// `TransformStringLiteral`: the string without quotes.
    pub(super) fn transform_string_literal(&mut self, parse_result: &ParseResult) -> Result<String> {
        Ok(parse_result.string_literal()?.0.to_owned())
    }
}

/// The type expressions of `STRUCT` / `UNION` members, aliased by member name.
fn named_type_expressions(members: Vec<(String, LogicalType)>) -> Result<Vec<ParsedExpression>> {
    members
        .into_iter()
        .map(|(name, ty)| Ok(type_expression_of(&ty)?.with_alias(name)))
        .collect()
}
