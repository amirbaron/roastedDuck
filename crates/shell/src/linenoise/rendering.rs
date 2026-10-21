//! Rendering of the line being edited (DuckDB's `tools/shell/linenoise/rendering.cpp`):
//! single-line and multi-line refreshes, continuation markers, the history search line,
//! error and bracket highlighting, and the completion hint and list.

use crate::highlight::{self, HighlightElementType, PrintIntensity, RESET_TERMINAL_CODE};
use crate::utf8;

use super::highlighting::{self, ExtraHighlighting, ExtraHighlightingType, HighlightToken, TokenType};
use super::{
    COMPLETION_RENDERING, CompletionType, ERROR_RENDERING, Linenoise, compute_render_width, history, terminal,
};

/// What of a multi-line edit is cut off (`RenderTruncation`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RenderTruncation {
    NoTruncate,
    TruncateTop,
    TruncateBottom,
    TruncateBoth,
}

/// `renderText`: the part of `buf` that fits in `cols` columns after a prompt of `plen`
/// columns while showing the cursor at `pos`, highlighted if `highlight`; also returns the
/// column of the cursor in it. (Like DuckDB, the fit is measured in bytes.)
fn render_text(buf: &[u8], pos: usize, cols: usize, plen: usize, highlight: bool) -> (Vec<u8>, usize) {
    let mut render_pos: usize = 0;
    if std::str::from_utf8(buf).is_err() {
        // Not UTF-8: a byte per column.
        let mut start = 0;
        let mut len = buf.len();
        let mut pos = pos;
        while plen + pos >= cols && len > 0 {
            start += 1;
            len -= 1;
            pos = pos.wrapping_sub(1);
        }
        while plen + len > cols && len > 0 {
            len -= 1;
        }
        return (buf[start..start + len].to_vec(), pos);
    }
    let remaining_render_width = cols.wrapping_sub(plen).wrapping_sub(1);
    let mut start_pos = 0;
    let mut cpos = 0;
    let mut total_render_width: usize = 0;
    while cpos < buf.len() {
        let char_render_width = utf8::byte_render_width(buf, cpos);
        let prev_pos = cpos;
        cpos = utf8::next_grapheme_cluster_bytes(buf, cpos);
        total_render_width += cpos - prev_pos;
        if total_render_width >= remaining_render_width {
            if prev_pos >= pos {
                // Past the cursor: stop.
                cpos = prev_pos;
                break;
            }
            // Before the cursor: drop characters from the start until it fits.
            while total_render_width >= remaining_render_width {
                let start_char_width = utf8::byte_render_width(buf, start_pos);
                let new_start = utf8::next_grapheme_cluster_bytes(buf, start_pos);
                total_render_width -= new_start - start_pos;
                start_pos = new_start;
                render_pos = render_pos.wrapping_sub(start_char_width);
            }
        }
        if prev_pos < pos {
            render_pos = render_pos.wrapping_add(char_render_width);
        }
    }
    let text = if highlight {
        let tokens = highlighting::tokenize(buf, buf.first() == Some(&b'.'));
        highlighting::highlight_text(buf, start_pos, cpos, &tokens)
    } else {
        buf[start_pos..cpos].to_vec()
    };
    (text, render_pos)
}

/// `InsertToken`: a token of one byte at `insert_pos`.
fn insert_token(insert_type: TokenType, insert_pos: usize, tokens: &mut Vec<HighlightToken>) {
    let mut new_tokens = Vec::with_capacity(tokens.len() + 1);
    let mut found = false;
    let mut i = 0;
    while i < tokens.len() {
        if tokens[i].start == insert_pos {
            new_tokens.push(HighlightToken::new(insert_type, insert_pos));
            // The token resumes after it, unless the next one starts right there.
            if i + 1 >= tokens.len() || tokens[i + 1].start > insert_pos + 1 {
                new_tokens.push(HighlightToken::new(tokens[i].token_type, insert_pos + 1));
            }
            i += 1;
            found = true;
            break;
        } else if tokens[i].start > insert_pos {
            new_tokens.push(HighlightToken::new(insert_type, insert_pos));
            new_tokens.push(tokens[i]);
            i += 1;
            found = true;
            break;
        }
        new_tokens.push(tokens[i]);
        i += 1;
    }
    new_tokens.extend_from_slice(&tokens[i..]);
    if !found {
        new_tokens.push(HighlightToken::new(insert_type, insert_pos));
    }
    *tokens = new_tokens;
}

/// `ScanState` of the error highlighting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScanState {
    Standard,
    InSingleQuote,
    InDoubleQuote,
    InComment,
    DollarQuotedString,
}

/// `OpenBracket`: brackets next to the cursor are highlighted with their match.
fn open_bracket(brackets: &mut Vec<usize>, cursor_brackets: &mut Vec<usize>, pos: usize, i: usize) {
    if pos == i {
        // The cursor is on this bracket: it wins.
        cursor_brackets.clear();
        cursor_brackets.push(i);
    }
    if cursor_brackets.is_empty() && (i + 1 == pos || pos + 1 == i) {
        cursor_brackets.push(i);
    }
    brackets.push(i);
}

/// `CloseBracket`: an unmatched closing bracket is an error.
fn close_bracket(
    brackets: &mut Vec<usize>,
    cursor_brackets: &mut Vec<usize>,
    pos: usize,
    i: usize,
    errors: &mut Vec<usize>,
) {
    if pos == i {
        cursor_brackets.clear();
    }
    let Some(&last) = brackets.last() else {
        errors.push(i);
        return;
    };
    if cursor_brackets.len() == 1 {
        if cursor_brackets.last() == Some(&last) {
            cursor_brackets.push(i);
        }
    } else if cursor_brackets.is_empty() && (pos == i || i + 1 == pos || pos + 1 == i) {
        cursor_brackets.push(i);
        cursor_brackets.push(last);
    }
    brackets.pop();
}

/// `IsCompletionCharacter`: the characters a completion hint follows.
fn is_completion_character(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_'
}

impl Linenoise<'_> {
    /// `Linenoise::RefreshSingleLine`: the line scrolled to show the cursor.
    fn refresh_single_line(&self) {
        let plen = self.prompt_width();
        let (render_buf, render_pos) = render_text(
            &self.buf,
            self.pos,
            self.ws.ws_col as usize,
            plen,
            highlighting::is_enabled(),
        );
        let mut append_buffer = Vec::new();
        append_buffer.push(b'\r');
        append_buffer.extend_from_slice(&self.prompt);
        append_buffer.extend_from_slice(&render_buf);
        // Erase to the right, and move the cursor back.
        append_buffer.extend_from_slice(b"\x1b[0K");
        append_buffer
            .extend_from_slice(format!("\r\x1b[{}C", (render_pos.wrapping_add(plen)) as i32).as_bytes());
        Self::write(self.ofd, &append_buffer);
    }

    /// `Linenoise::RefreshSearch`: the search text and match count as the prompt, the
    /// matching history entry as the line.
    pub(super) fn refresh_search(&mut self) {
        const SEARCH_PROMPT_RENDER_SIZE: usize = 28;
        let mut no_matches_text: &[u8] = b"(no matches)";
        let no_matches = self.search_index >= self.search_matches.len();
        let mut search_prompt: Vec<u8>;
        if self.search_buf.is_empty() {
            search_prompt = b"search".to_vec();
            search_prompt.extend(std::iter::repeat_n(b' ', SEARCH_PROMPT_RENDER_SIZE - 8));
            search_prompt.extend_from_slice(b"> ");
            no_matches_text = b"(type to search)";
        } else {
            let search_text = self.search_buf.clone();
            let matches_text = if no_matches {
                String::new()
            } else {
                format!("{}/{}", self.search_index + 1, self.search_matches.len())
            };
            let search_text_length = compute_render_width(&search_text);
            let matches_text_length = compute_render_width(matches_text.as_bytes());
            let total_text_length = search_text_length + matches_text_length;
            if total_text_length < SEARCH_PROMPT_RENDER_SIZE - 2 {
                search_prompt = search_text;
                search_prompt.extend(std::iter::repeat_n(
                    b' ',
                    SEARCH_PROMPT_RENDER_SIZE - 2 - total_text_length,
                ));
                search_prompt.extend_from_slice(matches_text.as_bytes());
                search_prompt.extend_from_slice(b"> ");
            } else {
                // Too long: the end of the search text.
                let render_matches = matches_text_length < SEARCH_PROMPT_RENDER_SIZE - 8;
                let mut max_render_size = SEARCH_PROMPT_RENDER_SIZE - 3;
                if render_matches {
                    max_render_size -= matches_text_length;
                }
                let (shown, search_render_pos) =
                    render_text(&search_text, search_text.len(), max_render_size, 0, false);
                search_prompt = shown;
                search_prompt.resize(
                    search_prompt.len() + max_render_size.saturating_sub(search_render_pos),
                    b' ',
                );
                if render_matches {
                    search_prompt.extend_from_slice(matches_text.as_bytes());
                }
                search_prompt.extend_from_slice(b"> ");
            }
        }
        let old_highlighting = highlight::is_enabled();
        let saved_prompt = std::mem::replace(&mut self.prompt, search_prompt);
        let (shown, shown_pos) = if no_matches || self.search_buf.is_empty() {
            // The no-matches text is not highlighted.
            highlight::set_highlighting(false);
            (no_matches_text.to_vec(), 0)
        } else {
            let found = self.search_matches[self.search_index];
            (history::entry(found.history_index), found.match_end)
        };
        let saved_buf = std::mem::replace(&mut self.buf, shown);
        let saved_pos = std::mem::replace(&mut self.pos, shown_pos);
        self.refresh_line();
        if old_highlighting {
            highlight::set_highlighting(true);
        }
        self.buf = saved_buf;
        self.pos = saved_pos;
        self.prompt = saved_prompt;
    }

    /// `Linenoise::AddContinuationMarkers`: the text with a continuation prompt after
    /// each newline (the selected one on the cursor's row, scroll markers at cut ends),
    /// moving the tokens past the inserted bytes.
    fn add_continuation_markers(
        &self,
        buf: &[u8],
        plen: i32,
        cursor_row: i32,
        rows_to_render: usize,
        tokens: &mut Vec<HighlightToken>,
        truncation: RenderTruncation,
    ) -> Vec<u8> {
        let mut result = Vec::with_capacity(buf.len());
        let mut rows = 1;
        let mut cols = plen;
        let mut cpos = 0;
        let mut prev_pos = 0;
        let mut extra_bytes = 0;
        let mut token_position = 0;
        let mut new_tokens = Vec::with_capacity(tokens.len());
        let truncate_top = matches!(
            truncation,
            RenderTruncation::TruncateTop | RenderTruncation::TruncateBoth
        );
        let truncate_bottom = matches!(
            truncation,
            RenderTruncation::TruncateBottom | RenderTruncation::TruncateBoth
        );
        while cpos < buf.len() {
            let is_newline = Self::is_newline(buf[cpos]);
            self.next_position(buf, &mut cpos, &mut rows, &mut cols, plen);
            result.extend_from_slice(&buf[prev_pos..cpos]);
            prev_pos = cpos;
            if !is_newline {
                continue;
            }
            let is_cursor_row = rows == cursor_row;
            let prompt = super::with_prompts(|prompts| {
                if !self.continuation_markers {
                    String::new()
                } else if is_cursor_row {
                    prompts.continuation_selected.clone()
                } else if truncate_top && rows == 2 {
                    prompts.scroll_up.clone()
                } else if truncate_bottom && rows as usize == rows_to_render + 1 {
                    prompts.scroll_down.clone()
                } else {
                    prompts.continuation.clone()
                }
            });
            let continuation_len = prompt.len();
            let continuation_render = compute_render_width(prompt.as_bytes());
            // Spaces before the prompt, to the width of the main prompt.
            result.resize(
                result.len() + (plen - continuation_render as i32).max(0) as usize,
                b' ',
            );
            result.extend_from_slice(prompt.as_bytes());
            let continuation_bytes = (plen as usize)
                .wrapping_sub(continuation_render)
                .wrapping_add(continuation_len);
            if token_position < tokens.len() {
                while token_position < tokens.len() {
                    if tokens[token_position].start >= cpos {
                        break;
                    }
                    tokens[token_position].start += extra_bytes;
                    new_tokens.push(tokens[token_position]);
                    token_position += 1;
                }
                let prev_type = if token_position > 0 {
                    tokens[token_position - 1].token_type
                } else {
                    TokenType::Identifier
                };
                new_tokens.push(HighlightToken::new(
                    if is_cursor_row {
                        TokenType::ContinuationSelected
                    } else {
                        TokenType::Continuation
                    },
                    cpos + extra_bytes,
                ));
                new_tokens.push(HighlightToken::new(
                    prev_type,
                    cpos + extra_bytes + continuation_bytes,
                ));
            }
            extra_bytes = extra_bytes.wrapping_add(continuation_bytes);
        }
        result.extend_from_slice(&buf[prev_pos..cpos]);
        for token in &tokens[token_position..] {
            let mut token = *token;
            token.start = token.start.wrapping_add(extra_bytes);
            new_tokens.push(token);
        }
        *tokens = new_tokens;
        result
    }

    /// `Linenoise::AddErrorHighlighting`: unclosed quotes and unmatched brackets are
    /// errors, the brackets around the cursor are highlighted, and comments are marked;
    /// for the rendered bytes from `render_start` to `render_end`.
    fn add_error_highlighting(
        &self,
        render_start: usize,
        render_end: usize,
        tokens: &mut Vec<HighlightToken>,
    ) {
        const MAX_ERROR_LENGTH: usize = 2000;
        if !ERROR_RENDERING.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        let buf = &self.buf;
        let len = buf.len();
        if len >= MAX_ERROR_LENGTH {
            return;
        }
        let pos = self.pos;
        let mut state = ScanState::Standard;
        let mut brackets = Vec::new();
        let mut square_brackets = Vec::new();
        let mut curly_brackets = Vec::new();
        let mut errors = Vec::new();
        let mut cursor_brackets = Vec::new();
        let mut comment_start = Vec::new();
        let mut comment_end = Vec::new();
        let mut dollar_quote_marker: &[u8] = &[];
        let mut quote_pos = 0;
        let mut i = 0;
        while i < len {
            let c = buf[i];
            match state {
                ScanState::Standard => match c {
                    b'-' if i + 1 < len && buf[i + 1] == b'-' => {
                        // `--` starts a comment.
                        comment_start.push(i);
                        i += 1;
                        state = ScanState::InComment;
                    }
                    b'\'' => {
                        state = ScanState::InSingleQuote;
                        quote_pos = i;
                    }
                    b'"' => {
                        state = ScanState::InDoubleQuote;
                        quote_pos = i;
                    }
                    b'(' => open_bracket(&mut brackets, &mut cursor_brackets, pos, i),
                    b'[' => open_bracket(&mut square_brackets, &mut cursor_brackets, pos, i),
                    b'{' => open_bracket(&mut curly_brackets, &mut cursor_brackets, pos, i),
                    b')' => close_bracket(&mut brackets, &mut cursor_brackets, pos, i, &mut errors),
                    b']' => close_bracket(&mut square_brackets, &mut cursor_brackets, pos, i, &mut errors),
                    b'}' => close_bracket(&mut curly_brackets, &mut cursor_brackets, pos, i, &mut errors),
                    b'$' if i + 1 < len => {
                        // Is this a dollar-quoted string?
                        let mut next_dollar = 0;
                        for (idx, &d) in buf.iter().enumerate().take(len).skip(i + 1) {
                            if d == b'$' {
                                next_dollar = idx;
                                break;
                            }
                            if d.is_ascii_alphabetic()
                                || d == b'_'
                                || d >= 0x80
                                || (idx > i + 1 && d.is_ascii_digit())
                            {
                                continue;
                            }
                            break;
                        }
                        if next_dollar != 0 {
                            state = ScanState::DollarQuotedString;
                            quote_pos = i;
                            i = next_dollar;
                            dollar_quote_marker = &buf[quote_pos + 1..i];
                        }
                    }
                    _ => {}
                },
                ScanState::InComment => {
                    if c == b'\r' || c == b'\n' {
                        state = ScanState::Standard;
                        comment_end.push(i);
                    }
                }
                ScanState::InSingleQuote => {
                    if c == b'\'' {
                        if i + 1 < len && buf[i + 1] == b'\'' {
                            // An escaped quote.
                            i += 1;
                        } else {
                            state = ScanState::Standard;
                        }
                    }
                }
                ScanState::InDoubleQuote => {
                    if c == b'"' {
                        if i + 1 < len && buf[i + 1] == b'"' {
                            i += 1;
                        } else {
                            state = ScanState::Standard;
                        }
                    }
                }
                ScanState::DollarQuotedString => {
                    // Only `$marker$` ends it.
                    if c == b'$' && i + 1 < len {
                        let start = i + 1;
                        let mut end = start;
                        while end < len && buf[end] != b'$' {
                            end += 1;
                        }
                        if end < len && &buf[start..end] == dollar_quote_marker {
                            dollar_quote_marker = &[];
                            state = ScanState::Standard;
                            i = end;
                        }
                    }
                }
            }
            i += 1;
        }
        if matches!(
            state,
            ScanState::InDoubleQuote | ScanState::InSingleQuote | ScanState::DollarQuotedString
        ) {
            // An unclosed quote.
            errors.push(quote_pos);
        }
        // Unclosed brackets.
        errors.extend_from_slice(&brackets);
        errors.extend_from_slice(&square_brackets);
        errors.extend_from_slice(&curly_brackets);
        for &error in &errors {
            if error < render_start || error > render_end {
                continue;
            }
            insert_token(TokenType::Error, error - render_start, tokens);
        }
        if cursor_brackets.len() != 2 {
            // No matching pair at the cursor.
            cursor_brackets.clear();
        }
        for &bracket_position in &cursor_brackets {
            if bracket_position < render_start || bracket_position > render_end {
                continue;
            }
            insert_token(TokenType::Bracket, bracket_position - render_start, tokens);
        }
        if comment_start.is_empty() {
            return;
        }
        // Comments replace the tokens inside them.
        let mut new_tokens = Vec::with_capacity(tokens.len());
        let mut token_idx = 0;
        for (c, &c_start) in comment_start.iter().enumerate() {
            let c_end = comment_end.get(c).copied().unwrap_or(len);
            if c_start < render_start || c_end > render_end {
                continue;
            }
            let c_start = c_start - render_start;
            let c_end = c_end - render_start;
            let comment_token = HighlightToken::new(TokenType::Comment, c_start);
            let mut inserted_comment = false;
            while token_idx < tokens.len() {
                if tokens[token_idx].start >= c_start {
                    new_tokens.push(comment_token);
                    inserted_comment = true;
                    break;
                }
                new_tokens.push(tokens[token_idx]);
                token_idx += 1;
            }
            if inserted_comment {
                // Skip the tokens inside the comment.
                while token_idx < tokens.len() && tokens[token_idx].start <= c_end {
                    token_idx += 1;
                }
            } else {
                new_tokens.push(comment_token);
            }
        }
        new_tokens.extend_from_slice(&tokens[token_idx..]);
        *tokens = new_tokens;
    }

    /// `Linenoise::AddCompletionMarker`: after typing at least three letters at the end,
    /// the rest of the first completion as an underlined comment (with
    /// `.render_completion on`). DuckDB indexes the rendered bytes by the cursor position.
    fn add_completion_marker(&mut self, buf: &[u8], tokens: &mut Vec<HighlightToken>) -> Option<Vec<u8>> {
        const MAX_COMPLETION_LENGTH: usize = 1000;
        if !COMPLETION_RENDERING.load(std::sync::atomic::Ordering::Relaxed) || !self.continuation_markers {
            return None;
        }
        let len = buf.len();
        if len >= MAX_COMPLETION_LENGTH || !self.insert || self.pos != len || self.pos < 3 {
            return None;
        }
        if tokens
            .last()
            .is_some_and(|token| token.token_type == TokenType::Error)
        {
            return None;
        }
        let byte = |i: usize| buf.get(i).copied().unwrap_or(0);
        if !(self.pos - 3..self.pos).all(|i| is_completion_character(byte(i))) {
            return None;
        }
        let completion = self.tab_complete();
        let first = completion.completions.first()?;
        if first.completion.len() <= len {
            return None;
        }
        // The completion must match the letters typed.
        for cpos in (0..self.pos).rev() {
            if !is_completion_character(byte(cpos)) {
                break;
            }
            if first.completion.get(cpos).map(u8::to_ascii_lowercase) != Some(byte(cpos).to_ascii_lowercase())
            {
                return None;
            }
        }
        let mut result_buffer = buf.to_vec();
        result_buffer.extend_from_slice(&first.completion[len..]);
        let mut completion_token = HighlightToken::new(TokenType::Comment, len);
        completion_token.extra_highlighting = ExtraHighlightingType::Underline;
        tokens.push(completion_token);
        Some(result_buffer)
    }

    /// `Linenoise::RefreshMultiLine`: redraws the rows of the line (scrolled to the cursor
    /// if they do not fit), with highlighting, continuation markers and the completion
    /// list, and puts the cursor back.
    #[allow(clippy::cognitive_complexity)]
    fn refresh_multi_line(&mut self) {
        if !self.render {
            return;
        }
        let plen = self.prompt_width() as i32;
        let mut truncation = RenderTruncation::NoTruncate;
        let (mut new_cursor_row, mut new_cursor_x, mut rows, _) = self.position_to_col_and_row(self.pos);
        let mut old_rows = if self.maxrows == 0 { 1 } else { self.maxrows };
        let mut render_start = 0;
        let mut render_end = self.len();
        if self.clear_screen {
            self.old_cursor_rows = 0;
            old_rows = 0;
            self.clear_screen = false;
        }
        let mut max_rows_to_render = self.ws.ws_row as usize;
        if self.render_completion_suggestion && !self.completion_list.completions.is_empty() {
            // Keep a line for the completions.
            max_rows_to_render -= 1;
        }
        if rows as usize > max_rows_to_render {
            if max_rows_to_render == self.ws.ws_row as usize && self.rendered_completion_lines > 0 {
                self.y_scroll = self.y_scroll.wrapping_sub(1);
            }
            // Too many rows: scroll so that the cursor shows.
            if new_cursor_row < self.y_scroll as i32 + 1 {
                self.y_scroll = (new_cursor_row - 1) as usize;
            } else if new_cursor_row > self.y_scroll as i32 + max_rows_to_render as i32 {
                self.y_scroll = (new_cursor_row - max_rows_to_render as i32) as usize;
            }
            let mut truncate_top = false;
            let mut truncate_bottom = false;
            if self.y_scroll == 0 {
                render_start = 0;
            } else {
                render_start = self.col_and_row_to_position(self.y_scroll as i32, 0);
                truncate_top = true;
            }
            if self.y_scroll as i32 + max_rows_to_render as i32 >= rows {
                render_end = self.len();
            } else {
                render_end =
                    self.col_and_row_to_position(self.y_scroll as i32 + max_rows_to_render as i32, 99999);
                truncate_bottom = true;
            }
            truncation = match (truncate_top, truncate_bottom) {
                (true, true) => RenderTruncation::TruncateBoth,
                (true, false) => RenderTruncation::TruncateTop,
                (false, true) => RenderTruncation::TruncateBottom,
                (false, false) => RenderTruncation::NoTruncate,
            };
            new_cursor_row -= self.y_scroll as i32;
            rows = max_rows_to_render as i32;
        } else {
            self.y_scroll = 0;
        }
        let mut render_buf: Vec<u8> = self.buf[render_start..render_end.max(render_start)].to_vec();
        if rows > self.maxrows as i32 {
            self.maxrows = rows as usize;
        }
        let mut tokens = Vec::new();
        if highlighting::is_enabled() {
            tokens = highlighting::tokenize(&render_buf, self.buf.first() == Some(&b'.'));
            self.add_error_highlighting(render_start, render_end, &mut tokens);
            // A search match is underlined, the extra character of a completion bold.
            let mut extra_highlighting = ExtraHighlighting::default();
            if let Some(found) = self.search_matches.get(self.search_index) {
                extra_highlighting = ExtraHighlighting {
                    start: found.match_start,
                    end: found.match_end,
                    kind: ExtraHighlightingType::Underline,
                };
            } else if let Some(index) = self.completion_idx {
                let completion = &self.completion_list.completions[index];
                if completion.extra_char != 0 {
                    extra_highlighting = ExtraHighlighting {
                        start: completion.extra_char_pos,
                        end: completion.extra_char_pos + 1,
                        kind: ExtraHighlightingType::Bold,
                    };
                }
            }
            highlighting::add_extra_highlighting(render_buf.len(), &mut tokens, extra_highlighting);
            if let Some(with_hint) = self.add_completion_marker(&render_buf, &mut tokens) {
                render_buf = with_hint;
            }
        }
        if rows > 1 {
            let cursor_row = if self.y_scroll > 0 {
                new_cursor_row + 1
            } else {
                new_cursor_row
            };
            let rows_to_render = if self.y_scroll > 0 { rows } else { rows - 1 } as usize;
            render_buf = self.add_continuation_markers(
                &render_buf,
                plen,
                cursor_row,
                rows_to_render,
                &mut tokens,
                truncation,
            );
        }
        if std::str::from_utf8(&render_buf).is_ok() && highlighting::is_enabled() {
            render_buf = highlighting::highlight_text(&render_buf, 0, render_buf.len(), &tokens);
        }

        let mut append_buffer: Vec<u8> = Vec::new();
        // Clear the rows used before: go to the last one, then clear and go up.
        if old_rows > self.old_cursor_rows {
            let extra_rows = old_rows - self.old_cursor_rows;
            append_buffer.extend_from_slice(format!("\x1b[{extra_rows}B").as_bytes());
        }
        for _ in 1..old_rows {
            append_buffer.extend_from_slice(b"\r\x1b[0K\x1b[1A");
        }
        append_buffer.extend_from_slice(b"\r\x1b[0K");
        if self.y_scroll == 0 {
            append_buffer.extend_from_slice(&self.prompt);
        }
        append_buffer.extend_from_slice(&render_buf);
        // With the cursor at the end of the last column, a new row starts.
        if self.pos > 0 && self.pos == self.len() && new_cursor_x == self.ws.ws_col {
            append_buffer.extend_from_slice(b"\n\r");
            rows += 1;
            new_cursor_row += 1;
            new_cursor_x = 0;
            if rows > self.maxrows as i32 {
                self.maxrows = rows as usize;
            }
        }
        if self.render_completion_suggestion && !self.completion_list.completions.is_empty() {
            append_buffer.extend_from_slice(b"\r\n");
            let completion_text = self.render_completion_list(rows);
            let (_, _, completion_rows, _) = self.position_to_col_and_row_in(0, &completion_text, 0);
            append_buffer.extend_from_slice(&completion_text);
            self.rendered_completion_lines = completion_rows as usize;
            rows += completion_rows;
            if rows > self.maxrows as i32 {
                self.maxrows = rows as usize;
            }
        } else {
            self.rendered_completion_lines = 0;
        }
        // Go up to the cursor's row, and to its column.
        if rows - new_cursor_row > 0 {
            append_buffer.extend_from_slice(format!("\x1b[{}A", rows - new_cursor_row).as_bytes());
        }
        if new_cursor_x != 0 {
            append_buffer.extend_from_slice(format!("\r\x1b[{new_cursor_x}C").as_bytes());
        } else {
            append_buffer.push(b'\r');
        }
        self.old_cursor_rows = new_cursor_row as usize;
        Self::write(self.ofd, &append_buffer);
    }

    /// The completions below the line, in aligned columns and their highlighting, the
    /// selected one underlined, within the rows the terminal has left.
    fn render_completion_list(&mut self, rows: i32) -> Vec<u8> {
        let ws = self.ws;
        let max_split_length = (ws.ws_col as usize / 2).wrapping_sub(2);
        let mut max_length = 0;
        for completion in &mut self.completion_list.completions {
            let length = *completion
                .original_completion_length
                .get_or_insert_with(|| compute_render_width(&completion.original_completion));
            // Oversized values are laid out apart.
            if length <= max_split_length && length > max_length {
                max_length = length;
            }
        }
        // At least one space between entries.
        max_length += 1;
        let column_count = ws.ws_col as usize / max_length;
        let mut column_index = 0;
        let mut rendered_rows: usize = 1;
        let mut completion_text = Vec::new();
        let count = self.completion_list.completions.len();
        for i in 0..count {
            let completion = &self.completion_list.completions[i];
            let mut rendered_text = completion.original_completion.clone();
            let element_type = match completion.completion_type {
                CompletionType::Keyword | CompletionType::TypeName => HighlightElementType::Keyword,
                CompletionType::CatalogName => HighlightElementType::SuggestionCatalogName,
                CompletionType::SchemaName => HighlightElementType::SuggestionSchemaName,
                CompletionType::TableName => HighlightElementType::SuggestionTableName,
                CompletionType::ColumnName => HighlightElementType::SuggestionColumnName,
                CompletionType::FileName => HighlightElementType::SuggestionFileName,
                CompletionType::DirectoryName => HighlightElementType::SuggestionDirectoryName,
                CompletionType::ScalarFunction | CompletionType::TableFunction => {
                    HighlightElementType::SuggestionFunctionName
                }
                CompletionType::SettingName => HighlightElementType::SuggestionSettingName,
                CompletionType::Unknown => HighlightElementType::None,
            };
            let element = highlight::highlight_element(element_type);
            let mut intensity = element.intensity;
            if self.completion_idx == Some(i) {
                intensity = if intensity == PrintIntensity::Bold {
                    PrintIntensity::BoldUnderline
                } else {
                    PrintIntensity::Underline
                };
            }
            let completion_length = completion.original_completion_length.unwrap_or(0);
            let is_oversized_value = completion_length > max_split_length;
            if is_oversized_value {
                // An oversized value starts a line.
                if column_index > 0 {
                    if rendered_rows + 1 + rows as usize > ws.ws_row as usize {
                        break;
                    }
                    completion_text.extend_from_slice(b"\r\n");
                    column_index = 0;
                    rendered_rows += 1;
                }
                let total_lines = completion_length / ws.ws_row as usize;
                if rendered_rows + total_lines + rows as usize > ws.ws_row as usize {
                    // No room: a single line of it.
                    let max_render_pos = self.col_and_row_to_position_in(0, &rendered_text, 0, ws.ws_col);
                    rendered_text.truncate(max_render_pos);
                } else {
                    rendered_rows += total_lines;
                }
            }
            let terminal_text = highlight::terminal_code(element.color, intensity);
            completion_text.extend_from_slice(terminal_text.as_bytes());
            completion_text.extend_from_slice(&rendered_text);
            if !terminal_text.is_empty() {
                completion_text.extend_from_slice(RESET_TERMINAL_CODE.as_bytes());
            }
            if i + 1 == count {
                continue;
            }
            column_index += 1;
            if column_index >= column_count || is_oversized_value {
                if rendered_rows + 1 + rows as usize > ws.ws_row as usize {
                    break;
                }
                completion_text.extend_from_slice(b"\r\n");
                column_index = 0;
                rendered_rows += 1;
            } else {
                completion_text.extend(std::iter::repeat_n(b' ', max_length - completion_length));
            }
        }
        completion_text
    }

    /// `Linenoise::RefreshLine`.
    pub(super) fn refresh_line(&mut self) {
        if terminal::is_multiline() {
            self.refresh_multi_line();
        } else {
            self.refresh_single_line();
        }
    }
}
