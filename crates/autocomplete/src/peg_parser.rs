//! The reader of the PEG grammar (DuckDB's `PEGParser`, `parser/peg_parser.cpp`): splits
//! the grammar text into rules, each a flat list of literal, rule-reference, operator,
//! function-call and regex tokens.

use std::collections::HashMap;

use roastedduck_common::{Error, Result};

/// The kind of a grammar token (DuckDB's `PEGTokenType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PegTokenType {
    /// A literal (`'Keyword'`).
    Literal,
    /// A reference to another rule (`Rule`).
    Reference,
    /// An operator (`/ ? ( ) * + !`).
    Operator,
    /// The start of a call of a parameterized rule (`Rule(`).
    FunctionCall,
    /// A regular expression (`[ \t]` or `<[a-z_]i>`).
    Regex,
}

/// One token of a rule (DuckDB's `PEGToken`).
#[derive(Clone, Copy, Debug)]
pub(crate) struct PegToken<'g> {
    pub ty: PegTokenType,
    pub text: &'g str,
}

/// A rule: its parameter names (at most one) and its tokens (DuckDB's `PEGRule`).
#[derive(Clone, Debug, Default)]
pub(crate) struct PegRule<'g> {
    /// The parameter names, by position.
    pub parameters: Vec<&'g str>,
    pub tokens: Vec<PegToken<'g>>,
}

impl PegRule<'_> {
    /// The position of the parameter called `name`.
    pub fn parameter_index(&self, name: &str) -> Option<usize> {
        self.parameters.iter().position(|parameter| *parameter == name)
    }
}

/// Where the reader is within a rule (DuckDB's `PEGParseState`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PegParseState {
    Name,
    Separator,
    Definition,
}

/// `IsPEGOperator`.
const fn is_peg_operator(c: u8) -> bool {
    matches!(c, b'/' | b'?' | b'(' | b')' | b'*' | b'+' | b'!')
}

/// `StringUtil::CharacterIsSpace`.
pub(crate) const fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | 0x0B | 0x0C | b'\r')
}

/// `StringUtil::CharacterIsNewline`.
const fn is_newline(c: u8) -> bool {
    matches!(c, b'\n' | b'\r')
}

/// The rules of a grammar, by case-insensitive name (DuckDB's `PEGParser::rules`).
#[derive(Debug, Default)]
pub(crate) struct PegParser<'g> {
    rules: HashMap<String, PegRule<'g>>,
}

impl<'g> PegParser<'g> {
    /// The rule called `name` (case-insensitive).
    pub fn rule(&self, name: &str) -> Option<&PegRule<'g>> {
        self.rules.get(&name.to_ascii_lowercase())
    }

    /// `PEGParser::AddRule`.
    fn add_rule(&mut self, name: &'g str, rule: PegRule<'g>) -> Result<()> {
        let key = name.to_ascii_lowercase();
        if self.rules.contains_key(&key) {
            return Err(Error::internal(format!(
                "Failed to parse grammar - duplicate rule name {name}"
            )));
        }
        self.rules.insert(key, rule);
        Ok(())
    }

    /// `PEGParser::ParseRules`: reads every rule of `grammar`. A rule ends at the first
    /// newline outside brackets that does not follow a `/`.
    #[allow(
        clippy::too_many_lines,
        reason = "one state machine, like DuckDB's ParseRules"
    )]
    pub fn parse_rules(grammar: &'g str) -> Result<Self> {
        let bytes = grammar.as_bytes();
        // The grammar is NUL-terminated in DuckDB: reading past the end yields 0.
        let at = |c: usize| bytes.get(c).copied().unwrap_or(0);
        let mut parser = Self::default();
        let mut rule_name: &'g str = "";
        let mut rule = PegRule::default();
        let mut parse_state = PegParseState::Name;
        let mut bracket_count = 0usize;
        let mut in_or_clause = false;
        let mut c = 0usize;
        while at(c) != 0 {
            if at(c) == b'#' {
                // A comment: ignore it up to the end of the line.
                while at(c) != 0 && !is_newline(at(c)) {
                    c += 1;
                }
                continue;
            }
            if parse_state == PegParseState::Definition
                && is_newline(at(c))
                && bracket_count == 0
                && !in_or_clause
                && !rule.tokens.is_empty()
            {
                // A newline completes the rule being defined.
                parser.add_rule(rule_name, std::mem::take(&mut rule))?;
                rule_name = "";
                parse_state = PegParseState::Name;
                c += 1;
                continue;
            }
            if is_space(at(c)) {
                c += 1;
                continue;
            }
            match parse_state {
                PegParseState::Name => {
                    let start = c;
                    if at(c) == b'%' {
                        // Rules can start with % (%whitespace).
                        c += 1;
                    }
                    while at(c).is_ascii_alphanumeric() {
                        c += 1;
                    }
                    if c == start {
                        return Err(Error::internal(format!(
                            "Failed to parse grammar - expected an alpha-numeric rule name (pos {c})"
                        )));
                    }
                    rule_name = &grammar[start..c];
                    rule = PegRule::default();
                    parse_state = PegParseState::Separator;
                }
                PegParseState::Separator => {
                    if at(c) == b'(' {
                        if !rule.parameters.is_empty() {
                            return Err(Error::internal(format!(
                                "Failed to parse grammar - multiple parameters at position {c}"
                            )));
                        }
                        c += 1;
                        let parameter_start = c;
                        while at(c).is_ascii_alphanumeric() {
                            c += 1;
                        }
                        if parameter_start == c {
                            return Err(Error::internal(format!(
                                "Failed to parse grammar - expected a parameter at position {c}"
                            )));
                        }
                        rule.parameters.push(&grammar[parameter_start..c]);
                        if at(c) != b')' {
                            return Err(Error::internal(format!(
                                "Failed to parse grammar - expected closing bracket at position {c}"
                            )));
                        }
                        c += 1;
                    } else {
                        if at(c) != b'<' || at(c + 1) != b'-' {
                            return Err(Error::internal(format!(
                                "Failed to parse grammar - expected a rule definition (<-) (pos {c})"
                            )));
                        }
                        c += 2;
                        parse_state = PegParseState::Definition;
                    }
                }
                PegParseState::Definition => {
                    // A literal ('Keyword'), a rule reference (Rule), a regex or an operator.
                    in_or_clause = false;
                    if at(c) == b'\'' {
                        c += 1;
                        let literal_start = c;
                        while at(c) != 0 && at(c) != b'\'' {
                            if at(c) == b'\\' {
                                // An escape.
                                c += 1;
                            }
                            c += 1;
                        }
                        if at(c) == 0 {
                            return Err(Error::internal(format!(
                                "Failed to parse grammar - did not find closing ' (pos {c})"
                            )));
                        }
                        rule.tokens.push(PegToken {
                            ty: PegTokenType::Literal,
                            text: &grammar[literal_start..c],
                        });
                        c += 1;
                        if at(c) == b'i' {
                            return Err(Error::internal(format!(
                                "Failed to parse grammar - unexpected \"i\" found in grammar near rule {rule_name}"
                            )));
                        }
                    } else if at(c).is_ascii_alphanumeric() {
                        let reference_start = c;
                        while at(c).is_ascii_alphanumeric() {
                            c += 1;
                        }
                        let text = &grammar[reference_start..c];
                        let ty = if at(c) == b'(' {
                            // A call of a parameterized rule.
                            c += 1;
                            bracket_count += 1;
                            PegTokenType::FunctionCall
                        } else {
                            PegTokenType::Reference
                        };
                        rule.tokens.push(PegToken { ty, text });
                    } else if at(c) == b'[' || at(c) == b'<' {
                        // A regular expression: [^"] or <...>.
                        let regex_start = c;
                        let final_char = if at(c) == b'[' { b']' } else { b'>' };
                        while at(c) != 0 && at(c) != final_char {
                            if at(c) == b'\\' {
                                c += 1;
                            }
                            if at(c) != 0 {
                                c += 1;
                            }
                        }
                        c += 1;
                        rule.tokens.push(PegToken {
                            ty: PegTokenType::Regex,
                            text: &grammar[regex_start..c.min(grammar.len())],
                        });
                    } else if is_peg_operator(at(c)) {
                        match at(c) {
                            b'(' => bracket_count += 1,
                            b')' => {
                                if bracket_count == 0 {
                                    return Err(Error::internal(format!(
                                        "Failed to parse grammar - unclosed bracket at position {c} in rule {rule_name}"
                                    )));
                                }
                                bracket_count -= 1;
                            }
                            b'/' => in_or_clause = true,
                            _ => {}
                        }
                        // Operators are always one byte.
                        rule.tokens.push(PegToken {
                            ty: PegTokenType::Operator,
                            text: &grammar[c..=c],
                        });
                        c += 1;
                    } else {
                        return Err(Error::internal(format!(
                            "Unrecognized rule contents in rule {rule_name} (character {})",
                            char::from(at(c))
                        )));
                    }
                }
            }
            if at(c) == 0 {
                break;
            }
        }
        match parse_state {
            PegParseState::Separator => Err(Error::internal(format!(
                "Failed to parse grammar - rule {rule_name} does not have a definition"
            ))),
            PegParseState::Definition if rule.tokens.is_empty() => Err(Error::internal(format!(
                "Failed to parse grammar - rule {rule_name} is empty"
            ))),
            PegParseState::Definition => {
                parser.add_rule(rule_name, rule)?;
                Ok(parser)
            }
            PegParseState::Name => Ok(parser),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules_end_at_newlines_outside_brackets_and_choices() {
        let parser =
            PegParser::parse_rules("A <- 'x' /\n  'y'\nB(P) <- (P\n 'z')? C # comment\nC <- A(B)\n").unwrap();
        let a = parser.rule("a").unwrap();
        let texts: Vec<&str> = a.tokens.iter().map(|token| token.text).collect();
        assert_eq!(texts, ["x", "/", "y"]);
        let b = parser.rule("B").unwrap();
        assert_eq!(b.parameters, ["P"]);
        assert_eq!(b.tokens.len(), 6);
        let c = parser.rule("C").unwrap();
        assert_eq!(c.tokens[0].ty, PegTokenType::FunctionCall);
        assert_eq!(c.tokens[1].ty, PegTokenType::Reference);
        assert_eq!(c.tokens[2].text, ")");
    }

    #[test]
    fn duplicate_rules_are_rejected() {
        assert!(PegParser::parse_rules("A <- 'x'\na <- 'y'\n").is_err());
    }
}
