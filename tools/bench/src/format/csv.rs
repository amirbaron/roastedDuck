//! Reader for expected-result files.
//!
//! DuckDB reads them with `read_csv(file, delim='|', header=1, nullstr='NULL',
//! all_varchar=1, quote='"', escape='"')` and renders every value with
//! `GetValue<string>`, so a `NULL` field and the string `NULL` are indistinguishable.
//! This reader produces the same strings without needing a SQL engine.

/// A parsed answer file: the header's column count and the data rows.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct AnswerFile {
    pub column_count: usize,
    pub rows: Vec<Vec<String>>,
}

/// Parses a `|`-delimited, `"`-quoted answer file with a header row.
pub(super) fn parse_answer_file(text: &str) -> Result<AnswerFile, String> {
    let mut records = Records::new(text);
    let header = records.next().transpose()?.ok_or("answer file is empty")?;
    let column_count = header.len();
    let mut rows = Vec::new();
    for (index, record) in records.enumerate() {
        let record = record?;
        if record.len() != column_count {
            return Err(format!(
                "answer file row {} has {} values, expected {column_count}",
                index + 1,
                record.len()
            ));
        }
        rows.push(record);
    }
    Ok(AnswerFile { column_count, rows })
}

const DELIMITER: char = '|';
const QUOTE: char = '"';

/// Iterator over CSV records; skips empty lines like DuckDB's reader.
struct Records<'a> {
    chars: std::iter::Peekable<std::str::Chars<'a>>,
}

impl<'a> Records<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            chars: text.chars().peekable(),
        }
    }

    fn read_record(&mut self) -> Result<Vec<String>, String> {
        let mut fields = Vec::new();
        let mut field = String::new();
        let mut quoted = false;
        loop {
            match self.chars.next() {
                None | Some('\n') => break,
                Some(QUOTE) if field.is_empty() && !quoted => {
                    quoted = true;
                    self.read_quoted(&mut field)?;
                }
                Some(DELIMITER) => {
                    fields.push(std::mem::take(&mut field));
                    quoted = false;
                }
                Some('\r') if self.chars.peek() == Some(&'\n') => {}
                Some(c) => field.push(c),
            }
        }
        fields.push(field);
        Ok(fields)
    }

    /// Reads a quoted field body; `""` is an escaped quote.
    fn read_quoted(&mut self, field: &mut String) -> Result<(), String> {
        loop {
            match self.chars.next() {
                None => return Err("unterminated quoted value in answer file".to_owned()),
                Some(QUOTE) if self.chars.peek() == Some(&QUOTE) => {
                    self.chars.next();
                    field.push(QUOTE);
                }
                Some(QUOTE) => return Ok(()),
                Some(c) => field.push(c),
            }
        }
    }
}

impl Iterator for Records<'_> {
    type Item = Result<Vec<String>, String>;

    fn next(&mut self) -> Option<Self::Item> {
        // Skip blank lines.
        while matches!(self.chars.peek(), Some('\n' | '\r')) {
            self.chars.next();
        }
        self.chars.peek()?;
        Some(self.read_record())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_header_and_rows() {
        let file = parse_answer_file("a|b\n1|x\n2|NULL\n").unwrap();
        assert_eq!(file.column_count, 2);
        assert_eq!(file.rows, vec![vec!["1", "x"], vec!["2", "NULL"]]);
    }

    #[test]
    fn handles_quotes_crlf_and_blank_lines() {
        let file = parse_answer_file("a|b\r\n\"x|y\"|\"say \"\"hi\"\"\"\r\n\n\"multi\nline\"|\n").unwrap();
        assert_eq!(
            file.rows,
            vec![vec!["x|y", "say \"hi\""], vec!["multi\nline", ""]]
        );
    }

    #[test]
    fn rejects_ragged_rows() {
        assert!(parse_answer_file("a|b\n1\n").is_err());
        assert!(parse_answer_file("").is_err());
    }
}
