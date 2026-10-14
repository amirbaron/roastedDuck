//! Statement extraction from sqllogictest files (DuckDB's dialect).

use std::path::{Path, PathBuf};

/// The kind of record a statement came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordKind {
    /// `statement ok`
    StatementOk,
    /// `statement error`
    StatementError,
    /// `statement maybe`
    StatementMaybe,
    /// `query ...`
    Query,
}

/// One SQL text from a `statement` or `query` record.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Statement {
    /// Path of the test file, relative to the corpus root.
    pub file: String,
    /// 1-based line of the record header.
    pub line: usize,
    pub kind: RecordKind,
    pub sql: String,
}

/// Whether `sql` still contains a loop variable the runner would substitute
/// (`${i}`, `{i}`); such statements are not meaningful on their own.
pub fn has_unsubstituted_variable(sql: &str) -> bool {
    if sql.contains("${") {
        return true;
    }
    let bytes = sql.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{' {
            let start = i + 1;
            let mut end = start;
            while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
                end += 1;
            }
            if end > start && end < bytes.len() && bytes[end] == b'}' && bytes[start].is_ascii_alphabetic() {
                return true;
            }
        }
        i += 1;
    }
    false
}

fn record_kind(header: &str) -> Option<RecordKind> {
    let mut words = header.split_whitespace();
    match (words.next()?, words.next()) {
        ("statement", Some("ok")) => Some(RecordKind::StatementOk),
        ("statement", Some("error")) => Some(RecordKind::StatementError),
        ("statement", Some("maybe")) => Some(RecordKind::StatementMaybe),
        ("query", _) => Some(RecordKind::Query),
        _ => None,
    }
}

/// Extracts the SQL of every `statement` and `query` record in one file: the lines after
/// the header up to a blank line or `----`.
pub fn extract_file(text: &str, file: &str) -> Vec<Statement> {
    let lines: Vec<&str> = text.lines().collect();
    let mut statements = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let Some(kind) = record_kind(lines[i]) else {
            i += 1;
            continue;
        };
        let header_line = i + 1;
        i += 1;
        let mut sql = Vec::new();
        while i < lines.len() && !lines[i].trim().is_empty() && lines[i].trim_end() != "----" {
            sql.push(lines[i]);
            i += 1;
        }
        let sql = sql.join("\n");
        if !sql.trim().is_empty() {
            statements.push(Statement {
                file: file.to_owned(),
                line: header_line,
                kind,
                sql,
            });
        }
    }
    statements
}

fn collect_test_files(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)?.collect::<std::io::Result<_>>()?;
    entries.sort_by_key(std::fs::DirEntry::path);
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            collect_test_files(&path, out)?;
        } else if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e == "test" || e == "test_slow")
        {
            out.push(path);
        }
    }
    Ok(())
}

/// Every statement of every `.test` / `.test_slow` file under `root`, in path order.
/// Files that are not UTF-8 are skipped; the second value counts them.
pub fn extract_dir(root: &Path) -> std::io::Result<(Vec<Statement>, usize)> {
    let mut files = Vec::new();
    collect_test_files(root, &mut files)?;
    let mut statements = Vec::new();
    let mut skipped = 0;
    for path in files {
        let Ok(text) = std::fs::read_to_string(&path) else {
            skipped += 1;
            continue;
        };
        let relative = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .into_owned();
        statements.extend(extract_file(&text, &relative));
    }
    Ok((statements, skipped))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_records() {
        let text = "# comment\nstatement ok\nCREATE TABLE t(i INT);\n\nquery I rowsort\nSELECT 1\nFROM t\n----\n1\n\nstatement error\nSELEC\n----\nParser Error\n\nloop i 0 3\nstatement ok\nSELECT ${i}\n\nendloop\n";
        let statements = extract_file(text, "x.test");
        let sql: Vec<&str> = statements.iter().map(|s| s.sql.as_str()).collect();
        assert_eq!(
            sql,
            [
                "CREATE TABLE t(i INT);",
                "SELECT 1\nFROM t",
                "SELEC",
                "SELECT ${i}"
            ]
        );
        assert_eq!(statements[1].line, 5);
        assert_eq!(statements[2].kind, RecordKind::StatementError);
        assert!(has_unsubstituted_variable(&statements[3].sql));
        assert!(has_unsubstituted_variable("SELECT {i}"));
        assert!(!has_unsubstituted_variable("SELECT {'a': 1}"));
    }
}
