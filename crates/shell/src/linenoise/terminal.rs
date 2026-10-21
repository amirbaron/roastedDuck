//! The terminal under the line editor (DuckDB's `tools/shell/linenoise/terminal.cpp`):
//! raw mode, key presses and escape sequences, the terminal size and background color.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};

use super::{BufferedKeyPresses, KeyPress, LINENOISE_MAX_LINE, Linenoise, history};

/// Key codes (`KEY_ACTION`).
pub mod key {
    pub const KEY_NULL: u8 = 0;
    pub const CTRL_A: u8 = 1;
    pub const CTRL_B: u8 = 2;
    pub const CTRL_C: u8 = 3;
    pub const CTRL_D: u8 = 4;
    pub const CTRL_E: u8 = 5;
    pub const CTRL_F: u8 = 6;
    pub const CTRL_G: u8 = 7;
    pub const CTRL_H: u8 = 8;
    pub const TAB: u8 = 9;
    pub const CTRL_J: u8 = 10;
    pub const CTRL_K: u8 = 11;
    pub const CTRL_L: u8 = 12;
    pub const ENTER: u8 = 13;
    pub const CTRL_N: u8 = 14;
    pub const CTRL_O: u8 = 15;
    pub const CTRL_P: u8 = 16;
    pub const CTRL_Q: u8 = 17;
    pub const CTRL_R: u8 = 18;
    pub const CTRL_S: u8 = 19;
    pub const CTRL_T: u8 = 20;
    pub const CTRL_U: u8 = 21;
    pub const CTRL_W: u8 = 23;
    pub const CTRL_X: u8 = 24;
    pub const CTRL_Y: u8 = 25;
    pub const CTRL_Z: u8 = 26;
    pub const ESC: u8 = 27;
    pub const BACKSPACE: u8 = 127;
}

/// The key an escape sequence stands for (`EscapeSequence`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EscapeSequence {
    Invalid,
    Unknown,
    CtrlMoveBackwards,
    CtrlMoveForwards,
    Home,
    End,
    Up,
    Down,
    Right,
    Left,
    DeleteKey,
    ShiftTab,
    Escape,
    /// Alt with a letter, `b'a'` to `b'z'`.
    Alt(u8),
    AltBackspace,
    AltLeftArrow,
    AltRightArrow,
    AltBackslash,
    CtrlUp,
    CtrlDown,
    MouseClick,
}

/// The size of the terminal, or a position in it (`TerminalSize`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TerminalSize {
    pub ws_col: i32,
    pub ws_row: i32,
}

/// A color of the terminal (`TerminalColor`).
#[derive(Clone, Copy, Debug, Default)]
pub struct TerminalColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

/// `mlmode`: multi-line editing, on by default.
static MULTILINE: AtomicBool = AtomicBool::new(true);
/// `orig_termios` while in raw mode (`rawmode`).
static ORIGINAL_TERMIOS: Mutex<Option<libc::termios>> = Mutex::new(None);
/// `atexit_registered`.
static AT_EXIT_REGISTERED: AtomicBool = AtomicBool::new(false);
/// `mouse_tracking`.
static MOUSE_TRACKING: AtomicBool = AtomicBool::new(false);
/// `unsupported_term`.
const UNSUPPORTED_TERM: [&str; 3] = ["dumb", "cons25", "emacs"];

const STDIN_FILENO: i32 = 0;
const STDOUT_FILENO: i32 = 1;

/// `linenoiseAtExit`: the terminal is restored when the process exits.
extern "C" fn linenoise_at_exit() {
    disable_raw_mode();
    history::free();
}

/// `read(fd, buf, 1)`: the next byte, `None` on end of input or error.
pub fn read_byte(fd: i32) -> Option<u8> {
    let mut byte = 0u8;
    // SAFETY: reads at most one byte into `byte`.
    let read = unsafe { libc::read(fd, (&raw mut byte).cast(), 1) };
    (read == 1).then_some(byte)
}

/// `write(fd, data, size)`; `false` on failure.
pub fn write_fd(fd: i32, data: &[u8]) -> bool {
    // SAFETY: writes `data.len()` bytes from `data`.
    let written = unsafe { libc::write(fd, data.as_ptr().cast(), data.len()) };
    written != -1
}

/// `isatty`.
fn is_tty(fd: i32) -> bool {
    // SAFETY: `isatty` only inspects the descriptor.
    unsafe { libc::isatty(fd) == 1 }
}

/// `Terminal::IsUnsupportedTerm`: terminals that do not understand escape sequences.
pub fn is_unsupported_term() -> bool {
    std::env::var("TERM").is_ok_and(|term| {
        UNSUPPORTED_TERM
            .iter()
            .any(|unsupported| unsupported.eq_ignore_ascii_case(&term))
    })
}

/// `EnableRawModeInternal`.
fn enable_raw_mode_internal(fd: i32) -> bool {
    // SAFETY: `tcgetattr` fills the struct passed.
    let mut original: libc::termios = unsafe { std::mem::zeroed() };
    // SAFETY: as above.
    if unsafe { libc::tcgetattr(fd, &raw mut original) } == -1 {
        return false;
    }
    let mut raw = original;
    // Input: no break, no CR to NL, no parity check, no strip char, no start/stop output
    // control; 8 bit characters.
    raw.c_iflag &= !(libc::BRKINT | libc::ICRNL | libc::INPCK | libc::ISTRIP | libc::IXON);
    raw.c_iflag |= libc::IUTF8;
    // Output: no post processing.
    raw.c_oflag &= !libc::OPOST;
    raw.c_cflag |= libc::CS8;
    // Local: no echo, not canonical, no extended functions, no signal characters.
    raw.c_lflag &= !(libc::ECHO | libc::ICANON | libc::IEXTEN | libc::ISIG);
    // Every read returns a byte, without timeout.
    raw.c_cc[libc::VMIN] = 1;
    raw.c_cc[libc::VTIME] = 0;
    // SAFETY: `raw` is a valid termios.
    if unsafe { libc::tcsetattr(fd, libc::TCSADRAIN, &raw const raw) } < 0 {
        return false;
    }
    *ORIGINAL_TERMIOS.lock().unwrap_or_else(PoisonError::into_inner) = Some(original);
    true
}

/// `DisableRawModeInternal`.
fn disable_raw_mode_internal(fd: i32) {
    let mut original = ORIGINAL_TERMIOS.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(termios) = original.as_ref() {
        // SAFETY: `termios` was read by `tcgetattr`.
        if unsafe { libc::tcsetattr(fd, libc::TCSADRAIN, termios) } != -1 {
            *original = None;
        }
    }
}

fn raw_mode() -> bool {
    ORIGINAL_TERMIOS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .is_some()
}

/// `Terminal::EnableRawMode`.
pub fn enable_raw_mode() -> bool {
    if !is_tty(STDIN_FILENO) {
        return false;
    }
    if !AT_EXIT_REGISTERED.swap(true, Ordering::Relaxed) {
        // SAFETY: registers a function without arguments to run at exit.
        unsafe {
            libc::atexit(linenoise_at_exit);
        }
    }
    enable_raw_mode_internal(STDIN_FILENO)
}

/// `Terminal::DisableRawMode`.
pub fn disable_raw_mode() {
    disable_mouse_tracking();
    disable_raw_mode_internal(STDIN_FILENO);
}

/// `printf` and `fflush(stdout)` of the escape sequences of mouse tracking.
fn print_stdout(text: &str) {
    crate::printer::write_stdout(text.as_bytes());
    crate::printer::flush_stdout();
}

/// `Terminal::EnableMouseTracking`: XTerm's normal tracking.
pub fn enable_mouse_tracking() {
    if !raw_mode() {
        return;
    }
    print_stdout("\x1b[?1000h");
    MOUSE_TRACKING.store(true, Ordering::Relaxed);
}

/// `Terminal::DisableMouseTracking`.
pub fn disable_mouse_tracking() {
    if !MOUSE_TRACKING.load(Ordering::Relaxed) {
        return;
    }
    print_stdout("\x1b[?1000l");
    MOUSE_TRACKING.store(false, Ordering::Relaxed);
}

/// `Terminal::IsMultiline`.
pub fn is_multiline() -> bool {
    MULTILINE.load(Ordering::Relaxed)
}

/// `Terminal::SetMultiLine`.
pub fn set_multiline(multiline: bool) {
    MULTILINE.store(multiline, Ordering::Relaxed);
}

/// `Terminal::IsAtty`.
pub fn is_atty() -> bool {
    is_tty(STDIN_FILENO)
}

/// `fgetc(stdin)`: the next byte of the buffered standard input.
fn read_stdin_byte() -> Option<u8> {
    use std::io::BufRead;
    let mut stdin = std::io::stdin().lock();
    let byte = *stdin.fill_buf().ok()?.first()?;
    stdin.consume(1);
    Some(byte)
}

/// `Terminal::EditNoTTY`: a line of a stdin that is not a terminal, without limit.
pub fn edit_no_tty() -> Option<Vec<u8>> {
    let mut line = Vec::new();
    loop {
        match read_stdin_byte() {
            None if line.is_empty() => return None,
            None | Some(b'\n') => return Some(line),
            Some(c) => line.push(c),
        }
    }
}

/// `Terminal::EditRaw`: edits a line with the terminal in raw mode.
pub fn edit_raw<'a>(
    prompt: &[u8],
    completion: Option<&'a mut super::CompletionCallback<'a>>,
) -> Option<Vec<u8>> {
    if !enable_raw_mode() {
        return None;
    }
    let mut editor = Linenoise::new(
        STDIN_FILENO,
        STDOUT_FILENO,
        LINENOISE_MAX_LINE,
        prompt,
        completion,
    );
    let count = editor.edit();
    disable_raw_mode();
    crate::printer::write_stdout(b"\n");
    count.map(|_| editor.into_buffer())
}

/// `Terminal::HasMoreData`: whether `fd` has input within `timeout_micros`.
pub fn has_more_data(fd: i32, timeout_micros: u64) -> bool {
    // SAFETY: the set is initialized by FD_ZERO and holds only `fd`.
    unsafe {
        let mut read_fds: libc::fd_set = std::mem::zeroed();
        libc::FD_ZERO(&raw mut read_fds);
        libc::FD_SET(fd, &raw mut read_fds);
        let mut timeout = libc::timeval {
            tv_sec: (timeout_micros / 1_000_000) as libc::time_t,
            tv_usec: (timeout_micros % 1_000_000) as libc::suseconds_t,
        };
        libc::select(
            fd + 1,
            &raw mut read_fds,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &raw mut timeout,
        ) > 0
    }
}

/// `parseInt`: leading digits, capped at 1000000; returns the value and the digit count.
fn parse_int(text: &[u8]) -> (i32, usize) {
    let mut result: i32 = 0;
    let mut idx = 0;
    while idx < text.len() && text[idx].is_ascii_digit() {
        result = result * 10 + i32::from(text[idx] - b'0');
        if result > 1_000_000 {
            result = 1_000_000;
        }
        idx += 1;
    }
    (result, idx)
}

/// `tryParseEnv`.
fn try_parse_env(name: &str) -> i32 {
    std::env::var(name).map_or(0, |value| parse_int(value.as_bytes()).0)
}

/// `Terminal::GetCursorPosition`: asks the terminal (`ESC [6n`).
pub fn get_cursor_position() -> TerminalSize {
    let mut ws = TerminalSize::default();
    if !write_fd(STDOUT_FILENO, b"\x1b[6n") {
        return ws;
    }
    // The answer: ESC [ rows ; cols R.
    let mut buf = Vec::new();
    while buf.len() < 31 {
        let Some(c) = read_byte(STDIN_FILENO) else {
            break;
        };
        if c == b'R' {
            break;
        }
        buf.push(c);
    }
    if buf.len() < 2 || buf[0] != key::ESC || buf[1] != b'[' {
        return ws;
    }
    let (row, digits) = parse_int(&buf[2..]);
    ws.ws_row = row;
    let offset = 2 + digits;
    if buf.get(offset) != Some(&b';') {
        return ws;
    }
    ws.ws_col = parse_int(&buf[offset + 1..]).0;
    ws
}

/// `Terminal::TryMeasureTerminalSize`: the cursor position after moving to the
/// bottom-right corner.
fn try_measure_terminal_size() -> TerminalSize {
    let start = get_cursor_position();
    if start.ws_col == 0 {
        return TerminalSize::default();
    }
    if !write_fd(STDOUT_FILENO, b"\x1b[999;999f") {
        return TerminalSize::default();
    }
    let result = get_cursor_position();
    if result.ws_col == 0 {
        return result;
    }
    // Restore the position.
    write_fd(
        STDOUT_FILENO,
        format!("\x1b[{};{}f", start.ws_row, start.ws_col).as_bytes(),
    );
    result
}

/// `ParseTerminalColor`: the color of an OSC 11 answer (`ESC ]11;rgb:RRRR/GGGG/BBBB`);
/// bytes before it are key presses to keep.
fn parse_terminal_color(buf: &[u8]) -> Option<TerminalColor> {
    const RGB_FORMAT: &[u8] = b"\x1b]11;rgb:";
    let mut offset = 0;
    while offset + RGB_FORMAT.len() < buf.len() {
        if buf[offset..].starts_with(RGB_FORMAT) {
            break;
        }
        BufferedKeyPresses::buffer_key_press(KeyPress::action(buf[offset]));
        offset += 1;
    }
    offset += RGB_FORMAT.len();
    if offset >= buf.len() {
        return None;
    }
    let mut values = [0u8; 3];
    for (k, value_out) in values.iter_mut().enumerate() {
        if k > 0 {
            if buf.get(offset) != Some(&b'/') {
                return None;
            }
            offset += 1;
        }
        // Hexadecimal values of 0 to 65535.
        let mut value: u32 = 0;
        let end_pos = offset + 4;
        while offset < end_pos {
            let c = *buf.get(offset)?;
            if c == b'/' {
                break;
            }
            value = value * 16 + char::from(c).to_digit(16)?;
            offset += 1;
        }
        *value_out = (value >> 8) as u8;
    }
    Some(TerminalColor {
        r: values[0],
        g: values[1],
        b: values[2],
    })
}

/// `Terminal::BufferAvailableInput`: keeps the input already typed for the editor.
pub fn buffer_available_input() {
    if !is_tty(STDIN_FILENO) {
        return;
    }
    while has_more_data(STDIN_FILENO, 0) {
        let Some(c) = read_byte(STDIN_FILENO) else {
            break;
        };
        BufferedKeyPresses::buffer_key_press(KeyPress::action(c));
    }
}

/// `Terminal::TryGetBackgroundColor`: asks the terminal for its background color (OSC
/// 11) followed by its device attributes (DA1), which every terminal answers, so that a
/// terminal without OSC 11 does not leave us waiting.
pub fn try_get_background_color() -> Option<TerminalColor> {
    let mut ifd = STDIN_FILENO;
    let mut ofd = STDOUT_FILENO;
    if !is_tty(ifd) {
        // SAFETY: opens the controlling terminal.
        ifd = unsafe { libc::open(c"/dev/tty".as_ptr(), libc::O_RDWR) };
        if ifd < 0 {
            return None;
        }
        ofd = ifd;
    }
    let close = |fd: i32| {
        if fd != STDIN_FILENO {
            // SAFETY: `fd` was opened above.
            unsafe {
                libc::close(fd);
            }
        }
    };
    if !enable_raw_mode_internal(ifd) {
        close(ifd);
        return None;
    }
    let mut color = None;
    if write_fd(ofd, b"\x1b]11;?\x07\x1b[c") {
        let mut buf = Vec::new();
        let mut found_da1 = false;
        let mut da1_start = 0;
        loop {
            if !has_more_data(ifd, 5_000_000) {
                crate::printer::write_stderr(
                    b"Timeout trying to read terminal background color (> 5s elapsed).\nDisable terminal \
                      background color detection by using duckdb -dark-mode or duckdb -light-mode.\nThis likely \
                      means duckdb does not correctly support your CLI.\nPlease file an issue.\n",
                );
                break;
            }
            let Some(c) = read_byte(ifd) else {
                break;
            };
            buf.push(c);
            if !found_da1 {
                if buf.ends_with(b"\x1b[?") {
                    found_da1 = true;
                    da1_start = buf.len() - 3;
                }
            } else if c == b'c' {
                break;
            }
        }
        // The OSC 11 answer, if any, comes before the DA1 answer.
        let response_size = if found_da1 { da1_start } else { buf.len() };
        color = parse_terminal_color(&buf[..response_size]);
    }
    disable_raw_mode_internal(ifd);
    close(ifd);
    color
}

/// `Terminal::GetTerminalSize`: from `ioctl`, `COLUMNS`/`ROWS`, measuring, or 80x24.
pub fn get_terminal_size() -> TerminalSize {
    let mut result = TerminalSize::default();
    // SAFETY: `TIOCGWINSZ` writes a `winsize` into the struct passed.
    let mut size: libc::winsize = unsafe { std::mem::zeroed() };
    // SAFETY: as above.
    unsafe {
        libc::ioctl(STDOUT_FILENO, libc::TIOCGWINSZ, &raw mut size);
    }
    result.ws_col = i32::from(size.ws_col);
    result.ws_row = i32::from(size.ws_row);
    if result.ws_col == 0 {
        result.ws_col = try_parse_env("COLUMNS");
    }
    if result.ws_row == 0 {
        result.ws_row = try_parse_env("ROWS");
    }
    if result.ws_col == 0 || result.ws_row == 0 {
        let measured = try_measure_terminal_size();
        if measured.ws_row != 0 {
            result.ws_row = measured.ws_row;
        }
        if measured.ws_col != 0 {
            result.ws_col = measured.ws_col;
        }
    }
    if result.ws_col == 0 {
        result.ws_col = 80;
    }
    if result.ws_row == 0 {
        result.ws_row = 24;
    }
    result
}

/// `Terminal::ClearScreen` (Ctrl+L).
pub fn clear_screen() {
    write_fd(STDOUT_FILENO, b"\x1b[H\x1b[2J");
}

/// `Terminal::Beep`.
pub fn beep() {
    crate::printer::write_stderr(b"\x07");
}

/// `Terminal::ReadEscapeSequence(int, char[])`: the bytes after an ESC, up to 5.
fn read_escape_sequence_bytes(ifd: i32) -> Vec<u8> {
    let mut seq = Vec::with_capacity(5);
    let read = |count: usize, seq: &mut Vec<u8>| -> bool {
        for _ in 0..count {
            match read_byte(ifd) {
                Some(c) => seq.push(c),
                None => return false,
            }
        }
        true
    };
    if !read(1, &mut seq) {
        return Vec::new();
    }
    if seq[0] != b'O' && seq[0] != b'[' {
        return seq;
    }
    if !read(1, &mut seq) {
        return Vec::new();
    }
    if seq[0] != b'[' {
        return seq;
    }
    if seq[1] == b'M' {
        // A mouse event: three more bytes.
        if !read(3, &mut seq) {
            return Vec::new();
        }
        return seq;
    }
    if !seq[1].is_ascii_digit() {
        return seq;
    }
    // An extended escape: one more byte, two more after a `;`.
    if !read(1, &mut seq) {
        return Vec::new();
    }
    if seq[2] == b';' && !read(2, &mut seq) {
        return Vec::new();
    }
    seq
}

/// `Terminal::ReadEscapeSequence`: the key of the escape sequence after an ESC.
pub fn read_escape_sequence(ifd: i32, key_press: &mut KeyPress) -> EscapeSequence {
    let seq = read_escape_sequence_bytes(ifd);
    match seq.len() {
        0 => EscapeSequence::Invalid,
        1 => match seq[0] {
            c @ b'a'..=b'z' => EscapeSequence::Alt(c),
            c @ b'A'..=b'Z' => EscapeSequence::Alt(c.to_ascii_lowercase()),
            key::BACKSPACE => EscapeSequence::AltBackspace,
            key::ESC => {
                // A double ESC may be Alt with an arrow key.
                match read_escape_sequence(ifd, key_press) {
                    EscapeSequence::Left => EscapeSequence::AltLeftArrow,
                    EscapeSequence::Right => EscapeSequence::AltRightArrow,
                    _ => EscapeSequence::Escape,
                }
            }
            b'<' => EscapeSequence::AltLeftArrow,
            b'>' => EscapeSequence::AltRightArrow,
            b'\\' => EscapeSequence::AltBackslash,
            _ => EscapeSequence::Unknown,
        },
        2 if seq[0] == b'O' => match seq[1] {
            b'A' => EscapeSequence::Up,
            b'B' => EscapeSequence::Down,
            b'C' => EscapeSequence::Right,
            b'D' => EscapeSequence::Left,
            b'H' => EscapeSequence::Home,
            b'F' => EscapeSequence::End,
            b'c' => EscapeSequence::Alt(b'f'),
            b'd' => EscapeSequence::Alt(b'b'),
            _ => EscapeSequence::Unknown,
        },
        2 if seq[0] == b'[' => match seq[1] {
            b'A' => EscapeSequence::Up,
            b'B' => EscapeSequence::Down,
            b'C' => EscapeSequence::Right,
            b'D' => EscapeSequence::Left,
            b'H' => EscapeSequence::Home,
            b'F' => EscapeSequence::End,
            b'Z' => EscapeSequence::ShiftTab,
            _ => EscapeSequence::Unknown,
        },
        3 if seq[2] == b'~' => match seq[1] {
            b'1' => EscapeSequence::Home,
            b'3' => EscapeSequence::DeleteKey,
            b'4' | b'8' => EscapeSequence::End,
            _ => EscapeSequence::Unknown,
        },
        3 if seq[1] == b'5' && seq[2] == b'C' => EscapeSequence::Alt(b'f'),
        3 if seq[1] == b'5' && seq[2] == b'D' => EscapeSequence::Alt(b'b'),
        5 => match &seq[..] {
            b"[1;5A" => EscapeSequence::CtrlUp,
            b"[1;5B" => EscapeSequence::CtrlDown,
            b"[1;5C" | b"[1;3C" => EscapeSequence::CtrlMoveForwards,
            b"[1;5D" | b"[1;3D" => EscapeSequence::CtrlMoveBackwards,
            _ if seq.starts_with(b"[M") => {
                // A mouse event: only a left click moves the cursor.
                let mut result_sequence = EscapeSequence::Unknown;
                if seq[2] == b' ' {
                    result_sequence = EscapeSequence::MouseClick;
                    // The coordinates are X + 32 and Y + 32.
                    key_press.position.ws_col = i32::from(seq[3]);
                    key_press.position.ws_row = i32::from(seq[4]);
                    if key_press.position.ws_col <= 32 || key_press.position.ws_row < 32 {
                        result_sequence = EscapeSequence::Unknown;
                    } else {
                        key_press.position.ws_col -= 33;
                        key_press.position.ws_row -= 32;
                    }
                }
                // Relative to the cursor.
                let cursor_position = get_cursor_position();
                key_press.position.ws_col -= cursor_position.ws_col;
                key_press.position.ws_row -= cursor_position.ws_row;
                // One mouse event at a time.
                disable_mouse_tracking();
                result_sequence
            }
            _ => EscapeSequence::Unknown,
        },
        _ => EscapeSequence::Unknown,
    }
}
