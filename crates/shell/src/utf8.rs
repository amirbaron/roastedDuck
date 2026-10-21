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
        .map(|grapheme| grapheme.chars().next().and_then(UnicodeWidthChar::width).unwrap_or(0))
        .sum()
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

/// The render width of a string printed to a terminal (`Linenoise::ComputeRenderWidth`,
/// the shell's `RenderLength`): a newline restarts the count, tabs advance to the next
/// multiple of 8, ANSI escape sequences take no space and every byte of invalid UTF-8
/// takes one column.
pub fn terminal_render_width(bytes: &[u8]) -> usize {
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
