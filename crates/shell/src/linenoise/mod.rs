//! The line editor of the interactive shell: DuckDB's port of linenoise
//! (`tools/shell/linenoise`): editing keys, history and its search, tab completion,
//! multi-line editing with continuation markers, syntax and error highlighting, and
//! `.edit` with an external editor.

mod highlighting;
pub mod history;
mod rendering;
pub mod terminal;

use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};

use terminal::{EscapeSequence, TerminalSize, key};

use crate::utf8;

/// `LINENOISE_MAX_LINE`: the longest line the editor takes.
pub const LINENOISE_MAX_LINE: usize = 204_800;
/// `LINENOISE_MAX_HISTORY`: the most bytes of a history file that are read.
pub const LINENOISE_MAX_HISTORY: usize = 104_857_600;

/// A completion the shell offers for the text before the cursor (the arguments of
/// `linenoiseAddCompletion`).
#[derive(Clone, Debug)]
pub struct CompletionCandidate {
    pub completion: Vec<u8>,
    /// Where the completion replaces the text.
    pub start: usize,
    pub completion_type: String,
    pub score: u64,
    /// A character the completion ends in (such as a closing quote), or 0.
    pub extra_char: u8,
}

/// The completions of the text before the cursor (`linenoiseCompletionCallback`).
pub type CompletionCallback<'a> = dyn FnMut(&[u8]) -> Vec<CompletionCandidate> + 'a;

/// What a completion is (`CompletionType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompletionType {
    Unknown,
    Keyword,
    CatalogName,
    SchemaName,
    TableName,
    TypeName,
    ColumnName,
    FileName,
    DirectoryName,
    ScalarFunction,
    TableFunction,
    SettingName,
}

/// `Linenoise::GetCompletionType`.
fn completion_type(name: &str) -> CompletionType {
    match name {
        "keyword" => CompletionType::Keyword,
        "catalog" => CompletionType::CatalogName,
        "schema" => CompletionType::SchemaName,
        "table" => CompletionType::TableName,
        "column" => CompletionType::ColumnName,
        "type" => CompletionType::TypeName,
        "file_name" => CompletionType::FileName,
        "directory" => CompletionType::DirectoryName,
        "scalar_function" => CompletionType::ScalarFunction,
        "table_function" => CompletionType::TableFunction,
        "setting" => CompletionType::SettingName,
        _ => CompletionType::Unknown,
    }
}

/// A completion of the line (`Completion`).
#[derive(Clone, Debug)]
#[allow(clippy::struct_field_names, reason = "DuckDB's field names")]
pub struct Completion {
    /// The whole line with the completion applied.
    completion: Vec<u8>,
    /// The completed word, as listed.
    original_completion: Vec<u8>,
    original_completion_length: Option<usize>,
    cursor_pos: usize,
    completion_type: CompletionType,
    score: u64,
    extra_char: u8,
    extra_char_pos: usize,
}

/// `TabCompletion`.
#[derive(Clone, Debug, Default)]
pub struct TabCompletion {
    completions: Vec<Completion>,
}

/// `linenoiseAddCompletion`: the line `line` with the candidate applied.
fn add_completion(completions: &mut TabCompletion, line: &[u8], candidate: CompletionCandidate) {
    let mut completion = line[..candidate.start.min(line.len())].to_vec();
    completion.extend_from_slice(&candidate.completion);
    completions.completions.push(Completion {
        cursor_pos: completion.len(),
        completion,
        original_completion: candidate.completion,
        original_completion_length: None,
        completion_type: completion_type(&candidate.completion_type),
        score: candidate.score,
        extra_char: candidate.extra_char,
        extra_char_pos: 0,
    });
}

/// A key press (`KeyPress`): a byte, the key of an escape sequence, and the position of
/// a mouse click.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyPress {
    pub action: u8,
    pub sequence: EscapeSequence,
    pub position: TerminalSize,
}

impl KeyPress {
    pub fn action(action: u8) -> Self {
        Self {
            action,
            sequence: EscapeSequence::Invalid,
            position: TerminalSize::default(),
        }
    }
}

/// Key presses read ahead and not consumed yet (`BufferedKeyPresses`).
#[derive(Debug)]
pub struct BufferedKeyPresses;

static BUFFERED_KEY_PRESSES: Mutex<VecDeque<KeyPress>> = Mutex::new(VecDeque::new());

impl BufferedKeyPresses {
    pub fn buffer_key_press(key_press: KeyPress) {
        BUFFERED_KEY_PRESSES
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push_back(key_press);
    }

    fn try_get_key_press() -> Option<KeyPress> {
        BUFFERED_KEY_PRESSES
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop_front()
    }

    fn has_more_data() -> bool {
        !BUFFERED_KEY_PRESSES
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_empty()
    }
}

/// `HistoryScrollDirection`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HistoryScrollDirection {
    Next,
    Prev,
    Start,
    End,
}

/// `Capitalization`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Capitalization {
    Capitalize,
    Lowercase,
    Uppercase,
}

/// A history entry matching the search (`searchMatch`).
#[derive(Clone, Copy, Debug)]
struct SearchMatch {
    history_index: usize,
    match_start: usize,
    match_end: usize,
}

/// `enableCompletionRendering`.
static COMPLETION_RENDERING: AtomicBool = AtomicBool::new(false);
/// `enableErrorRendering`.
static ERROR_RENDERING: AtomicBool = AtomicBool::new(true);

/// The prompts of continuation lines (`continuationPrompt`, `continuationSelectedPrompt`,
/// `scrollUpPrompt`, `scrollDownPrompt`).
#[derive(Debug)]
struct ContinuationPrompts {
    continuation: String,
    continuation_selected: String,
    scroll_up: String,
    scroll_down: String,
}

static PROMPTS: Mutex<Option<ContinuationPrompts>> = Mutex::new(None);

fn with_prompts<T>(f: impl FnOnce(&ContinuationPrompts) -> T) -> T {
    let prompts = PROMPTS.lock().unwrap_or_else(PoisonError::into_inner);
    let default = ContinuationPrompts {
        continuation: "  ".to_owned(),
        continuation_selected: "  ".to_owned(),
        scroll_up: "^ ".to_owned(),
        scroll_down: "v ".to_owned(),
    };
    f(prompts.as_ref().unwrap_or(&default))
}

/// `linenoiseSetPrompt`: the prompts of continuation lines.
pub fn set_prompt(continuation: &str, continuation_selected: &str, scroll_up: &str, scroll_down: &str) {
    *PROMPTS.lock().unwrap_or_else(PoisonError::into_inner) = Some(ContinuationPrompts {
        continuation: continuation.to_owned(),
        continuation_selected: continuation_selected.to_owned(),
        scroll_up: scroll_up.to_owned(),
        scroll_down: scroll_down.to_owned(),
    });
}

/// `linenoiseSetMultiLine`.
pub fn set_multi_line(multiline: bool) {
    terminal::set_multiline(multiline);
}

/// `linenoiseSetErrorRendering`.
pub fn set_error_rendering(enabled: bool) {
    ERROR_RENDERING.store(enabled, Ordering::Relaxed);
}

/// `linenoiseSetCompletionRendering`.
pub fn set_completion_rendering(enabled: bool) {
    COMPLETION_RENDERING.store(enabled, Ordering::Relaxed);
}

/// `linenoiseHistoryAdd`.
pub fn history_add(line: impl AsRef<[u8]>) {
    history::add(line.as_ref());
}

/// `linenoiseHistorySetMaxLen`.
pub fn history_set_max_len(len: usize) {
    history::set_max_length(len);
}

/// `linenoiseHistorySave`.
pub fn history_save(filename: &str) {
    history::save(filename);
}

/// `linenoiseHistoryLoad`.
pub fn history_load(filename: &str) {
    history::load(filename);
}

/// The background of the terminal (`linenoiseTerminalColorMode`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalColorMode {
    Unknown,
    Dark,
    Light,
    Mixed,
}

/// `linenoiseGetTerminalColorMode`: dark or light by the brightness of the background.
pub fn terminal_color_mode() -> TerminalColorMode {
    // The terminal's answer comes after any input already typed.
    terminal::buffer_available_input();
    let Some(color) = terminal::try_get_background_color() else {
        return TerminalColorMode::Unknown;
    };
    let brightness = 0.2126 * f64::from(color.r) + 0.7152 * f64::from(color.g) + 0.0722 * f64::from(color.b);
    if brightness <= 96.0 {
        TerminalColorMode::Dark
    } else if brightness >= 160.0 {
        TerminalColorMode::Light
    } else {
        TerminalColorMode::Mixed
    }
}

/// `linenoise`: a line typed at the terminal (read plainly if stdin is not one), `None`
/// at the end of input. Ctrl+C returns `\x03` (`\x03\x03` if the line had text).
pub fn linenoise<'a>(prompt: &str, completion: Option<&'a mut CompletionCallback<'a>>) -> Option<Vec<u8>> {
    if !terminal::is_atty() {
        return terminal::edit_no_tty();
    }
    if terminal::is_unsupported_term() {
        crate::printer::write_stdout(prompt.as_bytes());
        crate::printer::flush_stdout();
        let mut line = Vec::new();
        {
            use std::io::BufRead;
            let read = std::io::stdin().lock().read_until(b'\n', &mut line).ok()?;
            if read == 0 {
                return None;
            }
        }
        line.truncate(LINENOISE_MAX_LINE - 1);
        while line.last().is_some_and(|&c| c == b'\n' || c == b'\r') {
            line.pop();
        }
        return Some(line);
    }
    terminal::edit_raw(prompt.as_bytes(), completion)
}

/// A keyboard shortcut of `.help shortcuts` (`ShortcutEntry`).
#[derive(Clone, Copy, Debug)]
pub struct ShortcutEntry {
    pub key_name: &'static str,
    pub description: &'static str,
    pub category: &'static str,
}

/// `GetShellShortcuts`; kept in sync with [`Linenoise::edit`].
pub fn shell_shortcuts() -> Vec<ShortcutEntry> {
    const SHORTCUTS: [(&str, &str, &str); 35] = [
        ("Enter / Ctrl+J", "Submit input", "Control"),
        (
            "Ctrl+C",
            "Cancel current input or interrupt in-flight query",
            "Control",
        ),
        ("Ctrl+D", "Exit shell (when line is empty)", "Control"),
        ("Ctrl+G", "Submit input (in multiline mode)", "Control"),
        ("Ctrl+L", "Clear screen", "Control"),
        ("Ctrl+Z", "Suspend shell", "Control"),
        ("Tab", "Auto-complete", "Control"),
        (
            "Ctrl+Q, then click",
            "Move cursor to mouse click position",
            "Control",
        ),
        (
            "Ctrl+D / Delete",
            "Delete character under cursor (or exit if line is empty)",
            "Editing",
        ),
        ("Ctrl+H / Backspace", "Delete character before cursor", "Editing"),
        ("Ctrl+K", "Delete from cursor to end of line", "Editing"),
        ("Ctrl+U", "Delete entire line", "Editing"),
        ("Ctrl+W", "Delete previous word", "Editing"),
        ("Alt+D", "Delete next word", "Editing"),
        ("Alt+Backspace", "Delete previous word", "Editing"),
        ("Ctrl+T", "Swap character under cursor with previous", "Editing"),
        ("Alt+T", "Swap current word with previous", "Editing"),
        ("Alt+C", "Capitalize next word", "Editing"),
        ("Alt+L", "Lowercase next word", "Editing"),
        ("Alt+U", "Uppercase next word", "Editing"),
        ("Alt+R", "Delete entire line", "Editing"),
        ("Alt+\\", "Remove spaces around cursor", "Editing"),
        ("Ctrl+X", "Insert newline (multiline input)", "Editing"),
        ("Ctrl+A / Home", "Go to beginning of line", "Navigation"),
        ("Ctrl+E / End", "Go to end of line", "Navigation"),
        ("Ctrl+B / Left", "Move cursor left", "Navigation"),
        ("Ctrl+F / Right", "Move cursor right", "Navigation"),
        ("Alt+B / Alt+Left", "Move cursor one word left", "Navigation"),
        ("Alt+F / Alt+Right", "Move cursor one word right", "Navigation"),
        ("Ctrl+P / Up", "Previous history entry", "History"),
        ("Ctrl+N / Down", "Next history entry", "History"),
        ("Ctrl+R", "Reverse search history", "History"),
        ("Ctrl+S", "Forward search history", "History"),
        ("Ctrl+Up", "Jump to first history entry", "History"),
        ("Ctrl+Down", "Jump to last history entry", "History"),
    ];
    SHORTCUTS
        .iter()
        .map(|&(key_name, description, category)| ShortcutEntry {
            key_name,
            description,
            category,
        })
        .collect()
}

/// `Linenoise::HandleANSIEscape`: skips the ANSI escape sequence at `cpos`, if any.
pub fn handle_ansi_escape(buf: &[u8], cpos: &mut usize) -> bool {
    utf8::skip_ansi_escape(buf, cpos)
}

/// `linenoiseComputeRenderWidth` / `Linenoise::ComputeRenderWidth`.
pub fn compute_render_width(buf: &[u8]) -> usize {
    utf8::terminal_render_width(buf)
}

/// The state of the line being edited (`Linenoise`).
#[allow(clippy::struct_excessive_bools, reason = "DuckDB's flags")]
pub struct Linenoise<'a> {
    ifd: i32,
    ofd: i32,
    /// The line; its length is `len`.
    buf: Vec<u8>,
    /// The most bytes the line can have.
    buflen: usize,
    prompt: Vec<u8>,
    pos: usize,
    old_cursor_rows: usize,
    y_scroll: usize,
    ws: TerminalSize,
    maxrows: usize,
    history_index: usize,
    clear_screen: bool,
    continuation_markers: bool,
    search: bool,
    render: bool,
    /// Whether more input is waiting (pasted text).
    has_more_data: bool,
    /// Whether the last action inserted a character.
    insert: bool,
    search_buf: Vec<u8>,
    search_matches: Vec<SearchMatch>,
    search_index: usize,
    completion_list: TabCompletion,
    completion_idx: Option<usize>,
    rendered_completion_lines: usize,
    render_completion_suggestion: bool,
    completion_callback: Option<&'a mut CompletionCallback<'a>>,
}

impl std::fmt::Debug for Linenoise<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Linenoise")
            .field("buf", &String::from_utf8_lossy(&self.buf))
            .field("pos", &self.pos)
            .finish_non_exhaustive()
    }
}

impl<'a> Linenoise<'a> {
    fn new(
        ifd: i32,
        ofd: i32,
        buflen: usize,
        prompt: &[u8],
        completion_callback: Option<&'a mut CompletionCallback<'a>>,
    ) -> Self {
        Self {
            ifd,
            ofd,
            buf: Vec::new(),
            // Room for the terminating NUL.
            buflen: buflen - 1,
            prompt: prompt.to_vec(),
            pos: 0,
            old_cursor_rows: 1,
            y_scroll: 0,
            ws: terminal::get_terminal_size(),
            maxrows: 0,
            history_index: 0,
            clear_screen: false,
            continuation_markers: true,
            search: false,
            render: true,
            has_more_data: false,
            insert: false,
            search_buf: Vec::new(),
            search_matches: Vec::new(),
            search_index: 0,
            completion_list: TabCompletion::default(),
            completion_idx: None,
            rendered_completion_lines: 0,
            render_completion_suggestion: false,
            completion_callback,
        }
    }

    /// The edited line.
    fn into_buffer(self) -> Vec<u8> {
        self.buf
    }

    fn len(&self) -> usize {
        self.buf.len()
    }

    /// The byte at `index`, 0 past the end (the C buffer's terminating NUL).
    fn byte(&self, index: usize) -> u8 {
        self.buf.get(index).copied().unwrap_or(0)
    }

    /// `Linenoise::TabComplete`: the completions at the cursor, with the text after the
    /// cursor kept.
    fn tab_complete(&mut self) -> TabCompletion {
        let mut result = TabCompletion::default();
        let Some(callback) = self.completion_callback.as_mut() else {
            return result;
        };
        let line = self.buf[..self.pos].to_vec();
        for candidate in callback(&line) {
            add_completion(&mut result, &line, candidate);
        }
        for completion in &mut result.completions {
            let mut copy_offset = self.pos;
            if completion.extra_char != 0 {
                if self.byte(self.pos) == completion.extra_char {
                    // The extra character is there already: do not add it again.
                    copy_offset += 1;
                }
                completion.extra_char_pos = completion.completion.len() - 1;
            }
            completion
                .completion
                .extend_from_slice(self.buf.get(copy_offset..).unwrap_or_default());
        }
        result
    }

    /// `Linenoise::CompleteLine`: cycles through the completions on Tab; returns `false`
    /// when input ends. `next_key` is the key to handle next.
    fn complete_line(&mut self, next_key: &mut KeyPress) -> bool {
        next_key.action = key::KEY_NULL;
        self.completion_list = self.tab_complete();
        // Suggestions show once we tab through them.
        self.render_completion_suggestion = false;
        if self.completion_list.completions.is_empty() {
            terminal::beep();
        } else {
            let mut stop = false;
            let mut accept_completion = false;
            // With ties, the list shows instead of completing at once.
            let completions = &self.completion_list.completions;
            let has_ties = completions.len() > 1 && completions[0].score == completions[1].score;
            if has_ties {
                self.completion_idx = None;
                self.render_completion_suggestion = true;
            } else {
                self.completion_idx = Some(0);
            }
            let mut action_count = 0;
            while !stop {
                action_count += 1;
                self.handle_terminal_resize();
                // Show the completion or the original line.
                if let Some(index) = self.completion_idx {
                    let completion = &self.completion_list.completions[index];
                    let shown = completion.completion.clone();
                    let saved_pos = std::mem::replace(&mut self.pos, completion.cursor_pos);
                    let saved_buf = std::mem::replace(&mut self.buf, shown);
                    self.refresh_line();
                    self.buf = saved_buf;
                    self.pos = saved_pos;
                } else {
                    self.refresh_line();
                }
                let Some(key_press) = self.try_get_key_press() else {
                    self.completion_list.completions.clear();
                    return false;
                };
                match key_press.action {
                    key::TAB => {
                        if action_count == 1 && !has_ties {
                            // A clear winner was completed: complete again from there.
                            *next_key = key_press;
                            accept_completion = true;
                            stop = true;
                        } else {
                            let count = self.completion_list.completions.len();
                            self.completion_idx =
                                Some(self.completion_idx.map_or(0, |index| (index + 1) % count));
                            self.render_completion_suggestion = true;
                        }
                    }
                    key::CTRL_C => {
                        next_key.action = key::KEY_NULL;
                        accept_completion = false;
                        stop = true;
                    }
                    key::ESC => match key_press.sequence {
                        EscapeSequence::ShiftTab => {
                            match self.completion_idx {
                                None => {
                                    // Shift+Tab without a selected completion stops.
                                    self.refresh_line();
                                    next_key.action = key::KEY_NULL;
                                    stop = true;
                                }
                                Some(0) => self.completion_idx = None,
                                Some(index) => self.completion_idx = Some(index - 1),
                            }
                            self.render_completion_suggestion = true;
                        }
                        EscapeSequence::Escape => {
                            self.refresh_line();
                            *next_key = key_press;
                            stop = true;
                        }
                        _ => {
                            *next_key = key_press;
                            accept_completion = true;
                            stop = true;
                        }
                    },
                    _ => {
                        *next_key = key_press;
                        accept_completion = true;
                        stop = true;
                    }
                }
            }
            if accept_completion && let Some(index) = self.completion_idx {
                let accepted = &self.completion_list.completions[index];
                if accepted.extra_char != 0 && next_key.action == accepted.extra_char {
                    next_key.action = key::KEY_NULL;
                }
                let mut completion = accepted.completion.clone();
                completion.truncate(self.buflen.saturating_sub(1));
                self.pos = accepted.cursor_pos;
                self.buf = completion;
            }
        }
        self.completion_list.completions.clear();
        self.completion_idx = None;
        true
    }

    fn prompt_width(&self) -> usize {
        compute_render_width(&self.prompt)
    }

    /// `Linenoise::IsNewline`.
    fn is_newline(c: u8) -> bool {
        c == b'\r' || c == b'\n'
    }

    /// `Linenoise::NextPosition`: advances past the character (or newline, or escape
    /// sequence) at `cpos`, tracking the row and column it renders at.
    fn next_position(&self, buf: &[u8], cpos: &mut usize, rows: &mut i32, cols: &mut i32, plen: i32) {
        if Self::is_newline(buf[*cpos]) {
            // A newline: the next line starts after the prompt's width.
            *rows += 1;
            *cols = plen;
            *cpos += 1;
            if buf[*cpos - 1] == b'\r' && *cpos < buf.len() && buf[*cpos] == b'\n' {
                *cpos += 1;
            }
            return;
        }
        if handle_ansi_escape(buf, cpos) {
            return;
        }
        let char_render_width = if utf8::codepoint_at(buf, *cpos).is_none() {
            *cpos += 1;
            1
        } else {
            let width = utf8::byte_render_width(buf, *cpos) as i32;
            *cpos = utf8::next_grapheme_cluster_bytes(buf, *cpos);
            width
        };
        if *cols + char_render_width > self.ws.ws_col {
            // Past the last column: the next row.
            *rows += 1;
            *cols = char_render_width;
        }
        *cols += char_render_width;
    }

    /// `Linenoise::PositionToColAndRow`: the row and column of byte `target_pos` of
    /// `render_buf`, and the rows and columns the text takes.
    fn position_to_col_and_row_in(
        &self,
        prompt_len: i32,
        render_buf: &[u8],
        target_pos: usize,
    ) -> (i32, i32, i32, i32) {
        let mut out_row = -1;
        let mut out_col = 0;
        let mut rows = 1;
        let mut cols = prompt_len;
        let mut cpos = 0;
        while cpos < render_buf.len() {
            if cols >= self.ws.ws_col && !Self::is_newline(render_buf[cpos]) {
                rows += 1;
                cols = 0;
            }
            if out_row < 0 && cpos >= target_pos {
                out_row = rows;
                out_col = cols;
            }
            self.next_position(render_buf, &mut cpos, &mut rows, &mut cols, prompt_len);
        }
        if target_pos == render_buf.len() {
            out_row = rows;
            out_col = cols;
        }
        (out_row, out_col, rows, cols)
    }

    /// `PositionToColAndRow` of the line: `(cursor_row, cursor_col, rows, cols)`.
    fn position_to_col_and_row(&self, target_pos: usize) -> (i32, i32, i32, i32) {
        let plen = self.prompt_width() as i32;
        self.position_to_col_and_row_in(plen, &self.buf, target_pos)
    }

    /// `Linenoise::ColAndRowToPosition`: the byte of `buf` at a row and column, or the
    /// last one of the row if it is shorter.
    fn col_and_row_to_position_in(&self, plen: i32, buf: &[u8], target_row: i32, target_col: i32) -> usize {
        let mut rows = 1;
        let mut cols = plen;
        let mut last_cpos = 0;
        let mut cpos = 0;
        while cpos < buf.len() {
            if cols >= self.ws.ws_col {
                rows += 1;
                cols = 0;
            }
            if rows > target_row {
                return last_cpos;
            }
            if rows == target_row {
                last_cpos = cpos;
            }
            if rows == target_row && cols == target_col {
                return cpos;
            }
            self.next_position(buf, &mut cpos, &mut rows, &mut cols, plen);
        }
        cpos
    }

    fn col_and_row_to_position(&self, target_row: i32, target_col: i32) -> usize {
        let plen = self.prompt_width() as i32;
        self.col_and_row_to_position_in(plen, &self.buf, target_row, target_col)
    }

    /// `Linenoise::InsertCharacter`.
    fn insert_character(&mut self, c: u8) {
        if self.len() < self.buflen {
            self.buf.insert(self.pos, c);
            self.pos += 1;
        }
    }

    /// `Linenoise::EditInsert`: pasted characters are not rendered one by one.
    fn edit_insert(&mut self, c: u8) {
        if self.has_more_data {
            self.render = false;
        }
        self.insert = true;
        self.insert_character(c);
        self.refresh_line();
    }

    /// `Linenoise::EditInsertMulti`.
    fn edit_insert_multi(&mut self, text: &[u8]) {
        for &c in text {
            self.insert_character(c);
        }
        self.refresh_line();
    }

    fn prev_char(&self) -> usize {
        utf8::previous_grapheme_cluster_bytes(&self.buf, self.pos)
    }

    fn next_char(&self) -> usize {
        utf8::next_grapheme_cluster_bytes(&self.buf, self.pos)
    }

    fn edit_move_left(&mut self) {
        if self.pos > 0 {
            self.pos = self.prev_char();
            self.refresh_line();
        }
    }

    fn edit_move_right(&mut self) {
        if self.pos != self.len() {
            self.pos = self.next_char();
            self.refresh_line();
        }
    }

    /// `Linenoise::IsWordBoundary`: anything but an ASCII letter or digit.
    fn is_word_boundary(c: u8) -> bool {
        !c.is_ascii_alphanumeric()
    }

    fn edit_move_word_left(&mut self) {
        if self.pos == 0 {
            return;
        }
        loop {
            self.pos = self.prev_char();
            if self.pos == 0 || Self::is_word_boundary(self.byte(self.pos)) {
                break;
            }
        }
        self.refresh_line();
    }

    fn edit_move_word_right(&mut self) {
        if self.pos == self.len() {
            return;
        }
        loop {
            self.pos = self.next_char();
            if self.pos == self.len() || Self::is_word_boundary(self.byte(self.pos)) {
                break;
            }
        }
        self.refresh_line();
    }

    /// `Linenoise::EditMoveRowUp`: in multi-line mode, the cursor moves a row up.
    fn edit_move_row_up(&mut self) -> bool {
        if !terminal::is_multiline() {
            return false;
        }
        let (cursor_row, cursor_col, _, _) = self.position_to_col_and_row(self.pos);
        if cursor_row <= 1 {
            return false;
        }
        self.pos = self.col_and_row_to_position(cursor_row - 1, cursor_col);
        self.refresh_line();
        true
    }

    /// `Linenoise::EditMoveRowDown`.
    fn edit_move_row_down(&mut self) -> bool {
        if !terminal::is_multiline() {
            return false;
        }
        let (cursor_row, cursor_col, rows, _) = self.position_to_col_and_row(self.pos);
        if cursor_row >= rows {
            return false;
        }
        self.pos = self.col_and_row_to_position(cursor_row + 1, cursor_col);
        self.refresh_line();
        true
    }

    fn edit_move_home(&mut self) {
        if self.pos != 0 {
            self.pos = 0;
            self.refresh_line();
        }
    }

    fn edit_move_end(&mut self) {
        if self.pos != self.len() {
            self.pos = self.len();
            self.refresh_line();
        }
    }

    fn edit_move_start_of_line(&mut self) {
        while self.pos > 0 && self.byte(self.pos - 1) != b'\n' {
            self.pos -= 1;
        }
        self.refresh_line();
    }

    fn edit_move_end_of_line(&mut self) {
        while self.pos < self.len() && self.byte(self.pos + 1) != b'\n' {
            self.pos += 1;
        }
        self.refresh_line();
    }

    /// `Linenoise::EditHistoryNext`: the line becomes another history entry, the current
    /// one keeping the edits.
    fn edit_history_next(&mut self, direction: HistoryScrollDirection) {
        let history_len = history::len();
        if history_len <= 1 {
            return;
        }
        history::overwrite(history_len - 1 - self.history_index, &self.buf);
        match direction {
            HistoryScrollDirection::Prev => {
                self.history_index += 1;
                if self.history_index >= history_len {
                    self.history_index = history_len - 1;
                    return;
                }
            }
            HistoryScrollDirection::Next => {
                if self.history_index == 0 {
                    return;
                }
                self.history_index -= 1;
            }
            HistoryScrollDirection::End => self.history_index = 0,
            HistoryScrollDirection::Start => self.history_index = history_len - 1,
        }
        let mut entry = history::entry(history_len - 1 - self.history_index);
        entry.truncate(self.buflen.saturating_sub(1));
        if let Some(nul) = entry.iter().position(|&c| c == 0) {
            entry.truncate(nul);
        }
        self.buf = entry;
        self.pos = self.len();
        if terminal::is_multiline() && direction == HistoryScrollDirection::Next {
            self.pos = self.col_and_row_to_position(1, self.len() as i32);
        }
        self.refresh_line();
    }

    /// `Linenoise::EditDelete`: the character under the cursor.
    fn edit_delete(&mut self) {
        if !self.buf.is_empty() && self.pos < self.len() {
            let new_pos = self.next_char();
            self.buf.drain(self.pos..new_pos);
            self.refresh_line();
        }
    }

    /// `Linenoise::EditBackspace`.
    fn edit_backspace(&mut self) {
        if self.pos > 0 && !self.buf.is_empty() {
            let new_pos = self.prev_char();
            self.buf.drain(new_pos..self.pos);
            self.pos = new_pos;
            self.refresh_line();
        }
    }

    /// `Linenoise::IsSpace`.
    pub fn is_space(c: u8) -> bool {
        matches!(c, b' ' | b'\r' | b'\n' | b'\t')
    }

    /// `Linenoise::EditDeletePrevWord`.
    fn edit_delete_prev_word(&mut self) {
        let old_pos = self.pos;
        while self.pos > 0 && Self::is_space(self.byte(self.pos - 1)) {
            self.pos -= 1;
        }
        while self.pos > 0 && !Self::is_space(self.byte(self.pos - 1)) {
            self.pos -= 1;
        }
        self.buf.drain(self.pos..old_pos);
        self.refresh_line();
    }

    /// `Linenoise::EditDeleteNextWord`.
    fn edit_delete_next_word(&mut self) {
        let mut next_pos = self.pos;
        while next_pos < self.len() && Self::is_space(self.byte(next_pos)) {
            next_pos += 1;
        }
        while next_pos < self.len() && !Self::is_space(self.byte(next_pos)) {
            next_pos += 1;
        }
        self.buf.drain(self.pos..next_pos);
        self.refresh_line();
    }

    /// `Linenoise::EditRemoveSpaces`: the spaces around the cursor.
    fn edit_remove_spaces(&mut self) {
        let mut start_pos = self.pos;
        let mut end_pos = self.pos;
        while start_pos > 0 && self.byte(start_pos - 1) == b' ' {
            start_pos -= 1;
        }
        while end_pos < self.len() && self.byte(end_pos) == b' ' {
            end_pos += 1;
        }
        self.buf.drain(start_pos..end_pos);
        self.pos = start_pos;
        self.refresh_line();
    }

    /// `Linenoise::EditSwapCharacter`: the character before the cursor and the one under
    /// it.
    fn edit_swap_character(&mut self) {
        if self.pos == 0 || self.len() < 2 {
            return;
        }
        if self.pos + 1 > self.len() {
            self.pos = self.prev_char();
        }
        let prev_pos = self.prev_char();
        let next_pos = self.next_char();
        let previous: Vec<u8> = self.buf[prev_pos..self.pos].to_vec();
        let current: Vec<u8> = self.buf[self.pos..next_pos].to_vec();
        self.buf
            .splice(prev_pos..next_pos, current.into_iter().chain(previous));
        self.pos = next_pos;
        self.refresh_line();
    }

    /// `Linenoise::EditSwapWord`: the word before the cursor and the one at or after it.
    fn edit_swap_word(&mut self) {
        let len = self.len();
        // The end of the right word.
        let mut end_pos = self.pos;
        while end_pos < len && Self::is_space(self.byte(end_pos)) {
            end_pos += 1;
        }
        while end_pos < len && !Self::is_space(self.byte(end_pos)) {
            end_pos += 1;
        }
        let mut current_pos = end_pos;
        while current_pos > 0 && Self::is_space(self.byte(current_pos - 1)) {
            current_pos -= 1;
        }
        let right_word_end = current_pos;
        while current_pos > 0 && !Self::is_space(self.byte(current_pos - 1)) {
            current_pos -= 1;
        }
        let right_word_start = current_pos;
        while current_pos > 0 && Self::is_space(self.byte(current_pos - 1)) {
            current_pos -= 1;
        }
        let left_word_end = current_pos;
        while current_pos > 0 && !Self::is_space(self.byte(current_pos - 1)) {
            current_pos -= 1;
        }
        let left_word_start = current_pos;
        if left_word_start == right_word_start {
            return;
        }
        let left_len = left_word_end - left_word_start;
        let right_len = right_word_end - right_word_start;
        if left_len == 0 || right_len == 0 {
            return;
        }
        // [LEFT][RIGHT] becomes [RIGHT][LEFT], with spaces between.
        let left_word = self.buf[left_word_start..left_word_end].to_vec();
        let right_word = self.buf[right_word_start..right_word_end].to_vec();
        self.buf[left_word_start..end_pos].fill(b' ');
        self.buf[left_word_start..left_word_start + right_len].copy_from_slice(&right_word);
        self.buf[end_pos - left_len..end_pos].copy_from_slice(&left_word);
        self.pos = end_pos;
        self.refresh_line();
    }

    /// `Linenoise::EditDeleteAll`.
    fn edit_delete_all(&mut self) {
        self.buf.clear();
        self.pos = 0;
        self.refresh_line();
    }

    /// `Linenoise::SetCursorPosition`: moves the cursor by a mouse click's offset.
    fn set_cursor_position(&mut self, x: i32, y: i32) {
        let (current_row, current_col, rows, _) = self.position_to_col_and_row(self.pos);
        let current_col = current_col + x;
        let current_row = current_row + y;
        if current_col < 0 || current_row < 0 || current_row > rows {
            return;
        }
        self.pos = self.col_and_row_to_position(current_row, current_col);
        self.refresh_line();
    }

    /// `Linenoise::EditCapitalizeNextWord`.
    fn edit_capitalize_next_word(&mut self, capitalization: Capitalization) {
        let mut next_pos = self.pos;
        while next_pos < self.len() && Self::is_space(self.byte(next_pos)) {
            next_pos += 1;
        }
        let mut first_char = true;
        while next_pos < self.len() && !Self::is_space(self.byte(next_pos)) {
            let c = &mut self.buf[next_pos];
            match capitalization {
                Capitalization::Capitalize => {
                    if first_char {
                        first_char = false;
                        c.make_ascii_uppercase();
                    }
                }
                Capitalization::Lowercase => c.make_ascii_lowercase(),
                Capitalization::Uppercase => c.make_ascii_uppercase(),
            }
            next_pos += 1;
        }
        self.pos = next_pos;
        self.refresh_line();
    }

    /// `Linenoise::StartSearch`.
    fn start_search(&mut self) {
        self.search = true;
        self.search_buf.clone_from(&self.buf);
        self.search_matches.clear();
        self.search_index = 0;
        self.perform_search();
        self.refresh_search();
    }

    /// `Linenoise::CancelSearch`.
    fn cancel_search(&mut self) {
        self.search = false;
        self.search_buf.clear();
        self.search_matches.clear();
        self.search_index = 0;
        self.refresh_line();
    }

    /// `Linenoise::AcceptSearch`: the line becomes the match.
    fn accept_search(&mut self) {
        if let Some(found) = self.search_matches.get(self.search_index).copied() {
            self.buf = history::entry(found.history_index);
            self.pos = found.match_end;
        }
        self.cancel_search();
    }

    /// `Linenoise::PerformSearch`: the distinct history entries containing the search
    /// text (ignoring case), newest first, keeping the current match if it still matches.
    fn perform_search(&mut self) {
        let mut current_match = history::len();
        if let Some(found) = self.search_matches.get(self.search_index) {
            current_match = found.history_index;
        }
        self.search_matches.clear();
        self.search_index = 0;
        if self.search_buf.is_empty() {
            return;
        }
        let mut matches: HashSet<Vec<u8>> = HashSet::new();
        let lsearch = self.search_buf.to_ascii_lowercase();
        for history_index in (0..history::len()).rev() {
            let lhistory = history::entry(history_index).to_ascii_lowercase();
            if !matches.insert(lhistory.clone()) {
                continue;
            }
            if let Some(entry) = lhistory
                .windows(lsearch.len())
                .position(|window| window == lsearch.as_slice())
            {
                if history_index == current_match {
                    self.search_index = self.search_matches.len();
                }
                self.search_matches.push(SearchMatch {
                    history_index,
                    match_start: entry,
                    match_end: entry + lsearch.len(),
                });
            }
        }
    }

    fn search_prev(&mut self) {
        if self.search_index > 0 {
            self.search_index -= 1;
        } else if !self.search_matches.is_empty() {
            self.search_index = self.search_matches.len() - 1;
        }
    }

    fn search_next(&mut self) {
        self.search_index += 1;
        if self.search_index >= self.search_matches.len() {
            self.search_index = 0;
        }
    }

    /// `Linenoise::Search`: a key during the history search; returns the key to handle
    /// after the search ends (`KEY_NULL` for none).
    fn search_key(&mut self, key_press: KeyPress) -> KeyPress {
        match key_press.action {
            key::CTRL_J | key::ENTER => {
                self.accept_search();
                return KeyPress::action(key::ENTER);
            }
            key::CTRL_R | key::CTRL_S | key::CTRL_N => self.search_next(),
            key::ESC => match key_press.sequence {
                EscapeSequence::Escape => {
                    self.accept_search();
                    return KeyPress::action(key::KEY_NULL);
                }
                EscapeSequence::Up => self.search_prev(),
                EscapeSequence::Down => self.search_next(),
                EscapeSequence::Home => {
                    self.accept_search();
                    return KeyPress::action(key::CTRL_A);
                }
                EscapeSequence::End => {
                    self.accept_search();
                    return KeyPress::action(key::CTRL_E);
                }
                EscapeSequence::Left => {
                    self.accept_search();
                    return KeyPress::action(key::CTRL_B);
                }
                EscapeSequence::Right => {
                    self.accept_search();
                    return KeyPress::action(key::CTRL_F);
                }
                _ => {}
            },
            key::CTRL_A => {
                self.accept_search();
                return KeyPress::action(key::CTRL_A);
            }
            key::TAB | key::CTRL_E => {
                self.accept_search();
                return KeyPress::action(key::CTRL_E);
            }
            action @ (key::CTRL_B | key::CTRL_F | key::CTRL_T | key::CTRL_U | key::CTRL_K | key::CTRL_D) => {
                self.accept_search();
                return KeyPress::action(action);
            }
            key::CTRL_L => terminal::clear_screen(),
            key::CTRL_P => self.search_prev(),
            key::CTRL_C | key::CTRL_G => {
                self.cancel_search();
                return KeyPress::action(key::KEY_NULL);
            }
            key::BACKSPACE | key::CTRL_H | key::CTRL_W => {
                // The last UTF-8 character: its continuation bytes, then its first byte.
                while self.search_buf.last().is_some_and(|&c| c & 0xc0 == 0x80) {
                    self.search_buf.pop();
                }
                self.search_buf.pop();
                self.perform_search();
            }
            action => {
                self.search_buf.push(action);
                self.perform_search();
            }
        }
        self.refresh_search();
        KeyPress::action(key::KEY_NULL)
    }

    /// `Linenoise::AllWhitespace`: only whitespace and comments.
    fn all_whitespace(z: &[u8]) -> bool {
        crate::shell::all_whitespace(z)
    }

    /// `Linenoise::HandleTerminalResize`: in multi-line mode, a new terminal size changes
    /// the rows the line takes.
    fn handle_terminal_resize(&mut self) {
        if !terminal::is_multiline() {
            return;
        }
        let new_size = terminal::get_terminal_size();
        if new_size == self.ws {
            return;
        }
        self.ws = new_size;
        let (cursor_row, _, rows, _) = self.position_to_col_and_row(self.pos);
        self.old_cursor_rows = cursor_row.max(0) as usize;
        self.maxrows = rows.max(0) as usize;
        if self.rendered_completion_lines > 0 {
            // The rows the rendered completions take now.
            let mut completion_text = Vec::new();
            for (i, completion) in self.completion_list.completions.iter().enumerate() {
                if i > 0 {
                    completion_text.push(b' ');
                }
                completion_text.extend_from_slice(&completion.original_completion);
            }
            let (_, _, completion_rows, _) = self.position_to_col_and_row_in(0, &completion_text, 0);
            self.rendered_completion_lines = completion_rows.max(0) as usize;
            self.maxrows += self.rendered_completion_lines;
        }
    }

    /// `Linenoise::TryGetKeyPress`: a key read ahead, or the next key of the terminal.
    fn try_get_key_press(&mut self) -> Option<KeyPress> {
        if let Some(key_press) = BufferedKeyPresses::try_get_key_press() {
            self.has_more_data = BufferedKeyPresses::has_more_data();
            return Some(key_press);
        }
        let c = terminal::read_byte(self.ifd)?;
        self.has_more_data = terminal::has_more_data(self.ifd, 0);
        if !self.has_more_data {
            self.handle_terminal_resize();
        }
        let mut key_press = KeyPress::action(c);
        if c == key::ESC {
            key_press.sequence = terminal::read_escape_sequence(self.ifd, &mut key_press);
        }
        Some(key_press)
    }

    /// `Linenoise::Write`.
    fn write(fd: i32, data: &[u8]) -> bool {
        terminal::write_fd(fd, data)
    }

    /// `Linenoise::Edit`: edits the line until Enter (with a complete statement in
    /// multi-line mode), Ctrl+C or Ctrl+D; `None` for the end of input.
    fn edit(&mut self) -> Option<usize> {
        // The newest history entry is the line being edited.
        history::add(b"");
        if !Self::write(self.ofd, &self.prompt) {
            return None;
        }
        loop {
            let Some(mut key_press) = self.try_get_key_press() else {
                return Some(self.len());
            };
            self.render = true;
            self.insert = false;
            if self.search {
                let next_action = self.search_key(key_press);
                if self.search || next_action.action == key::KEY_NULL {
                    continue;
                }
                key_press = next_action;
            }
            if key_press.action == key::TAB && self.completion_callback.is_some() {
                if self.has_more_data {
                    // A pasted tab: spaces.
                    self.edit_insert_multi(b"    ");
                    continue;
                }
                while key_press.action == key::TAB {
                    if !self.complete_line(&mut key_press) {
                        return Some(self.len());
                    }
                }
                if key_press.action == key::KEY_NULL {
                    self.refresh_line();
                    continue;
                }
            }
            match key_press.action {
                key::CTRL_G | key::CTRL_J | key::ENTER => {
                    if let Some(result) = self.handle_enter(key_press) {
                        return Some(result);
                    }
                }
                key::CTRL_O | key::CTRL_C => {
                    if terminal::is_multiline() {
                        self.continuation_markers = false;
                        // Force a refresh at the end.
                        self.pos = 0;
                        self.edit_move_end();
                    }
                    // `\x03`, twice if the line had text: the shell tells clearing a
                    // line from asking to exit.
                    self.buf = if self.buf.is_empty() {
                        vec![key::CTRL_C]
                    } else {
                        vec![key::CTRL_C, key::CTRL_C]
                    };
                    self.pos = self.len();
                    return Some(self.len());
                }
                key::BACKSPACE | key::CTRL_H => self.edit_backspace(),
                key::CTRL_D => {
                    // Deletes the character under the cursor; on an empty line it is
                    // the end of input.
                    if self.buf.is_empty() {
                        history::remove_last_entry();
                        return None;
                    }
                    self.edit_delete();
                }
                key::CTRL_Z => {
                    terminal::disable_raw_mode();
                    // SAFETY: raising SIGTSTP suspends the process like the terminal would.
                    unsafe {
                        libc::raise(libc::SIGTSTP);
                    }
                    terminal::enable_raw_mode();
                    self.refresh_line();
                }
                key::CTRL_T => self.edit_swap_character(),
                key::CTRL_B => self.edit_move_left(),
                key::CTRL_F => self.edit_move_right(),
                key::CTRL_P => self.edit_history_next(HistoryScrollDirection::Prev),
                key::CTRL_N => self.edit_history_next(HistoryScrollDirection::Next),
                key::CTRL_S | key::CTRL_R => self.start_search(),
                key::ESC => self.handle_escape(key_press),
                key::CTRL_U => self.edit_delete_all(),
                key::CTRL_K => {
                    self.buf.truncate(self.pos);
                    self.refresh_line();
                }
                key::CTRL_A => self.edit_move_start_of_line(),
                key::CTRL_E => self.edit_move_end_of_line(),
                key::CTRL_L => {
                    terminal::clear_screen();
                    self.clear_screen = true;
                    self.refresh_line();
                }
                key::CTRL_W => self.edit_delete_prev_word(),
                key::CTRL_X => self.edit_insert_multi(b"\r\n"),
                key::CTRL_Q => terminal::enable_mouse_tracking(),
                key::CTRL_Y => {
                    // Unsupported.
                }
                action => self.edit_insert(action),
            }
        }
    }

    /// Enter, Ctrl+J or Ctrl+G: `.edit` opens the editor; in multi-line mode an incomplete
    /// statement continues on a new line (Ctrl+G completes it with `;`). Returns the
    /// length of the line when it is done.
    fn handle_enter(&mut self, key_press: KeyPress) -> Option<usize> {
        if !self.buf.is_empty() {
            // Is the last line `.edit` or `\e`?
            let begin_pos = self
                .buf
                .iter()
                .rposition(|&c| c == b'\n')
                .map_or(0, |newline| newline + 1);
            let last_line = &self.buf[begin_pos..];
            if last_line == b".edit" || last_line == b"\\e" {
                self.buf.truncate(begin_pos);
                self.pos = begin_pos;
                if !self.edit_buffer_with_editor(None) {
                    // Show the line without `.edit`.
                    self.refresh_line();
                    return None;
                }
            }
        }
        if terminal::is_multiline() && !self.buf.is_empty() {
            let text = String::from_utf8_lossy(&self.buf).into_owned();
            if self.buf[0] != b'.'
                && !Self::all_whitespace(&self.buf)
                && !crate::shell::sql_is_complete(&text)
            {
                // Not a complete statement yet.
                self.pos = self.len();
                if key_press.action != key::CTRL_G {
                    self.edit_insert_multi(b"\r\n");
                    return None;
                }
                self.edit_insert_multi(b";");
            }
        }
        // The last rendering, without continuation markers.
        self.continuation_markers = false;
        history::remove_last_entry();
        if terminal::is_multiline() {
            if self.pos == self.len() {
                self.refresh_line();
            } else {
                self.edit_move_end();
            }
        }
        // `\r\n` becomes `\n`.
        let mut line = Vec::with_capacity(self.len());
        for (i, &c) in self.buf.iter().enumerate() {
            if c == b'\r' && self.byte(i + 1) == b'\n' {
                continue;
            }
            line.push(c);
        }
        self.buf = line;
        Some(self.len())
    }

    /// The keys of escape sequences.
    fn handle_escape(&mut self, key_press: KeyPress) {
        match key_press.sequence {
            EscapeSequence::CtrlUp => self.edit_history_next(HistoryScrollDirection::Start),
            EscapeSequence::CtrlDown => self.edit_history_next(HistoryScrollDirection::End),
            EscapeSequence::CtrlMoveBackwards | EscapeSequence::AltLeftArrow | EscapeSequence::Alt(b'b') => {
                self.edit_move_word_left();
            }
            EscapeSequence::CtrlMoveForwards | EscapeSequence::AltRightArrow | EscapeSequence::Alt(b'f') => {
                self.edit_move_word_right();
            }
            EscapeSequence::Alt(b'd') => self.edit_delete_next_word(),
            EscapeSequence::Alt(b'c') => self.edit_capitalize_next_word(Capitalization::Capitalize),
            EscapeSequence::Alt(b'l') => self.edit_capitalize_next_word(Capitalization::Lowercase),
            EscapeSequence::Alt(b'n' | b'p') => self.start_search(),
            EscapeSequence::Alt(b't') => self.edit_swap_word(),
            EscapeSequence::Alt(b'u') => self.edit_capitalize_next_word(Capitalization::Uppercase),
            EscapeSequence::Alt(b'r') => self.edit_delete_all(),
            EscapeSequence::AltBackspace => self.edit_delete_prev_word(),
            EscapeSequence::AltBackslash => self.edit_remove_spaces(),
            EscapeSequence::Home => self.edit_move_home(),
            EscapeSequence::End => self.edit_move_end(),
            EscapeSequence::Up => {
                if !self.edit_move_row_up() {
                    self.edit_history_next(HistoryScrollDirection::Prev);
                }
            }
            EscapeSequence::Down => {
                if !self.edit_move_row_down() {
                    self.edit_history_next(HistoryScrollDirection::Next);
                }
            }
            EscapeSequence::Right => self.edit_move_right(),
            EscapeSequence::Left => self.edit_move_left(),
            EscapeSequence::DeleteKey => self.edit_delete(),
            EscapeSequence::MouseClick => {
                self.set_cursor_position(key_press.position.ws_col, key_press.position.ws_row);
            }
            _ => {}
        }
    }

    /// `Linenoise::EditFileWithEditor`: `DUCKDB_EDITOR`, `EDITOR`, `VISUAL` or `vi`.
    fn edit_file_with_editor(file_name: &str, editor: Option<&str>) -> bool {
        let editor = editor.map_or_else(
            || {
                ["DUCKDB_EDITOR", "EDITOR", "VISUAL"]
                    .iter()
                    .find_map(|name| std::env::var(name).ok())
                    .unwrap_or_else(|| "vi".to_owned())
            },
            str::to_owned,
        );
        // The editor is not quoted: it may have options (`EDITOR="pico -t"`).
        crate::state::run_system(&format!("exec {editor} '{file_name}'")) == 0
    }

    /// `Linenoise::GetTemporaryDirectory`.
    fn temporary_directory() -> String {
        std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".to_owned())
    }

    /// `Linenoise::EditBufferWithEditor`: `.edit` edits the line (or the previous history
    /// entry if it is empty) in a temporary file; its lines end in `\r\n` after.
    fn edit_buffer_with_editor(&mut self, editor: Option<&str>) -> bool {
        let file_name = format!(
            "{}/duckdb.edit.{}.sql",
            Self::temporary_directory(),
            std::process::id()
        );
        let mut write_buffer = self.buf.clone();
        if write_buffer.is_empty() {
            let edit_index = history::len();
            if edit_index >= 2 {
                write_buffer = history::entry(edit_index - 2);
            }
        }
        if std::fs::write(&file_name, &write_buffer).is_err() {
            let _ = std::fs::remove_file(&file_name);
            terminal::beep();
            return false;
        }
        if !Self::edit_file_with_editor(&file_name, editor) {
            terminal::beep();
            return false;
        }
        let Ok(contents) = std::fs::read(&file_name) else {
            let _ = std::fs::remove_file(&file_name);
            terminal::beep();
            return false;
        };
        let mut new_buffer = Vec::with_capacity(contents.len());
        for line in contents.split_inclusive(|&c| c == b'\n') {
            let mut line_len = line.len();
            while line_len > 0 && (line[line_len - 1] == b'\r' || line[line_len - 1] == b'\n') {
                line_len -= 1;
            }
            new_buffer.extend_from_slice(&line[..line_len]);
            if line_len != line.len() {
                new_buffer.extend_from_slice(b"\r\n");
            }
        }
        if std::fs::remove_file(&file_name).is_err() {
            terminal::beep();
            return false;
        }
        self.buf = new_buffer;
        self.pos = self.len();
        self.refresh_line();
        true
    }
}
