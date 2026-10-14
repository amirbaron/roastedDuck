//! Engine-independent loader for `<FILE>:` expected results.
//!
//! DuckDB's runner reads these files with `read_csv(path, header=1, sep='|', columns=<all
//! VARCHAR>, auto_detect=false)`. The Rust backend cannot run SQL yet, so this module implements
//! that exact dialect: `|` delimiter, `"` quote and escape, a header row, `\n`/`\r\n`/`\r` line
//! endings, unquoted empty fields as NULL (rendered `NULL`). A blank line is a NULL row when
//! the file has a single column and is skipped otherwise.

use std::fs;

/// Reads `path` and returns its values row-major, rendered like `Value::ToString()`.
pub fn load(path: &str, columns: usize) -> Result<Vec<String>, String> {
    let bytes = fs::read(path).map_err(|error| read_error(path, &error.to_string()))?;
    let text = String::from_utf8_lossy(&bytes);
    let rows = parse(&text).map_err(|error| read_error(path, &error))?;
    let mut values = Vec::new();
    for (index, row) in rows.into_iter().enumerate().skip(1) {
        if row.is_empty() {
            if columns == 1 {
                values.push("NULL".to_owned());
            }
            continue;
        }
        if row.len() != columns {
            return Err(read_error(
                path,
                &format!(
                    "Invalid Input Error: CSV Error on Line: {}\nExpected Number of Columns: {columns} Found: {}",
                    index + 1,
                    row.len()
                ),
            ));
        }
        values.extend(
            row.into_iter()
                .map(|field| field.unwrap_or_else(|| "NULL".to_owned())),
        );
    }
    Ok(values)
}

fn read_error(path: &str, message: &str) -> String {
    format!("Could not read CSV File \"{path}\": {message}")
}

/// Splits CSV text into rows of fields; `None` is a NULL (unquoted empty) field and a blank line
/// is an empty row.
fn parse(text: &str) -> Result<Vec<Vec<Option<String>>>, String> {
    let mut rows = Vec::new();
    let mut row: Vec<Option<String>> = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    let mut line_has_content = false;
    while let Some(c) = chars.next() {
        match c {
            '"' if field.is_empty() && !quoted => {
                quoted = true;
                line_has_content = true;
                loop {
                    match chars.next() {
                        Some('"') if chars.peek() == Some(&'"') => {
                            chars.next();
                            field.push('"');
                        }
                        Some('"') => break,
                        Some(other) => field.push(other),
                        None => return Err("Value with unterminated quote found.".to_owned()),
                    }
                }
            }
            '|' => {
                row.push(finish_field(&mut field, &mut quoted));
                line_has_content = true;
            }
            '\n' | '\r' => {
                if c == '\r' && chars.peek() == Some(&'\n') {
                    chars.next();
                }
                if line_has_content || !field.is_empty() {
                    row.push(finish_field(&mut field, &mut quoted));
                }
                rows.push(std::mem::take(&mut row));
                line_has_content = false;
            }
            other if quoted => {
                return Err(format!(
                    "Value with unexpected character '{other}' after a closing quote."
                ));
            }
            other => {
                field.push(other);
                line_has_content = true;
            }
        }
    }
    if line_has_content || !field.is_empty() {
        row.push(finish_field(&mut field, &mut quoted));
        rows.push(row);
    }
    Ok(rows)
}

fn finish_field(field: &mut String, quoted: &mut bool) -> Option<String> {
    let value = std::mem::take(field);
    let was_quoted = std::mem::replace(quoted, false);
    if value.is_empty() && !was_quoted {
        None
    } else {
        Some(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(row: &[Option<&str>]) -> Vec<Option<String>> {
        row.iter().map(|f| f.map(str::to_owned)).collect()
    }

    #[test]
    fn quoting_nulls_and_line_endings() {
        let rows = parse("a|b\r\n1|\"x|\"\"y\"\"\"\n|\"\"\n\n2|z").unwrap();
        assert_eq!(rows.len(), 5);
        assert_eq!(rows[1], fields(&[Some("1"), Some("x|\"y\"")]));
        assert_eq!(rows[2], fields(&[None, Some("")]));
        assert_eq!(rows[3], Vec::<Option<String>>::new());
        assert_eq!(rows[4], fields(&[Some("2"), Some("z")]));
    }

    #[test]
    fn quoted_newlines_stay_in_the_value() {
        let rows = parse("h\n\"line1\nline2\"\n").unwrap();
        assert_eq!(rows[1], fields(&[Some("line1\nline2")]));
    }

    #[test]
    fn blank_lines_are_null_rows_only_for_single_columns() {
        let dir = std::env::temp_dir().join(format!("slt-result-file-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("answer.csv");
        fs::write(&path, "avg\n\n").unwrap();
        assert_eq!(load(path.to_str().unwrap(), 1).unwrap(), ["NULL"]);
        fs::write(&path, "a|b\n1|2\n\n").unwrap();
        assert_eq!(load(path.to_str().unwrap(), 2).unwrap(), ["1", "2"]);
        fs::write(&path, "a|b\n1\n").unwrap();
        assert!(load(path.to_str().unwrap(), 2).is_err());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn unterminated_quote_is_an_error() {
        assert!(parse("h\n\"abc\n").is_err());
    }
}
