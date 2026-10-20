//! The transform functions of DuckDB's `transform_create_sequence.cpp`: `CREATE
//! SEQUENCE` and the sequence options (also used by `ALTER SEQUENCE`).

#![allow(
    clippy::unused_self,
    clippy::unnecessary_wraps,
    reason = "transform functions have the signature the rule registry calls"
)]

use std::cmp::Ordering;

use rustdb_function::cast::ValueCast;
use rustdb_parser::hash_order::CiHashMap;

use super::create_table::{create_info, on_create_conflict};
use super::prelude::*;

/// `Value::GetValue<int64_t>`.
fn bigint_value(value: &Value) -> Result<i64> {
    if value.is_null() {
        return Err(Error::internal("Calling GetValue on a value that is NULL"));
    }
    value
        .default_cast_as(&LogicalType::BIGINT)?
        .as_i64()
        .ok_or_else(|| Error::internal("BIGINT value without payload"))
}

/// `Value::GetValue<hugeint_t>`.
fn hugeint_value(value: &Value) -> Result<i128> {
    if value.is_null() {
        return Err(Error::internal(
            "Calling GetValueInternal on a value that is NULL",
        ));
    }
    value
        .default_cast_as(&LogicalType::HUGEINT)?
        .as_i128()
        .ok_or_else(|| Error::internal("HUGEINT value without payload"))
}

/// `Value::GetValue<bool>`.
fn bool_value(value: &Value) -> Result<bool> {
    if value.is_null() {
        return Err(Error::internal(
            "Calling GetValueInternal on a value that is NULL",
        ));
    }
    value
        .default_cast_as(&LogicalType::BOOLEAN)?
        .as_bool()
        .ok_or_else(|| Error::internal("BOOLEAN value without payload"))
}

/// The value of an option that has one (`unique_ptr_cast` to `ValueSequenceOption`).
fn option_value(option: &SequenceOption) -> Result<&Value> {
    match option {
        SequenceOption::Value(_, value) => Ok(value),
        _ => Err(Error::internal("Expected a sequence option with a value")),
    }
}

/// `-constant` written as the `-` function: the negated constant, as a BIGINT
/// (`Value::Numeric(BIGINT, -value)`: negation wraps, and a value out of the BIGINT
/// range becomes 0). Other expressions are returned as they are.
fn fold_negated_constant(expr: ParsedExpression) -> Result<ParsedExpression> {
    if !matches!(expr.kind, ParsedExpressionKind::Function(_)) {
        return Ok(expr);
    }
    let ParsedExpressionKind::Function(func_expr) = expr.kind else {
        return Err(Error::internal("Expected a function expression"));
    };
    let mut func_expr = *func_expr;
    if func_expr.function_name != "-" {
        return Err(Error::invalid_input(format!(
            "Expected a minus function instead of {}",
            func_expr.function_name
        )));
    }
    if func_expr.children.is_empty() {
        return Err(Error::internal("Expected a child of the minus function"));
    }
    let mut const_expr = func_expr.children.swap_remove(0);
    let ParsedExpressionKind::Constant(constant) = &mut const_expr.kind else {
        return Err(Error::invalid_input(
            "Expected constant expression as child of minus function",
        ));
    };
    let negated = hugeint_value(&constant.value)?.wrapping_neg();
    constant.value = Value::bigint(i64::try_from(negated).unwrap_or(0));
    Ok(const_expr)
}

/// The value of a constant option expression.
fn constant_option_value(expr: ParsedExpression) -> Result<Value> {
    match expr.kind {
        ParsedExpressionKind::Constant(constant) => Ok(constant.value),
        _ => Err(Error::parser("Expected constant expression.")),
    }
}

/// `CREATE SEQUENCE`'s message for an option given twice: `Name should be passed at
/// most once`.
fn duplicate_option_error(name: &str) -> Error {
    let mut seq_option_capital = name.to_ascii_lowercase();
    if let Some(first) = seq_option_capital.get_mut(..1) {
        first.make_ascii_uppercase();
    }
    Error::parser(format!("{seq_option_capital} should be passed at most once"))
}

impl PegTransformer {
    /// `TransformCreateSequenceStmt`: `SEQUENCE [IF NOT EXISTS] name options*`. The
    /// options are applied in the iteration order of DuckDB's `case_insensitive_map_t`.
    pub(super) fn transform_create_sequence_stmt(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<CreateInfo> {
        let list = parse_result.list()?;
        let if_not_exists = list.has(1)?;
        let qualified_name: QualifiedName = self.transform_child(list, 2)?;
        let mut info = CreateSequenceInfo {
            name: qualified_name.name,
            usage_count: 0,
            increment: 1,
            min_value: 1,
            max_value: i64::MAX,
            start_value: 1,
            cycle: false,
        };
        let mut sequence_options: CiHashMap<SequenceOption> = CiHashMap::new();
        if let Some(repeat_seq_options) = list.optional(3)? {
            for seq_option in repeat_seq_options.repeat()? {
                let (name, option): (String, SequenceOption) = self.transform(seq_option)?;
                if sequence_options.contains_key(&name) {
                    return Err(duplicate_option_error(&name));
                }
                sequence_options.insert(name, option);
            }
        }
        let no_min = sequence_options.contains_key("nominvalue");
        let no_max = sequence_options.contains_key("nomaxvalue");
        if no_min && sequence_options.contains_key("minvalue") {
            return Err(Error::parser("Minvalue should be passed at most once"));
        }
        if no_max && sequence_options.contains_key("maxvalue") {
            return Err(Error::parser("Maxvalue should be passed at most once"));
        }
        let mut has_start_value = false;
        let mut min_value_set = false;
        let mut max_value_set = false;
        for (name, option) in sequence_options.iter() {
            match name {
                "increment" => {
                    info.increment = bigint_value(option_value(option)?)?;
                    match info.increment.cmp(&0) {
                        Ordering::Equal => return Err(Error::parser("Increment must not be zero")),
                        Ordering::Less => {
                            if !min_value_set {
                                info.min_value = i64::MIN;
                            }
                            if !max_value_set {
                                info.max_value = -1;
                            }
                        }
                        Ordering::Greater => {
                            if !max_value_set {
                                info.max_value = i64::MAX;
                            }
                            if !min_value_set {
                                info.min_value = 1;
                            }
                        }
                    }
                }
                "minvalue" => {
                    if no_min {
                        continue;
                    }
                    info.min_value = bigint_value(option_value(option)?)?;
                    min_value_set = true;
                }
                "maxvalue" => {
                    if no_max {
                        continue;
                    }
                    info.max_value = bigint_value(option_value(option)?)?;
                    max_value_set = true;
                }
                "start" => {
                    info.start_value = bigint_value(option_value(option)?)?;
                    has_start_value = true;
                }
                "cycle" => info.cycle = bool_value(option_value(option)?)?,
                _ => {
                    return Err(Error::parser(format!(
                        "Unrecognized option \"{name}\" for CREATE SEQUENCE"
                    )));
                }
            }
        }
        if !has_start_value {
            info.start_value = if info.increment < 0 {
                info.max_value
            } else {
                info.min_value
            };
        }
        if info.max_value <= info.min_value {
            return Err(Error::parser(format!(
                "MINVALUE ({}) must be less than MAXVALUE ({})",
                info.min_value, info.max_value
            )));
        }
        if info.start_value < info.min_value {
            return Err(Error::parser(format!(
                "START value ({}) cannot be less than MINVALUE ({})",
                info.start_value, info.min_value
            )));
        }
        if info.start_value > info.max_value {
            return Err(Error::parser(format!(
                "START value ({}) cannot be greater than MAXVALUE ({})",
                info.start_value, info.max_value
            )));
        }
        let mut result = create_info(&qualified_name.schema, CreateInfoKind::Sequence(Box::new(info)));
        result.catalog = qualified_name.catalog;
        result.on_conflict = on_create_conflict(if_not_exists);
        Ok(result)
    }

    /// `TransformSequenceOption`.
    pub(super) fn transform_sequence_option(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<(String, SequenceOption)> {
        self.transform(parse_result.list()?.choice(0)?)
    }

    /// `TransformSeqSetCycle`: `[NO] CYCLE`. DuckDB stores the flag as the INTEGER
    /// `Value(true)` / `Value(false)` (there is no `Value(bool)` constructor).
    pub(super) fn transform_seq_set_cycle(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<(String, SequenceOption)> {
        let cycle = !parse_result.list()?.has(0)?;
        Ok((
            "cycle".to_owned(),
            SequenceOption::Value(SequenceInfo::Cycle, Value::integer(i32::from(cycle))),
        ))
    }

    /// `TransformSeqSetIncrement`: `INCREMENT [BY] constant`.
    pub(super) fn transform_seq_set_increment(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<(String, SequenceOption)> {
        let expr: ParsedExpression = self.transform_child(parse_result.list()?, 2)?;
        let value = constant_option_value(fold_negated_constant(expr)?)?;
        Ok((
            "increment".to_owned(),
            SequenceOption::Value(SequenceInfo::Increment, value),
        ))
    }

    /// `TransformSeqSetMinMax`: `MINVALUE | MAXVALUE constant`.
    pub(super) fn transform_seq_set_min_max(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<(String, SequenceOption)> {
        let list = parse_result.list()?;
        let expr: ParsedExpression = self.transform_child(list, 1)?;
        let rule_name: String = self.transform_child(list, 0)?;
        let value = constant_option_value(fold_negated_constant(expr)?)?;
        let seq_info = if rule_name == "minvalue" {
            SequenceInfo::Min
        } else {
            SequenceInfo::Max
        };
        Ok((rule_name, SequenceOption::Value(seq_info, value)))
    }

    /// `TransformSeqMinOrMax`: `minvalue` or `maxvalue`.
    pub(super) fn transform_seq_min_or_max(&mut self, parse_result: &ParseResult) -> Result<String> {
        Self::transform_enum(parse_result.list()?.choice(0)?)
    }

    /// `TransformSeqNoMinMax`: `NO MINVALUE | MAXVALUE`.
    pub(super) fn transform_seq_no_min_max(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<(String, SequenceOption)> {
        let rule_name: String = self.transform_child(parse_result.list()?, 1)?;
        let seq_info = if rule_name == "minvalue" {
            SequenceInfo::Min
        } else {
            SequenceInfo::Max
        };
        // `Value(true)` is the INTEGER 1 in DuckDB.
        Ok((
            format!("no{rule_name}"),
            SequenceOption::Value(seq_info, Value::integer(1)),
        ))
    }

    /// `TransformSeqStartWith`: `START [WITH] constant`.
    pub(super) fn transform_seq_start_with(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<(String, SequenceOption)> {
        let expr: ParsedExpression = self.transform_child(parse_result.list()?, 2)?;
        let value = constant_option_value(expr)?;
        Ok((
            "start".to_owned(),
            SequenceOption::Value(SequenceInfo::Start, value),
        ))
    }

    /// `TransformSeqOwnedBy`: `OWNED BY name`.
    pub(super) fn transform_seq_owned_by(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<(String, SequenceOption)> {
        let qualified_name: QualifiedName = self.transform_child(parse_result.list()?, 2)?;
        Ok((
            "owned".to_owned(),
            SequenceOption::Qualified(SequenceInfo::Owner, qualified_name),
        ))
    }
}
