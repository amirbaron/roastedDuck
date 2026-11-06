//! Line-level parsing of sqllogictest files (port of `test/sqlite/sqllogic_parser.cpp`).
//!
//! Like DuckDB's runner, parsing is interleaved with execution: the runner asks for the next
//! record header, decides (via `skipif`/`onlyif`/`mode skip`) whether to look at it at all, and
//! only then extracts the record body. Malformed records that are skipped therefore never fail.

use std::path::Path;

use crate::outcome::Failure;
use crate::text;

/// The record keyword at the start of a line (`SQLLogicTokenType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenKind {
    Invalid,
    SkipIf,
    OnlyIf,
    Statement,
    Query,
    HashThreshold,
    Halt,
    Mode,
    Set,
    Reset,
    Loop,
    Foreach,
    ConcurrentLoop,
    ConcurrentForeach,
    EndLoop,
    Require,
    RequireEnv,
    TestEnv,
    Load,
    Restart,
    Reconnect,
    Sleep,
    Unzip,
    Tags,
    Continue,
}

impl TokenKind {
    fn from_keyword(keyword: &str) -> Option<Self> {
        Some(match keyword {
            "skipif" => Self::SkipIf,
            "onlyif" => Self::OnlyIf,
            "statement" => Self::Statement,
            "query" => Self::Query,
            "hash-threshold" => Self::HashThreshold,
            "halt" => Self::Halt,
            "mode" => Self::Mode,
            "set" => Self::Set,
            "reset" => Self::Reset,
            "loop" => Self::Loop,
            "concurrentloop" => Self::ConcurrentLoop,
            "foreach" => Self::Foreach,
            "concurrentforeach" => Self::ConcurrentForeach,
            "endloop" => Self::EndLoop,
            "require" => Self::Require,
            "require-env" => Self::RequireEnv,
            "test-env" => Self::TestEnv,
            "load" => Self::Load,
            "restart" => Self::Restart,
            "reconnect" => Self::Reconnect,
            "sleep" => Self::Sleep,
            "unzip" => Self::Unzip,
            "tags" => Self::Tags,
            "continue" => Self::Continue,
            _ => return None,
        })
    }

    /// Records that must be followed by an empty line or a comment (`IsSingleLineStatement`).
    pub fn is_single_line(self) -> bool {
        !matches!(
            self,
            Self::SkipIf | Self::OnlyIf | Self::Invalid | Self::Statement | Self::Query
        )
    }

    /// Records that execute SQL (`IsTestCommand`); tags must precede the first of them.
    pub fn is_test_command(self) -> bool {
        matches!(self, Self::Statement | Self::Query)
    }
}

/// A tokenized record header: keyword plus whitespace-separated parameters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub params: Vec<String>,
}

/// What a `statement` record expects (`ExpectedResult`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExpectedResult {
    Success,
    Error,
    /// `statement maybe`: either outcome is fine, but an error must match the message.
    Unknown,
    /// `statement debug`: either outcome is fine; the result is printed.
    DontCare,
}

/// How query results are ordered before comparison (`SortStyle`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SortStyle {
    #[default]
    NoSort,
    RowSort,
    ValueSort,
}

impl SortStyle {
    /// `TestConfiguration::TryParseSortStyle`.
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "nosort" | "none" => Some(Self::NoSort),
            "rowsort" | "sort" => Some(Self::RowSort),
            "valuesort" => Some(Self::ValueSort),
            _ => None,
        }
    }
}

/// Comparison operator of a loop-variable condition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Comparison {
    Equal,
    NotEqual,
    GreaterThan,
    GreaterThanOrEqual,
    LessThan,
    LessThanOrEqual,
}

/// A `skipif`/`onlyif` condition on a loop variable, e.g. `skipif i>=2`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Condition {
    pub keyword: String,
    pub value: String,
    pub comparison: Comparison,
    pub skip_if: bool,
}

/// Cursor over the lines of a test file (`SQLLogicParser`).
#[derive(Debug)]
pub struct Parser {
    file_name: String,
    lines: Vec<String>,
    current_line: usize,
    seen_statement: bool,
}

impl Parser {
    /// Reads `path` (`OpenFile`); `\r` characters are removed from every line.
    pub fn open(path: &Path, file_name: &str) -> std::io::Result<Self> {
        let content = std::fs::read(path)?;
        Ok(Self::from_text(&String::from_utf8_lossy(&content), file_name))
    }

    /// A parser over in-memory text, split like `std::getline`.
    pub fn from_text(text: &str, file_name: &str) -> Self {
        let mut lines: Vec<String> = text.split('\n').map(|line| line.replace('\r', "")).collect();
        if text.ends_with('\n') {
            lines.pop();
        }
        Self {
            file_name: file_name.to_owned(),
            lines,
            current_line: 0,
            seen_statement: false,
        }
    }

    /// The 0-based index of the current line.
    pub fn current_line(&self) -> usize {
        self.current_line
    }

    fn empty_or_comment(line: &str) -> bool {
        line.is_empty() || line.starts_with('#')
    }

    fn line_is_record_body(&self) -> bool {
        self.lines
            .get(self.current_line)
            .is_some_and(|line| !Self::empty_or_comment(line))
    }

    /// Whether the line after the current one is empty, a comment, or past the end.
    pub fn next_line_empty_or_comment(&self) -> bool {
        self.lines
            .get(self.current_line + 1)
            .is_none_or(|line| Self::empty_or_comment(line))
    }

    /// Moves to the first line of the next record; returns `false` at the end of the file.
    pub fn next_statement(&mut self) -> bool {
        if self.seen_statement {
            while self.line_is_record_body() {
                self.current_line += 1;
            }
        }
        self.seen_statement = true;
        while self.current_line < self.lines.len() && Self::empty_or_comment(&self.lines[self.current_line]) {
            self.current_line += 1;
        }
        self.current_line < self.lines.len()
    }

    pub fn next_line(&mut self) {
        self.current_line += 1;
    }

    /// Reads the SQL text up to the next empty line, comment or `----` separator.
    pub fn extract_statement(&mut self) -> String {
        let mut statement = String::new();
        let mut first_line = true;
        while self.line_is_record_body() {
            let line = &self.lines[self.current_line];
            if line == "----" {
                break;
            }
            if !first_line {
                statement.push('\n');
            }
            statement.push_str(line);
            first_line = false;
            self.current_line += 1;
        }
        statement
    }

    /// Reads the expected result lines after an optional `----` up to the next empty line.
    pub fn extract_expected_result(&mut self) -> Vec<String> {
        if self
            .lines
            .get(self.current_line)
            .is_some_and(|line| line == "----")
        {
            self.current_line += 1;
        }
        let mut result = Vec::new();
        while let Some(line) = self.lines.get(self.current_line).filter(|line| !line.is_empty()) {
            result.push(line.clone());
            self.current_line += 1;
        }
        result
    }

    /// Reads the expected error message of a `statement error`/`statement maybe` record.
    pub fn extract_expected_error(
        &mut self,
        expected: ExpectedResult,
        original_sqlite_test: bool,
    ) -> Result<String, Failure> {
        let expect_error_message = matches!(expected, ExpectedResult::Error | ExpectedResult::Unknown);
        if self
            .lines
            .get(self.current_line)
            .is_none_or(|line| line != "----")
        {
            if expect_error_message && !original_sqlite_test {
                return Err(self.fail(
                    "Failed to parse statement: statement error and maybe needs to have an expected error message",
                ));
            }
            return Ok(String::new());
        }
        if !expect_error_message {
            return Err(self.fail(
                "Failed to parse statement: only statement error or maybe can have an expected error message, \
                 not statement ok",
            ));
        }
        self.current_line += 1;
        let mut error_lines = Vec::new();
        while let Some(line) = self.lines.get(self.current_line).filter(|line| !line.is_empty()) {
            error_lines.push(line.as_str());
            self.current_line += 1;
        }
        Ok(error_lines.join("\n"))
    }

    /// Splits the current line into a keyword and parameters (`Tokenize`).
    pub fn tokenize(&self) -> Result<Token, Failure> {
        let Some(line) = self.lines.get(self.current_line) else {
            return Ok(Token {
                kind: TokenKind::Invalid,
                params: Vec::new(),
            });
        };
        let mut words = line
            .split(|c: char| c.is_ascii() && text::is_space(c as u8))
            .filter(|word| !word.is_empty())
            .map(str::to_owned);
        let Some(keyword) = words.next() else {
            return Err(self.fail("Empty line!?"));
        };
        let kind = TokenKind::from_keyword(&keyword)
            .ok_or_else(|| self.fail(&format!("Unrecognized parameter {keyword}")))?;
        Ok(Token {
            kind,
            params: words.collect(),
        })
    }

    /// A failure at the current line, formatted like `SQLLogicParser::Fail`.
    pub fn fail(&self, message: &str) -> Failure {
        let line = self.current_line + 1;
        Failure::at(line, format!("{}:{line}: {message}", self.file_name))
    }
}

/// Comparison operators in the order `TryParseConditions` tries them.
const COMPARATORS: [(&str, Comparison); 6] = [
    ("<>", Comparison::NotEqual),
    (">=", Comparison::GreaterThanOrEqual),
    (">", Comparison::GreaterThan),
    ("<=", Comparison::LessThanOrEqual),
    ("<", Comparison::LessThan),
    ("=", Comparison::Equal),
];

/// `TryParseConditions`: parses `x=y`, `x<>y`, `x>=y`, ... joined by `&&`. Returns `Ok(None)`
/// when the text is not a condition at all (a system name such as `duckdb`).
pub fn parse_conditions(
    parser: &Parser,
    text: &str,
    skip_if: bool,
) -> Result<Option<Vec<Condition>>, Failure> {
    if !text.contains(['=', '>', '<']) {
        return Ok(None);
    }
    let mut conditions = Vec::new();
    for condition in text::split(text, "&&") {
        let parsed = COMPARATORS
            .iter()
            .find(|(op, _)| condition.contains(op))
            .map(|(op, comparison)| (text::split(&condition, op), *comparison));
        let Some((sides, comparison)) = parsed.filter(|(sides, _)| sides.len() == 2) else {
            return Err(
                parser.fail("skipif/onlyif must be in the form of x=y or x>y, potentially separated by &&")
            );
        };
        conditions.push(Condition {
            keyword: text::trim(&sides[0]),
            value: text::trim(&sides[1]),
            comparison,
            skip_if,
        });
    }
    Ok(Some(conditions))
}

const SIGNED_TYPES: [&str; 5] = ["tinyint", "smallint", "integer", "bigint", "hugeint"];
const UNSIGNED_TYPES: [&str; 5] = ["utinyint", "usmallint", "uinteger", "ubigint", "uhugeint"];
const COMPRESSIONS: [&str; 9] = [
    "none",
    "uncompressed",
    "rle",
    "bitpacking",
    "dictionary",
    "fsst",
    "dict_fsst",
    "alp",
    "alprd",
];
const ALL_TYPES_COLUMNS: [&str; 52] = [
    "bool",
    "tinyint",
    "smallint",
    "int",
    "bigint",
    "hugeint",
    "uhugeint",
    "utinyint",
    "usmallint",
    "uint",
    "ubigint",
    "date",
    "time",
    "timestamp",
    "timestamp_s",
    "timestamp_ms",
    "timestamp_ns",
    "time_tz",
    "timestamp_tz",
    "float",
    "double",
    "dec_4_1",
    "dec_9_4",
    "dec_18_6",
    "dec38_10",
    "uuid",
    "interval",
    "varchar",
    "blob",
    "bit",
    "small_enum",
    "medium_enum",
    "large_enum",
    "int_array",
    "double_array",
    "date_array",
    "timestamp_array",
    "timestamptz_array",
    "varchar_array",
    "nested_int_array",
    "struct",
    "struct_of_arrays",
    "array_of_structs",
    "map",
    "union",
    "fixed_int_array",
    "fixed_varchar_array",
    "fixed_nested_int_array",
    "fixed_nested_varchar_array",
    "fixed_struct_array",
    "struct_of_fixed_array",
    "fixed_array_of_int_list",
];

/// `ForEachTokenReplace`: expands collection tokens (`<numeric>`, `<compression>`, ...) and
/// applies `!token` removals into `tokens`. Returns `false` if `parameter` is a plain token that
/// the caller must append itself.
pub fn expand_foreach_token(parameter: &str, tokens: &mut Vec<String>) -> bool {
    if parameter.is_empty() {
        return true;
    }
    let name = text::trim(&parameter.to_ascii_lowercase());
    let is_all = name == "<alltypes>";
    let is_numeric = is_all || name == "<numeric>";
    let is_integral = is_numeric || name == "<integral>";
    let is_signed = is_integral || name == "<signed>";
    let is_unsigned = is_integral || name == "<unsigned>";
    let mut collection = false;
    if name.starts_with('!') {
        let removed = parameter.get(1..).unwrap_or_default();
        let Some(position) = tokens.iter().position(|token| token == removed) else {
            return false;
        };
        tokens.remove(position);
        collection = true;
    }
    let mut extend = |items: &[&str]| {
        tokens.extend(items.iter().map(|item| (*item).to_owned()));
        collection = true;
    };
    if is_signed {
        extend(&SIGNED_TYPES);
    }
    if is_unsigned {
        extend(&UNSIGNED_TYPES);
    }
    if is_numeric {
        extend(&["float", "double"]);
    }
    if is_all {
        extend(&["bool", "interval", "varchar"]);
    }
    if name == "<compression>" {
        extend(&COMPRESSIONS);
    }
    if name == "<all_types_columns>" {
        extend(&ALL_TYPES_COLUMNS);
    }
    collection
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parser(text: &str) -> Parser {
        Parser::from_text(text, "test/x.test")
    }

    #[test]
    fn records_are_separated_by_blank_lines_and_comments() {
        let mut p = parser("# comment\nstatement ok\nSELECT 1\n\nquery I\nSELECT 2\n----\n2\n");
        assert!(p.next_statement());
        let token = p.tokenize().unwrap();
        assert_eq!(token.kind, TokenKind::Statement);
        assert_eq!(token.params, ["ok"]);
        p.next_line();
        assert_eq!(p.extract_statement(), "SELECT 1");
        assert!(p.next_statement());
        let token = p.tokenize().unwrap();
        assert_eq!(token.kind, TokenKind::Query);
        assert_eq!(p.current_line() + 1, 5);
        p.next_line();
        assert_eq!(p.extract_statement(), "SELECT 2");
        assert_eq!(p.extract_expected_result(), ["2"]);
        assert!(!p.next_statement());
    }

    #[test]
    fn multi_line_statement_and_error_message() {
        let mut p = parser("statement error\nSELECT\n  1 +\n----\nBinder Error\nline two\n\n");
        assert!(p.next_statement());
        p.next_line();
        assert_eq!(p.extract_statement(), "SELECT\n  1 +");
        let error = p.extract_expected_error(ExpectedResult::Error, false).unwrap();
        assert_eq!(error, "Binder Error\nline two");
    }

    #[test]
    fn statement_error_requires_message_except_in_sqlite_tests() {
        let mut p = parser("statement error\nSELECT x\n");
        p.next_statement();
        p.next_line();
        p.extract_statement();
        assert!(p.extract_expected_error(ExpectedResult::Error, false).is_err());
        assert_eq!(p.extract_expected_error(ExpectedResult::Error, true).unwrap(), "");
        let mut p = parser("statement ok\nSELECT 1\n----\nfoo\n");
        p.next_statement();
        p.next_line();
        p.extract_statement();
        let failure = p
            .extract_expected_error(ExpectedResult::Success, false)
            .unwrap_err();
        assert_eq!(failure.line, Some(3));
    }

    #[test]
    fn tokenizer_splits_on_any_whitespace() {
        let p = parser("query  III\trowsort label-1");
        let token = p.tokenize().unwrap();
        assert_eq!(token.params, ["III", "rowsort", "label-1"]);
        assert!(parser("bogus 1").tokenize().is_err());
        assert!(parser("   ").tokenize().is_err());
    }

    #[test]
    fn crlf_and_trailing_newline_handling() {
        let mut p = parser("statement ok\r\nSELECT 1\r\n");
        p.next_statement();
        p.next_line();
        assert_eq!(p.extract_statement(), "SELECT 1");
        assert!(!p.next_statement());
    }

    #[test]
    fn single_line_records_must_be_followed_by_blank_line() {
        let p = parser("mode skip\nstatement ok\n");
        assert!(p.tokenize().unwrap().kind.is_single_line());
        assert!(!p.next_line_empty_or_comment());
        assert!(parser("halt\n# done").next_line_empty_or_comment());
    }

    #[test]
    fn conditions() {
        let p = parser("");
        assert_eq!(parse_conditions(&p, "duckdb", true).unwrap(), None);
        let conditions = parse_conditions(&p, "i>=2&&type<>varchar", false)
            .unwrap()
            .unwrap();
        assert_eq!(conditions.len(), 2);
        assert_eq!(conditions[0].comparison, Comparison::GreaterThanOrEqual);
        assert_eq!(
            (conditions[0].keyword.as_str(), conditions[0].value.as_str()),
            ("i", "2")
        );
        assert_eq!(conditions[1].comparison, Comparison::NotEqual);
        assert!(!conditions[1].skip_if);
        assert!(parse_conditions(&p, "i=1=2", true).is_err());
    }

    #[test]
    fn sort_styles() {
        assert_eq!(SortStyle::parse("sort"), Some(SortStyle::RowSort));
        assert_eq!(SortStyle::parse("valuesort"), Some(SortStyle::ValueSort));
        assert_eq!(SortStyle::parse("con1"), None);
    }

    #[test]
    fn foreach_collections_and_removal() {
        let mut tokens = Vec::new();
        assert!(expand_foreach_token("<integral>", &mut tokens));
        assert_eq!(tokens.len(), 10);
        assert!(expand_foreach_token("!hugeint", &mut tokens));
        assert!(!tokens.contains(&"hugeint".to_owned()));
        assert!(!expand_foreach_token("!missing", &mut tokens));
        assert!(!expand_foreach_token("varchar", &mut tokens));
        let mut tokens = Vec::new();
        assert!(expand_foreach_token("<numeric>", &mut tokens));
        assert_eq!(tokens.last().map(String::as_str), Some("double"));
        let mut tokens = Vec::new();
        assert!(expand_foreach_token("<ALL_TYPES_COLUMNS>", &mut tokens));
        assert_eq!(tokens.len(), 52);
        let mut tokens = Vec::new();
        assert!(expand_foreach_token("<compression>", &mut tokens));
        assert_eq!(tokens.len(), 9);
    }
}
