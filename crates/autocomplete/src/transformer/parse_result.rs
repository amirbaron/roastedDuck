//! The parse tree the matcher builds for the transformer (DuckDB's `ParseResult`
//! hierarchy, `transformer/parse_result.hpp`).

use rustdb_ast::ParsedExpression;
use rustdb_common::{Error, Result};
use rustdb_types::{LogicalType, Value};

use crate::matcher::SpecialStringCharacter;

/// The kind of a parse result (DuckDB's `ParseResultType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ParseResultType {
    List,
    Optional,
    Repeat,
    Choice,
    Identifier,
    Keyword,
    Operator,
    Number,
    String,
}

impl ParseResultType {
    /// `ParseResultToString`.
    pub const fn name(self) -> &'static str {
        match self {
            Self::List => "LIST",
            Self::Optional => "OPTIONAL",
            Self::Repeat => "REPEAT",
            Self::Choice => "CHOICE",
            Self::Identifier => "IDENTIFIER",
            Self::Keyword => "KEYWORD",
            Self::Operator => "OPERATOR",
            Self::Number => "NUMBER",
            Self::String => "STRING",
        }
    }
}

/// The contents of a parse result (the `ParseResult` subclasses).
#[derive(Clone, Debug)]
pub(crate) enum ParseResultKind {
    /// `ListParseResult`: the results of a sequence.
    List(Vec<ParseResult>),
    /// `OptionalParseResult`: the result of an optional element, if it matched.
    Optional(Option<Box<ParseResult>>),
    /// `RepeatParseResult`: every repetition.
    Repeat(Vec<ParseResult>),
    /// `ChoiceParseResult`: the alternative that matched and its position.
    Choice {
        result: Box<ParseResult>,
        selected: usize,
    },
    /// `IdentifierParseResult`: the identifier, unquoted.
    Identifier(String),
    /// `KeywordParseResult`: the keyword as written.
    Keyword(String),
    /// `OperatorParseResult`.
    Operator(String),
    /// `NumberParseResult`: the number as written.
    Number(String),
    /// `StringLiteralParseResult`: the contents between the quotes and the prefix kind.
    String {
        text: String,
        string_type: SpecialStringCharacter,
    },
}

/// One node of the parse tree (DuckDB's `ParseResult`): the rule it matched (empty for
/// anonymous sub-rules), where it starts in the query and what it holds.
#[derive(Clone, Debug)]
pub(crate) struct ParseResult {
    pub name: &'static str,
    pub offset: Option<usize>,
    pub kind: ParseResultKind,
}

impl ParseResult {
    pub const fn new(name: &'static str, offset: Option<usize>, kind: ParseResultKind) -> Self {
        Self { name, offset, kind }
    }

    pub const fn result_type(&self) -> ParseResultType {
        match &self.kind {
            ParseResultKind::List(_) => ParseResultType::List,
            ParseResultKind::Optional(_) => ParseResultType::Optional,
            ParseResultKind::Repeat(_) => ParseResultType::Repeat,
            ParseResultKind::Choice { .. } => ParseResultType::Choice,
            ParseResultKind::Identifier(_) => ParseResultType::Identifier,
            ParseResultKind::Keyword(_) => ParseResultType::Keyword,
            ParseResultKind::Operator(_) => ParseResultType::Operator,
            ParseResultKind::Number(_) => ParseResultType::Number,
            ParseResultKind::String { .. } => ParseResultType::String,
        }
    }

    /// The error of a `Cast` to the wrong kind.
    fn cast_error(&self, expected: ParseResultType) -> Error {
        Error::internal(format!(
            "Failed to cast parse result of type {} to type {} for rule {}",
            expected.name(),
            self.result_type().name(),
            self.name
        ))
    }

    /// `Cast<ListParseResult>`.
    pub fn list(&self) -> Result<List<'_>> {
        match &self.kind {
            ParseResultKind::List(children) => Ok(List { children }),
            _ => Err(self.cast_error(ParseResultType::List)),
        }
    }

    /// `Cast<OptionalParseResult>().optional_result`.
    pub fn optional(&self) -> Result<Option<&Self>> {
        match &self.kind {
            ParseResultKind::Optional(result) => Ok(result.as_deref()),
            _ => Err(self.cast_error(ParseResultType::Optional)),
        }
    }

    /// `Cast<RepeatParseResult>().children`.
    pub fn repeat(&self) -> Result<&[Self]> {
        match &self.kind {
            ParseResultKind::Repeat(children) => Ok(children),
            _ => Err(self.cast_error(ParseResultType::Repeat)),
        }
    }

    /// `Cast<ChoiceParseResult>()`: the alternative that matched.
    pub fn choice(&self) -> Result<&Self> {
        self.choice_with_index().map(|(result, _)| result)
    }

    /// `Cast<ChoiceParseResult>()`: the alternative that matched and its position.
    pub fn choice_with_index(&self) -> Result<(&Self, usize)> {
        match &self.kind {
            ParseResultKind::Choice { result, selected } => Ok((result, *selected)),
            _ => Err(self.cast_error(ParseResultType::Choice)),
        }
    }

    /// `Cast<IdentifierParseResult>().identifier`.
    pub fn identifier(&self) -> Result<&str> {
        match &self.kind {
            ParseResultKind::Identifier(identifier) => Ok(identifier),
            _ => Err(self.cast_error(ParseResultType::Identifier)),
        }
    }

    /// `Cast<KeywordParseResult>().keyword`.
    pub fn keyword(&self) -> Result<&str> {
        match &self.kind {
            ParseResultKind::Keyword(keyword) => Ok(keyword),
            _ => Err(self.cast_error(ParseResultType::Keyword)),
        }
    }

    /// `Cast<OperatorParseResult>().operator_token`.
    pub fn operator(&self) -> Result<&str> {
        match &self.kind {
            ParseResultKind::Operator(token) => Ok(token),
            _ => Err(self.cast_error(ParseResultType::Operator)),
        }
    }

    /// `Cast<NumberParseResult>().number`.
    pub fn number(&self) -> Result<&str> {
        match &self.kind {
            ParseResultKind::Number(number) => Ok(number),
            _ => Err(self.cast_error(ParseResultType::Number)),
        }
    }

    /// `Cast<StringLiteralParseResult>()`: the string and its prefix kind.
    pub fn string_literal(&self) -> Result<(&str, SpecialStringCharacter)> {
        match &self.kind {
            ParseResultKind::String { text, string_type } => Ok((text, *string_type)),
            _ => Err(self.cast_error(ParseResultType::String)),
        }
    }

    /// `ExtractResultFromParens`: `D` of `Parens(D) <- '(' D ')'`.
    pub fn parens_content(&self) -> Result<&Self> {
        self.list()?.child(1)
    }

    /// `ExtractParseResultsFromList`: the elements of `List(D) <- D (',' D)* ','?`.
    pub fn list_elements(&self) -> Result<Vec<&Self>> {
        let list = self.list()?;
        let mut result = vec![list.child(0)?];
        if let Some(repeat) = list.optional(1)? {
            for child in repeat.repeat()? {
                result.push(child.list()?.child(1)?);
            }
        }
        Ok(result)
    }
}

/// A list parse result (DuckDB's `ListParseResult`), with accessors for its children.
#[derive(Clone, Copy, Debug)]
pub(crate) struct List<'a> {
    pub children: &'a [ParseResult],
}

impl<'a> List<'a> {
    /// `GetChild`.
    pub fn child(&self, index: usize) -> Result<&'a ParseResult> {
        self.children
            .get(index)
            .ok_or_else(|| Error::internal("Child index out of bounds"))
    }

    /// `Child<ListParseResult>`.
    pub fn list(&self, index: usize) -> Result<List<'a>> {
        self.child(index)?.list()
    }

    /// `Child<OptionalParseResult>(index).optional_result`.
    pub fn optional(&self, index: usize) -> Result<Option<&'a ParseResult>> {
        self.child(index)?.optional()
    }

    /// `Child<ChoiceParseResult>(index).result`.
    pub fn choice(&self, index: usize) -> Result<&'a ParseResult> {
        self.child(index)?.choice()
    }

    /// `Child<RepeatParseResult>(index).children`.
    pub fn repeat(&self, index: usize) -> Result<&'a [ParseResult]> {
        self.child(index)?.repeat()
    }

    /// `Child<IdentifierParseResult>(index).identifier`.
    pub fn identifier(&self, index: usize) -> Result<&'a str> {
        self.child(index)?.identifier()
    }

    /// `Child<KeywordParseResult>(index).keyword`.
    pub fn keyword(&self, index: usize) -> Result<&'a str> {
        self.child(index)?.keyword()
    }

    /// Whether the optional child `index` matched (`HasResult`).
    pub fn has(&self, index: usize) -> Result<bool> {
        Ok(self.optional(index)?.is_some())
    }
}

/// `StringLiteralParseResult::ToExpression`: the constant a string literal stands for.
pub(crate) fn string_literal_expression(text: &str, string_type: SpecialStringCharacter) -> ParsedExpression {
    match string_type {
        SpecialStringCharacter::Standard => ParsedExpression::constant(Value::varchar(text)),
        SpecialStringCharacter::NationalString => ParsedExpression::cast(
            ParsedExpression::constant(Value::varchar(text)),
            LogicalType::VARCHAR,
        ),
        SpecialStringCharacter::HexadecimalString => {
            ParsedExpression::constant(Value::varchar(format!("x{text}")))
        }
        SpecialStringCharacter::BitString => ParsedExpression::constant(Value::varchar(format!("b{text}"))),
        SpecialStringCharacter::EscapeString => {
            let mut escaped = String::with_capacity(text.len());
            let mut chars = text.chars();
            while let Some(c) = chars.next() {
                if c == '\\'
                    && let Some(next) = chars.next()
                {
                    escaped.push(match next {
                        'n' => '\n',
                        't' => '\t',
                        'r' => '\r',
                        other => other,
                    });
                } else {
                    escaped.push(c);
                }
            }
            ParsedExpression::constant(Value::varchar(escaped))
        }
    }
}
