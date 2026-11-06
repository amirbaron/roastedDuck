//! The text measurements the shell renders with: DuckDB's `Utf8Proc` helpers
//! (`third_party/utf8proc/utf8proc_wrapper.cpp`) and linenoise's render width of a
//! terminal string (`Linenoise::ComputeRenderWidth`).

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthChar;

/// The render width of the code point starting at byte `pos` of `text`
/// (`Utf8Proc::RenderWidth(const char *, size_t, size_t)`: utf8proc's `charwidth`, zero
/// for control characters and combining marks).
pub fn char_render_width(text: &str, pos: usize) -> usize {
    text.get(pos..)
        .and_then(|rest| rest.chars().next())
        .and_then(UnicodeWidthChar::width)
        .unwrap_or(0)
}

/// The render width of `text` (`Utf8Proc::RenderWidth(const string &)`): the width of
/// the first code point of every grapheme cluster, so that combining marks and emoji
/// sequences count once.
pub fn render_width(text: &str) -> usize {
    text.graphemes(true)
        .map(|grapheme| {
            grapheme
                .chars()
                .next()
                .and_then(UnicodeWidthChar::width)
                .unwrap_or(0)
        })
        .sum()
}

/// Native `Utf8Proc` glyph widths, without terminal tab/newline/ANSI interpretation.
pub fn native_render_width(bytes: &[u8]) -> rustdb::Result<usize> {
    let mut width = 0;
    let mut position = 0;
    while position < bytes.len() {
        let (next, cluster_width) = rustdb_common::utf8::native_grapheme_cluster(bytes, position)?;
        width += cluster_width;
        position = next;
    }
    Ok(width)
}

/// The byte offset of the grapheme cluster after the one at `pos`
/// (`Utf8Proc::NextGraphemeCluster`).
pub fn next_grapheme_cluster(text: &str, pos: usize) -> usize {
    text.get(pos..)
        .and_then(|rest| rest.graphemes(true).next())
        .map_or(pos + 1, |grapheme| pos + grapheme.len())
}

/// The byte offset of the grapheme cluster before `pos`
/// (`Utf8Proc::PreviousGraphemeCluster`).
pub fn previous_grapheme_cluster(text: &str, pos: usize) -> usize {
    text.get(..pos)
        .and_then(|head| head.grapheme_indices(true).next_back())
        .map_or(0, |(offset, _)| offset)
}

/// The code point starting at byte `pos` of `bytes` and its length
/// (`Utf8Proc::UTF8ToCodepoint`), `None` if the bytes there are not UTF-8.
pub fn codepoint_at(bytes: &[u8], pos: usize) -> Option<(char, usize)> {
    let first = *bytes.get(pos)?;
    let length = match first {
        0..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => return None,
    };
    let text = std::str::from_utf8(bytes.get(pos..pos + length)?).ok()?;
    text.chars().next().map(|c| (c, length))
}

/// `Utf8Proc::RenderWidth(const char *, size_t, size_t)` on bytes that may not be UTF-8:
/// an invalid byte takes no width.
pub fn byte_render_width(bytes: &[u8], pos: usize) -> usize {
    codepoint_at(bytes, pos)
        .and_then(|(c, _)| UnicodeWidthChar::width(c))
        .unwrap_or(0)
}

/// The UTF-8 text at the start of `bytes`, at most `limit` bytes of it.
fn valid_prefix(bytes: &[u8], limit: usize) -> &str {
    let window = &bytes[..limit.min(bytes.len())];
    match std::str::from_utf8(window) {
        Ok(text) => text,
        Err(error) => std::str::from_utf8(&window[..error.valid_up_to()]).unwrap_or_default(),
    }
}

/// `Utf8Proc::NextGraphemeCluster` on bytes that may not be UTF-8: an invalid byte is a
/// cluster of its own.
pub fn next_grapheme_cluster_bytes(bytes: &[u8], pos: usize) -> usize {
    let rest = &bytes[pos.min(bytes.len())..];
    let mut limit = 64;
    loop {
        let text = valid_prefix(rest, limit);
        let Some(grapheme) = text.graphemes(true).next() else {
            return pos + 1;
        };
        // A cluster reaching the end of the window may continue past it.
        if grapheme.len() < text.len() || text.len() < limit.min(rest.len()) || limit >= rest.len() {
            return pos + grapheme.len();
        }
        limit *= 2;
    }
}

/// `Utf8Proc::PreviousGraphemeCluster` on bytes: the previous byte if they are not UTF-8.
pub fn previous_grapheme_cluster_bytes(bytes: &[u8], pos: usize) -> usize {
    match std::str::from_utf8(bytes) {
        Ok(text) => previous_grapheme_cluster(text, pos),
        Err(_) => pos.saturating_sub(1),
    }
}

/// The render width of a string printed to a terminal (`Linenoise::ComputeRenderWidth`,
/// the shell's `RenderLength`): a newline restarts the count, tabs advance to the next
/// multiple of 8, ANSI escape sequences take no space and every byte of invalid UTF-8
/// takes one column.
pub fn native_terminal_render_width(bytes: &[u8]) -> rustdb::Result<usize> {
    let mut width = 0;
    let mut pos = 0;
    while pos < bytes.len() {
        match bytes[pos] {
            b'\n' => {
                width = 0;
                pos += 1;
            }
            b'\t' => {
                width += 8 - (width % 8);
                pos += 1;
            }
            _ if skip_ansi_escape(bytes, &mut pos) => {}
            _ => {
                let (next, cluster_width) = rustdb_common::utf8::native_grapheme_cluster(bytes, pos)?;
                width += cluster_width;
                pos = next;
            }
        }
    }
    Ok(width)
}

pub fn terminal_render_width(bytes: &[u8]) -> usize {
    if let Ok(width) = native_terminal_render_width(bytes) {
        return width;
    }
    let mut render_width = 0;
    for chunk in bytes.utf8_chunks() {
        let text = chunk.valid();
        let bytes = text.as_bytes();
        let mut cpos = 0;
        while cpos < bytes.len() {
            match bytes[cpos] {
                b'\n' => {
                    render_width = 0;
                    cpos += 1;
                }
                b'\t' => {
                    render_width += 8 - (render_width % 8);
                    cpos += 1;
                }
                _ if skip_ansi_escape(bytes, &mut cpos) => {}
                _ => {
                    render_width += char_render_width(text, cpos);
                    cpos = next_grapheme_cluster(text, cpos);
                }
            }
        }
        // Every invalid byte takes one column.
        render_width += chunk.invalid().len();
    }
    render_width
}

/// Skips the ANSI escape sequence at `cpos`, if there is one (`Linenoise::HandleANSIEscape`):
/// a CSI sequence up to its final byte, or the byte after a lone ESC.
pub fn skip_ansi_escape(bytes: &[u8], cpos: &mut usize) -> bool {
    if bytes[*cpos] != 0x1b {
        return false;
    }
    *cpos += 1;
    if *cpos < bytes.len() && bytes[*cpos] == b'[' {
        *cpos += 1;
        while *cpos < bytes.len() && !(b'@'..=b'~').contains(&bytes[*cpos]) {
            *cpos += 1;
        }
        if *cpos < bytes.len() {
            *cpos += 1;
        }
    } else {
        *cpos += 1;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_render_width_retains_cpp_malformed_sequence_behavior() {
        assert_eq!(native_terminal_render_width(b"caf\xe9").unwrap(), 5);
        let error = native_terminal_render_width(b"a\xffb").unwrap_err();
        assert_eq!(
            error.raw_message_bytes(),
            b"invalid code point detected in Utf8Proc::UTF8ToCodepoint, likely due to invalid UTF-8"
        );
        assert_eq!(native_terminal_render_width("雪".as_bytes()).unwrap(), 2);
    }

    #[test]
    fn widths() {
        assert_eq!(render_width("abc"), 3);
        assert_eq!(render_width("🦆"), 2);
        assert_eq!(render_width("e\u{301}"), 1);
        assert_eq!(terminal_render_width(b"\x1b[1mab\x1b[00m"), 2);
        assert_eq!(terminal_render_width(b"a\tb"), 9);
        assert_eq!(terminal_render_width(b"ab\ncd"), 2);
        assert_eq!(next_grapheme_cluster("ab", 0), 1);
        assert_eq!(previous_grapheme_cluster("ab", 2), 1);
    }
}
