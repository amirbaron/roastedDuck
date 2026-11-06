//! Rendering of an error position as query context (DuckDB's `QueryErrorContext::Format`):
//!
//! ```text
//! Binder Error: Referenced column "xyz" not found in FROM clause!
//!
//! LINE 1: select xyz from t
//!                ^
//! ```

use crate::ByteString;

const MAX_LINE_RENDER_WIDTH: usize = 120;

/// DuckDB's `StringUtil::CharacterIsOperator`.
fn is_operator(byte: u8) -> bool {
    byte != b'_' && matches!(byte, b'!'..=b'/' | b':'..=b'@' | b'['..=b'`' | b'{'..=b'~')
}

fn is_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | b'\x0b' | b'\x0c' | b'\r')
}

/// Appends the line of `query` containing byte `position`, with a caret under it.
#[cfg(test)]
pub(crate) fn format(query: &str, message: &str, position: u64) -> String {
    // Text input guarantees the resulting source slice is UTF-8.
    String::from_utf8(format_bytes(query.as_bytes(), message.as_bytes(), position).into_bytes())
        .expect("UTF-8 query context")
}

pub(crate) fn format_bytes(query: &[u8], message: &[u8], position: u64) -> ByteString {
    let Ok(error_location) = usize::try_from(position) else {
        return message.into();
    };
    if error_location >= query.len() {
        return message.into();
    }
    let bytes = query;
    let mut line_start = 0;
    let mut line_number = 1;
    for i in 0..error_location {
        let is_newline = match bytes[i] {
            b'\r' => i + 1 >= error_location || bytes[i + 1] != b'\n',
            b'\n' => true,
            _ => false,
        };
        if is_newline {
            line_number += 1;
            line_start = i + 1;
        }
    }
    let end_pos = bytes[error_location..]
        .iter()
        .position(|&b| b == b'\n' || b == b'\r')
        .map_or(query.len(), |offset| error_location + offset);

    // Grapheme clusters of the line with their byte offsets and render widths.
    let line = &query[line_start..end_pos];
    let (positions, widths, natural_break) = {
        let mut positions = Vec::new();
        let mut widths = Vec::new();
        let mut breaks = Vec::new();
        let valid = !matches!(crate::utf8::analyze(line), crate::utf8::UnicodeType::Invalid(_));
        let mut offset = 0;
        while offset < line.len() {
            let (next, width) = if valid {
                // The line was validated before calling utf8proc's native traversal.
                #[allow(
                    clippy::expect_used,
                    reason = "strict UTF-8 validation precedes native cluster traversal"
                )]
                let cluster =
                    crate::tree_renderer::unicode::cluster(line, offset).expect("validated UTF-8 cluster");
                cluster
            } else {
                (offset + 1, 1)
            };
            positions.push(offset);
            widths.push(width);
            breaks.push(is_operator(line[offset]) || is_space(line[offset]));
            offset = next;
        }
        (positions, widths, breaks)
    };

    let target = error_location - line_start;
    let epos = positions.iter().position(|&p| p >= target).unwrap_or(0);
    let error_line_start = line_start;
    let mut start_pos = line_start;
    let mut end_pos = end_pos;
    let mut truncate_beginning = false;
    let mut truncate_end = false;
    let mut spos = 0;

    let mut width = 0;
    let mut i = epos;
    while i > 0 {
        width += widths[i];
        if width >= MAX_LINE_RENDER_WIDTH / 2 {
            let start_scan = i.saturating_sub(4);
            let end_scan = (i + 4).min(epos);
            if let Some(k) = (start_scan..end_scan).find(|&k| natural_break[k]) {
                i = k;
            }
            truncate_beginning = true;
            start_pos += positions[i];
            spos = i;
            break;
        }
        i -= 1;
    }

    width = 0;
    for mut i in epos..positions.len() {
        width += widths[i];
        if width >= MAX_LINE_RENDER_WIDTH / 2 {
            let start_scan = if i > epos + 4 { i - 4 } else { epos };
            let end_scan = (i + 4).min(positions.len());
            if let Some(k) = (start_scan..end_scan).find(|&k| natural_break[k]) {
                i = k;
            }
            truncate_end = true;
            end_pos = error_line_start + positions[i];
            break;
        }
    }

    let line_indicator = format!("LINE {line_number}: ");
    let begin_trunc = if truncate_beginning { "..." } else { "" };
    let end_trunc = if truncate_end { "..." } else { "" };
    let caret_offset: usize =
        widths[spos..epos].iter().sum::<usize>() + line_indicator.len() + begin_trunc.len();
    let shown = &query[start_pos..end_pos];
    crate::byte_format!(
        "{message}\n\n{line_indicator}{begin_trunc}{shown}{end_trunc}\n{}^",
        " ".repeat(caret_offset)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_like_duckdb() {
        let rendered = format(
            "select xyz from (select 1 as a) t",
            "Referenced column \"xyz\" not found in FROM clause!\nCandidate bindings: \"a\"",
            7,
        );
        assert_eq!(
            rendered,
            "Referenced column \"xyz\" not found in FROM clause!\nCandidate bindings: \"a\"\n\n\
             LINE 1: select xyz from (select 1 as a) t\n               ^"
        );
        assert_eq!(format("a\nbc", "m", 3), "m\n\nLINE 2: bc\n         ^");
        assert_eq!(format("abc", "m", 10), "m");
    }
}
