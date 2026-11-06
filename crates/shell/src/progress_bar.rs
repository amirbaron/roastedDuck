//! The status line shown while a query runs (DuckDB's `tools/shell/shell_progress_bar.cpp`
//! with `TerminalProgressBarDisplay::FormatProgressBar` / `FormatETA`): components such
//! as the percentage, the bar, the time remaining and the memory used, each a prompt.

use std::fmt::Write as _;

use rustdb::{Error, Result};

use crate::prompt::{PROMPT_SETTINGS, Prompt, PromptContext, PromptOptions};

/// The width and characters of the bar (`ProgressBarDisplayInfo`).
const PROGRESS_BAR_WIDTH: usize = 38;
const PROGRESS_EMPTY: &str = " ";
const PROGRESS_BLOCK: &str = "█";
const PROGRESS_START: &str = "▕";
const PROGRESS_END: &str = "▏";
/// `UnicodeBar::PartialBlocks`.
const PARTIAL_BLOCKS: [&str; 8] = [" ", "▏", "▎", "▍", "▌", "▋", "▊", "▉"];

/// `TerminalProgressBarDisplay::FormatProgressBar`.
pub fn format_progress_bar(percentage: i32) -> String {
    let blocks_to_draw = PROGRESS_BAR_WIDTH as f64 * (f64::from(percentage) / 100.0);
    let full_blocks = blocks_to_draw as usize;
    let mut result = String::from(PROGRESS_START);
    let mut i = 0;
    while i < full_blocks {
        result.push_str(PROGRESS_BLOCK);
        i += 1;
    }
    if i < PROGRESS_BAR_WIDTH {
        // A partial block for the remainder.
        let index = ((blocks_to_draw - full_blocks as f64) * PARTIAL_BLOCKS.len() as f64) as usize;
        result.push_str(PARTIAL_BLOCKS[index.min(PARTIAL_BLOCKS.len() - 1)]);
        i += 1;
    }
    while i < PROGRESS_BAR_WIDTH {
        result.push_str(PROGRESS_EMPTY);
        i += 1;
    }
    result.push_str(PROGRESS_END);
    result
}

/// `TerminalProgressBarDisplay::FormatETA`: the time remaining (or elapsed), padded to a
/// constant width.
#[allow(
    clippy::float_cmp,
    reason = "`INT_MAX` seconds is DuckDB's marker of an unknown time"
)]
pub fn format_eta(seconds: f64, elapsed: bool) -> String {
    const RENDER_SIZE: usize = 26;
    if seconds < 0.0 || seconds == 2_147_483_647.0 || (!elapsed && seconds > 3600.0 * 99.0) {
        // Unknown: render nothing.
        return " ".repeat(RENDER_SIZE);
    }
    let total_centiseconds = (seconds * 100.0).round() as u64;
    let total_seconds = total_centiseconds / 100;
    let centiseconds = total_centiseconds % 100;
    let hours = total_seconds / 3600;
    let minutes = (total_seconds % 3600) / 60;
    let secs = total_seconds % 60;
    let mut result = String::from("(");
    if elapsed {
        let _ = write!(
            result,
            "{hours:02}:{minutes:02}:{secs:02}.{centiseconds:02} elapsed"
        );
    } else {
        if hours == 0 && minutes == 0 && secs == 0 {
            result += "<1 second";
        } else if hours == 0 && minutes == 0 {
            let _ = write!(result, "~{secs} second{}", if secs > 1 { "s" } else { "" });
        } else if hours == 0 {
            let minute_fraction = (secs as f64 / 60.0 * 10.0) as u32;
            let _ = write!(result, "~{minutes}.{minute_fraction} minutes");
        } else {
            let hour_fraction = (minutes as f64 / 60.0 * 10.0) as u32;
            let _ = write!(result, "~{hours}.{hour_fraction} hours");
        }
        result += " remaining";
    }
    result.push(')');
    if result.len() < RENDER_SIZE {
        result += &" ".repeat(RENDER_SIZE - result.len());
    }
    result
}

/// Where a component goes, and where its content goes within its minimum size
/// (`ProgressBarAlignment`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProgressBarAlignment {
    Left,
    Middle,
    Right,
}

/// A component of the status line (`ProgressBarPrompt`).
#[derive(Clone, Debug)]
pub struct ProgressBarPrompt {
    prompt: Prompt,
    alignment: ProgressBarAlignment,
    content_alignment: ProgressBarAlignment,
    /// The component is hidden while its text contains this.
    does_not_contain: rustdb_common::ByteString,
    min_size: Option<usize>,
}

/// The options of a status line component (`ProgressBarPrompt::ParseSetting`).
#[derive(Debug)]
struct ProgressBarOptions {
    alignment: ProgressBarAlignment,
    content_alignment: ProgressBarAlignment,
    does_not_contain: rustdb_common::ByteString,
    min_size: Option<usize>,
}

impl PromptOptions for ProgressBarOptions {
    fn parse_setting(&mut self, _prompt: &mut Prompt, bracket_type: &[u8], value: &[u8]) -> Result<bool> {
        match bracket_type {
            b"align" => {
                self.alignment = match value {
                    b"right" => ProgressBarAlignment::Right,
                    b"left" => ProgressBarAlignment::Left,
                    _ => {
                        return Err(Error::invalid_input(rustdb_common::byte_format!(
                            "Unsupported type {value} for align: expected left or right"
                        )));
                    }
                };
            }
            b"content_align" => {
                self.content_alignment = match value {
                    b"right" => ProgressBarAlignment::Right,
                    b"left" => ProgressBarAlignment::Left,
                    b"middle" => ProgressBarAlignment::Middle,
                    _ => {
                        return Err(Error::invalid_input(rustdb_common::byte_format!(
                            "Unsupported type {value} for content_align: expected left or right"
                        )));
                    }
                };
            }
            b"hide_if_contains" => self.does_not_contain = value.into(),
            b"min_size" => self.min_size = Some(crate::prompt::parse_optional_index(value)?),
            _ => return Ok(false),
        }
        Ok(true)
    }

    fn supported_settings(&self) -> Vec<&'static str> {
        let mut supported = PROMPT_SETTINGS.to_vec();
        supported.extend(["progress_bar_percentage", "progress_bar", "eta"]);
        supported
    }
}

impl ProgressBarPrompt {
    /// `ParsePrompt` of a component.
    fn parse(text: &[u8]) -> Result<Self> {
        let mut options = ProgressBarOptions {
            alignment: ProgressBarAlignment::Left,
            content_alignment: ProgressBarAlignment::Left,
            does_not_contain: rustdb_common::ByteString::new(),
            min_size: None,
        };
        let prompt = Prompt::parse(text, &mut options)?;
        Ok(Self {
            prompt,
            alignment: options.alignment,
            content_alignment: options.content_alignment,
            does_not_contain: options.does_not_contain,
            min_size: options.min_size,
        })
    }

    /// `AddPaddingIfRequired`: pads the text to the minimum size.
    fn add_padding_if_required(&self, text: &mut rustdb_common::ByteString, render_length: &mut usize) {
        let Some(min_size) = self.min_size.filter(|&min_size| *render_length < min_size) else {
            return;
        };
        let padding_required = min_size - *render_length;
        let (left_padding, right_padding) = match self.content_alignment {
            ProgressBarAlignment::Left => (1, padding_required - 1),
            ProgressBarAlignment::Middle => (padding_required - padding_required / 2, padding_required / 2),
            ProgressBarAlignment::Right => (padding_required - 1, 1),
        };
        *text =
            rustdb_common::byte_format!("{}{text}{}", " ".repeat(left_padding), " ".repeat(right_padding));
        *render_length += padding_required;
    }
}

/// The progress the status line shows (the progress members of `ShellProgressBar`).
#[derive(Clone, Copy, Debug, Default)]
pub struct ProgressStatus {
    pub percentage: i32,
    pub estimated_remaining_seconds: f64,
}

/// The settings of a status line component: the progress, then those of the main prompt
/// (`ProgressBarPrompt::HandleSetting`).
struct ProgressBarContext<'a> {
    status: ProgressStatus,
    base: &'a mut dyn PromptContext,
}

impl PromptContext for ProgressBarContext<'_> {
    fn setting(&mut self, name: &[u8]) -> Result<rustdb_common::ByteString> {
        match name {
            b"progress_bar_percentage" => {
                let mut result = String::new();
                if self.status.percentage < 100 {
                    result.push(' ');
                }
                if self.status.percentage < 10 {
                    result.push(' ');
                }
                let _ = write!(result, "{}%", self.status.percentage);
                Ok(result.into())
            }
            b"progress_bar" => Ok(format_progress_bar(self.status.percentage).into()),
            b"eta" => Ok(format_eta(self.status.estimated_remaining_seconds, false).into()),
            _ => self.base.setting(name),
        }
    }

    fn execute_sql(&mut self, query: &[u8]) -> rustdb_common::ByteString {
        self.base.execute_sql(query)
    }
}

/// The components of the status line (`ShellProgressBar`).
#[derive(Clone, Debug, Default)]
pub struct ShellProgressBar {
    components: Vec<ProgressBarPrompt>,
}

impl ShellProgressBar {
    /// The components of `ShellState::Initialize`.
    pub fn with_default_components() -> Self {
        let mut progress_bar = Self::default();
        for component in [
            "{setting:progress_bar_percentage} {setting:progress_bar}{setting:eta}",
            "{align:right}{min_size:18}{hide_if_contains:0 bytes}Written: {setting:bytes_written}",
            "{align:right}{min_size:15}{hide_if_contains:0 bytes}Read: {setting:bytes_read}",
            "{align:right}{min_size:17}Memory: {setting:memory_usage}",
            "{align:right}{min_size:15}{hide_if_contains:0 bytes}Swap: {setting:swap_usage}",
        ] {
            let added = progress_bar.add_component(component);
            debug_assert!(added.is_ok());
        }
        progress_bar
    }

    /// `AddComponent`.
    pub fn add_component(&mut self, text: impl AsRef<[u8]>) -> Result<()> {
        self.components.push(ProgressBarPrompt::parse(text.as_ref())?);
        Ok(())
    }

    /// `ClearComponents`.
    pub fn clear_components(&mut self) {
        self.components.clear();
    }

    /// `GenerateProgressBar`: the components that fit in `terminal_width`, the
    /// right-aligned ones at the end.
    pub fn generate_progress_bar(
        &self,
        status: ProgressStatus,
        base: &mut dyn PromptContext,
        terminal_width: usize,
    ) -> Result<rustdb_common::ByteString> {
        let mut context = ProgressBarContext { status, base };
        let mut lhs = rustdb_common::ByteString::new();
        let mut rhs = rustdb_common::ByteString::new();
        let mut total_render_length = 0;
        for component in &self.components {
            let mut text = component.prompt.generate_prompt(&mut context)?;
            if !component.does_not_contain.is_empty()
                && text
                    .windows(component.does_not_contain.len())
                    .any(|part| part == component.does_not_contain.as_ref())
            {
                continue;
            }
            let mut render_length = crate::utf8::native_terminal_render_width(&text)?;
            component.add_padding_if_required(&mut text, &mut render_length);
            if total_render_length + render_length > terminal_width {
                break;
            }
            total_render_length += render_length;
            if component.alignment == ProgressBarAlignment::Left {
                lhs.push_bytes(&text);
            } else {
                rhs.push_bytes(&text);
            }
        }
        let mut result = lhs;
        if total_render_length < terminal_width {
            result.push_bytes(" ".repeat(terminal_width - total_render_length));
            result.push_bytes(&rhs);
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eta() {
        assert_eq!(format_eta(0.2, false), "(<1 second remaining)     ");
        assert_eq!(format_eta(75.0, false), "(~1.2 minutes remaining)  ");
        assert_eq!(format_eta(3.5, true), "(00:00:03.50 elapsed)     ");
        assert_eq!(format_progress_bar(100).chars().count(), PROGRESS_BAR_WIDTH + 2);
    }
}
