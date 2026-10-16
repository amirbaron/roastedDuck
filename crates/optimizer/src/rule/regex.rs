//! `regexp_matches` with a pattern that is a plain string or a simple LIKE pattern
//! (DuckDB's `RegexOptimizationRule`): `regexp_matches(s, 'abc')` becomes
//! `contains(s, 'abc')` and `regexp_matches(s, '^abc$')` becomes `s LIKE 'abc'` (which
//! the LIKE rule simplifies further).
//!
//! DuckDB inspects RE2's simplified parse tree; the same shapes are recognized here in
//! `regex-syntax`'s HIR, which normalizes patterns the same way for these cases (merged
//! literal runs, single-character classes as literals, text anchors).

use regex_syntax::ParserBuilder;
use regex_syntax::hir::{Class, Hir, HirKind, Look};
use rustdb_common::Result;
use rustdb_planner::{Expression, ExpressionKind};
use rustdb_types::Value;

use super::RuleOutcome;
use crate::expression_rewriter::RuleContext;
use crate::expression_util::try_evaluate_scalar;
use crate::optimizer::bind_scalar_function;

/// A LIKE string derived from a regex (DuckDB's `LikeString`).
#[derive(Default)]
struct LikeString {
    exists: bool,
    like_string: String,
}

/// Appends a literal's text; control characters, and in LIKE patterns `%` and `_`, make
/// the conversion impossible (DuckDB's `AddCharacter`).
fn add_literal(bytes: &[u8], result: &mut LikeString, contains: bool) {
    let Ok(text) = std::str::from_utf8(bytes) else {
        result.exists = false;
        return;
    };
    for c in text.chars() {
        if c.is_ascii_control() || (!contains && (c == '%' || c == '_')) {
            result.exists = false;
            return;
        }
        result.like_string.push(c);
    }
}

/// Whether a class matches every character, i.e. `.` with the `s` option (RE2's
/// `kRegexpAnyChar`).
fn is_any_char(hir: &Hir) -> bool {
    match hir.kind() {
        HirKind::Class(Class::Unicode(class)) => {
            let ranges = class.ranges();
            ranges.len() == 1 && ranges[0].start() == '\0' && ranges[0].end() == char::MAX
        }
        _ => false,
    }
}

/// The LIKE pattern of a concatenation (DuckDB's `LikeMatchFromRegex`).
fn like_match_from_concat(subs: &[Hir]) -> LikeString {
    let mut result = LikeString {
        exists: true,
        like_string: String::new(),
    };
    let count = subs.len();
    for (index, sub) in subs.iter().enumerate() {
        match sub.kind() {
            _ if is_any_char(sub) => {
                if index == 0 {
                    result.like_string.push('%');
                }
                result.like_string.push('_');
                if index + 1 == count {
                    result.like_string.push('%');
                }
            }
            HirKind::Repetition(repetition)
                if repetition.min == 0 && repetition.max.is_none() && is_any_char(&repetition.sub) =>
            {
                result.like_string.push('%');
            }
            HirKind::Literal(literal) => {
                if index == 0 {
                    result.like_string.push('%');
                }
                add_literal(&literal.0, &mut result, false);
                if !result.exists {
                    return result;
                }
                if index + 1 == count {
                    result.like_string.push('%');
                }
            }
            HirKind::Look(Look::Start | Look::End) | HirKind::Empty => {}
            _ => {
                result.exists = false;
                return result;
            }
        }
    }
    result
}

/// The options of a `regexp_matches` call: case sensitivity, `.` matching newlines and
/// literal patterns (DuckDB's `ParseRegexOptions`).
fn parse_options(options: &str) -> Option<(bool, bool, bool)> {
    let (mut case_sensitive, mut dot_nl, mut literal) = (true, false, false);
    for c in options.chars() {
        match c {
            'c' => case_sensitive = true,
            'i' => case_sensitive = false,
            'l' => literal = true,
            'm' | 'n' | 'p' => dot_nl = false,
            's' => dot_nl = true,
            ' ' | '\t' | '\n' => {}
            _ => return None,
        }
    }
    Some((case_sensitive, dot_nl, literal))
}

pub fn regex_optimization(
    context: &RuleContext<'_>,
    expression: &mut Expression,
    _: bool,
) -> Result<RuleOutcome> {
    let return_type = expression.return_type.clone();
    let ExpressionKind::Function(function) = &mut expression.kind else {
        return Ok(RuleOutcome::Unchanged);
    };
    if &*function.function.name != "regexp_matches" || !(2..=3).contains(&function.children.len()) {
        return Ok(RuleOutcome::Unchanged);
    }
    let Some(pattern) = function.children[1].as_constant().cloned() else {
        return Ok(RuleOutcome::Unchanged);
    };
    if pattern.is_null() {
        return Ok(RuleOutcome::Replaced(Expression::constant(Value::null(
            return_type,
        ))));
    }
    let options = match function.children.get(2) {
        None => String::new(),
        Some(options) => match try_evaluate_scalar(context.client, options) {
            Some(value) if !value.is_null() => value.to_varchar().unwrap_or_default(),
            _ => return Ok(RuleOutcome::Unchanged),
        },
    };
    let Some((case_sensitive, dot_nl, literal)) = parse_options(&options) else {
        return Ok(RuleOutcome::Unchanged);
    };
    let Ok(pattern) = pattern.to_varchar() else {
        return Ok(RuleOutcome::Unchanged);
    };
    // Flags inside the pattern may switch case folding or line mode on and off.
    if !case_sensitive || pattern.contains("(?") {
        return Ok(RuleOutcome::Unchanged);
    }
    let hir = if literal {
        Hir::literal(pattern.as_bytes().to_vec())
    } else {
        match ParserBuilder::new()
            .dot_matches_new_line(dot_nl)
            .build()
            .parse(&pattern)
        {
            Ok(hir) => hir,
            Err(_) => return Ok(RuleOutcome::Unchanged),
        }
    };
    let (function_name, needle) = match hir.kind() {
        HirKind::Literal(literal) => {
            let mut contains = LikeString {
                exists: true,
                like_string: String::new(),
            };
            add_literal(&literal.0, &mut contains, true);
            if !contains.exists {
                return Ok(RuleOutcome::Unchanged);
            }
            ("contains", contains.like_string)
        }
        HirKind::Concat(subs) => {
            let like = like_match_from_concat(subs);
            if !like.exists {
                return Ok(RuleOutcome::Unchanged);
            }
            ("~~", like.like_string)
        }
        _ => return Ok(RuleOutcome::Unchanged),
    };
    function.children.truncate(2);
    function.children[1] = Expression::constant(Value::varchar(needle));
    let children = std::mem::take(&mut function.children);
    let is_operator = function_name == "~~";
    match bind_scalar_function(context.client, function_name, children, is_operator) {
        Ok(replacement) => Ok(RuleOutcome::Replaced(replacement)),
        Err(error) => Err(error),
    }
}
