//! Dynamic prompts (DuckDB's `tools/shell/shell_prompt.cpp`): literal text mixed with
//! `{setting:...}`, `{sql:...}`, `{color:...}` and `{highlight_element:...}` components,
//! e.g. `{max_length:40}{color:green}{setting:current_database_and_schema}{color:reset} D `.

use rustdb::{Error, Result};
use rustdb_common::{ByteString, ErrorKind};

use crate::highlight::{
    self, HighlightElementType, PrintColor, PrintIntensity, RESET_TERMINAL_CODE, ShellHighlight,
};
use crate::output::{Output, PrintOutput};
use crate::state::ShellState;

/// `PromptComponentType`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptComponentType {
    Literal,
    Sql,
    SetColor,
    SetIntensity,
    SetHighlightElement,
    ResetColor,
    Setting,
}

/// `PromptComponent`.
#[derive(Clone, Debug)]
pub struct PromptComponent {
    pub kind: PromptComponentType,
    pub literal: ByteString,
    pub color: PrintColor,
    pub intensity: PrintIntensity,
    pub highlight_element: HighlightElementType,
}

impl PromptComponent {
    fn new(kind: PromptComponentType) -> Self {
        Self {
            kind,
            literal: ByteString::new(),
            color: PrintColor::STANDARD,
            intensity: PrintIntensity::Standard,
            highlight_element: HighlightElementType::None,
        }
    }
}

/// The values a prompt reads: settings and query results (`Prompt::HandleSetting` and
/// `Prompt::ExecuteSQL` with `Prompt::GetConnection`).
pub trait PromptContext {
    /// The value of `{setting:name}`.
    fn setting(&mut self, name: &[u8]) -> Result<rustdb_common::ByteString>;
    /// The single value `query` returns, or a `#...#` marker for other results.
    fn execute_sql(&mut self, query: &[u8]) -> ByteString;
}

/// The settings of the main prompt (`Prompt::GetSupportedSettings`).
pub const PROMPT_SETTINGS: [&str; 9] = [
    "current_database",
    "current_schema",
    "current_database_and_schema",
    "memory_limit",
    "memory_usage",
    "swap_usage",
    "swap_max",
    "bytes_written",
    "bytes_read",
];

/// What a bracket of the prompt sets besides its components: `max_length`, or for the
/// progress bar its alignment options (`Prompt::ParseSetting` and
/// `Prompt::GetSupportedSettings`, which the progress bar's prompts override).
pub trait PromptOptions {
    /// Applies `{bracket_type:value}`; `false` if it is not an option.
    fn parse_setting(&mut self, prompt: &mut Prompt, bracket_type: &[u8], value: &[u8]) -> Result<bool>;
    /// The names `{setting:...}` accepts.
    fn supported_settings(&self) -> Vec<&'static str>;
}

/// The options of the main prompt: `{max_length:N}`.
#[derive(Debug)]
pub struct MainPromptOptions;

impl PromptOptions for MainPromptOptions {
    fn parse_setting(&mut self, prompt: &mut Prompt, bracket_type: &[u8], value: &[u8]) -> Result<bool> {
        if bracket_type != b"max_length" {
            return Ok(false);
        }
        if value.is_empty() {
            return Err(Error::invalid_input("max_length requires a parameter"));
        }
        prompt.max_length = Some(parse_optional_index(value)?);
        Ok(true)
    }

    fn supported_settings(&self) -> Vec<&'static str> {
        PROMPT_SETTINGS.to_vec()
    }
}

/// `PromptParseState`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PromptParseState {
    Standard,
    ParseBracketType,
    ParseBracketContent,
    Escaped,
}

/// A parsed prompt (`Prompt`).
#[derive(Clone, Debug, Default)]
pub struct Prompt {
    pub components: Vec<PromptComponent>,
    pub max_length: Option<usize>,
}

/// `StringUtil::ToUnsigned` (`std::stoull`), followed by `optional_idx` validation.
pub fn parse_optional_index(text: &[u8]) -> Result<usize> {
    let text = text.trim_ascii_start();
    let (negative, digits) = match text.first() {
        Some(b'-') => (true, &text[1..]),
        Some(b'+') => (false, &text[1..]),
        _ => (false, text),
    };
    if digits.first().is_none_or(|c| !c.is_ascii_digit()) {
        return Err(unsigned_conversion_error("stoull: no conversion"));
    }
    let mut value = 0_u64;
    for &digit in digits.iter().take_while(|c| c.is_ascii_digit()) {
        value = value.checked_mul(10)
            .and_then(|value| value.checked_add(u64::from(digit - b'0')))
            .ok_or_else(|| unsigned_conversion_error("stoull: out of range"))?;
    }
    if negative {
        value = 0_u64.wrapping_sub(value);
    }
    if value == u64::MAX {
        return Err(Error::internal("optional_idx cannot be initialized with an invalid index"));
    }
    usize::try_from(value).map_err(|_| unsigned_conversion_error("stoull: out of range"))
}

fn unsigned_conversion_error(libcxx_message: &'static str) -> Error {
    // libstdc++ reports only the conversion name for both invalid_argument and
    // out_of_range. Apple's libc++ includes the reason in the message.
    let message = if cfg!(target_env = "gnu") {
        "stoull"
    } else {
        libcxx_message
    };
    Error::new(ErrorKind::Invalid, message)
}

impl Prompt {
    /// The default prompt of `ShellState::Initialize`.
    pub fn default_main_prompt() -> Self {
        Self::parse(
            "{max_length:40}{highlight_element:prompt}{setting:current_database_and_schema}{color:reset} D ",
            &mut MainPromptOptions,
        )
        .unwrap_or_default()
    }

    /// `AddLiteral`.
    fn add_literal(&mut self, text: &[u8]) {
        if self
            .components
            .last()
            .is_none_or(|last| last.kind != PromptComponentType::Literal)
        {
            self.components
                .push(PromptComponent::new(PromptComponentType::Literal));
        }
        if let Some(last) = self.components.last_mut() {
            last.literal.push_bytes(text);
        }
    }

    /// `AddComponent`.
    fn add_component(
        &mut self,
        options: &mut dyn PromptOptions,
        bracket_type: &[u8],
        value: &[u8],
    ) -> Result<()> {
        let requires_parameter = || Error::invalid_input(rustdb_common::byte_format!("{bracket_type} requires a parameter"));
        let component = match bracket_type {
            b"setting" => {
                if value.is_empty() {
                    return Err(requires_parameter());
                }
                let supported_settings = options.supported_settings();
                if !supported_settings.iter().any(|setting| setting.as_bytes() == value) {
                    return Err(Error::invalid_input(rustdb_common::byte_format!(
                        "unsupported setting \"{value}\" for setting, supported values: {}",
                        supported_settings.join(", ")
                    )));
                }
                let mut component = PromptComponent::new(PromptComponentType::Setting);
                component.literal = value.into();
                component
            }
            b"sql" => {
                if value.is_empty() {
                    return Err(requires_parameter());
                }
                let mut component = PromptComponent::new(PromptComponentType::Sql);
                component.literal = value.into();
                component
            }
            b"color" => {
                if value.is_empty() {
                    return Err(requires_parameter());
                }
                match value {
                    b"bold" | b"underline" => {
                        let mut component = PromptComponent::new(PromptComponentType::SetIntensity);
                        component.intensity = if value == b"bold" {
                            PrintIntensity::Bold
                        } else {
                            PrintIntensity::Underline
                        };
                        component
                    }
                    b"reset" => PromptComponent::new(PromptComponentType::ResetColor),
                    _ => {
                        let mut component = PromptComponent::new(PromptComponentType::SetColor);
                        component.color =
                            highlight::try_get_print_color(value).map_err(Error::invalid_input)?;
                        component
                    }
                }
            }
            b"highlight_element" => {
                if value.is_empty() {
                    return Err(requires_parameter());
                }
                let mut component = PromptComponent::new(PromptComponentType::SetHighlightElement);
                component.highlight_element =
                    highlight::try_get_highlight_element(value).map_err(Error::invalid_input)?;
                component
            }
            _ => {
                if options.parse_setting(self, bracket_type, value)? {
                    return Ok(());
                }
                return Err(Error::invalid_input(rustdb_common::byte_format!(
                    "Unknown bracket type {bracket_type}"
                )));
            }
        };
        self.components.push(component);
        Ok(())
    }

    /// `ParsePrompt`, with `options` taking the option brackets.
    pub fn parse(text: impl AsRef<[u8]>, options: &mut dyn PromptOptions) -> Result<Self> {
        let text = text.as_ref();
        let mut prompt = Self::default();
        let mut parse_state = PromptParseState::Standard;
        let mut prev_state = parse_state;
        let mut bracket_type = ByteString::new();
        let mut literal = ByteString::new();
        for &c in text {
            match parse_state {
                PromptParseState::Standard => match c {
                    b'\\' => {
                        prev_state = parse_state;
                        parse_state = PromptParseState::Escaped;
                    }
                    b'{' => {
                        if !literal.is_empty() {
                            prompt.add_literal(&literal);
                            literal.clear();
                        }
                        parse_state = PromptParseState::ParseBracketType;
                    }
                    _ => literal.push(c),
                },
                PromptParseState::Escaped => {
                    literal.push(c);
                    parse_state = prev_state;
                }
                PromptParseState::ParseBracketType => match c {
                    b'}' => {
                        prompt.add_component(options, &literal, b"")?;
                        literal.clear();
                        parse_state = PromptParseState::Standard;
                    }
                    b':' => {
                        bracket_type = std::mem::take(&mut literal);
                        parse_state = PromptParseState::ParseBracketContent;
                    }
                    b'\\' => {
                        prev_state = parse_state;
                        parse_state = PromptParseState::Escaped;
                    }
                    _ => literal.push(c),
                },
                PromptParseState::ParseBracketContent => match c {
                    b'}' => {
                        prompt.add_component(options, &bracket_type, &literal)?;
                        bracket_type.clear();
                        literal.clear();
                        parse_state = PromptParseState::Standard;
                    }
                    b'\\' => {
                        prev_state = parse_state;
                        parse_state = PromptParseState::Escaped;
                    }
                    _ => literal.push(c),
                },
            }
        }
        if parse_state != PromptParseState::Standard {
            return Err(Error::invalid_input(rustdb_common::byte_format!(
                "Failed to parse prompt \"{text}\" - unterminated bracket or escape"
            )));
        }
        if !literal.is_empty() {
            prompt.add_literal(&literal);
        }
        Ok(prompt)
    }

    /// `HandleColor`: the terminal code of a color component, if highlighting is on.
    fn handle_color(component: &PromptComponent) -> String {
        if !highlight::is_enabled() {
            return String::new();
        }
        match component.kind {
            PromptComponentType::SetColor => {
                highlight::terminal_code(component.color, PrintIntensity::Standard)
            }
            PromptComponentType::SetIntensity => {
                highlight::terminal_code(PrintColor::STANDARD, component.intensity)
            }
            PromptComponentType::SetHighlightElement => {
                let element = highlight::highlight_element(component.highlight_element);
                highlight::terminal_code(element.color, element.intensity)
            }
            _ => RESET_TERMINAL_CODE.to_owned(),
        }
    }

    /// `HandleText`: text that keeps the prompt within `max_length` columns, ending in
    /// `... D ` where it is cut.
    fn handle_text(&self, text: impl AsRef<[u8]>, length: &mut usize) -> Result<ByteString> {
        let text = text.as_ref();
        let Some(max_length) = self.max_length else {
            return Ok(text.into());
        };
        if *length > max_length {
            // Already exceeded.
            return Ok(ByteString::new());
        }
        let render_length = crate::utf8::native_terminal_render_width(text)?;
        if *length + render_length <= max_length {
            *length += render_length;
            return Ok(text.into());
        }
        // Render the characters that fit.
        let bytes = text;
        let mut start_pos = 0;
        let mut truncated_text = rustdb_common::ByteString::new();
        for i in 1..=bytes.len() {
            if i < bytes.len() && !ShellState::is_character(bytes[i]) {
                continue;
            }
            let prev_character = &text[start_pos..i];
            let char_length = crate::utf8::native_terminal_render_width(prev_character)?;
            if *length + char_length > max_length {
                break;
            }
            truncated_text.push_bytes(prev_character);
            *length += char_length;
            start_pos = i;
        }
        truncated_text.push_bytes(b"... D ");
        *length += 6;
        Ok(truncated_text)
    }

    /// `GeneratePrompt`: the prompt with terminal color codes.
    pub fn generate_prompt(&self, context: &mut dyn PromptContext) -> Result<rustdb_common::ByteString> {
        let mut prompt = rustdb_common::ByteString::new();
        let mut length = 0;
        for component in &self.components {
            match component.kind {
                PromptComponentType::Literal => {
                    prompt.push_bytes(self.handle_text(&component.literal, &mut length)?);
                }
                PromptComponentType::Sql => {
                    let result = context.execute_sql(&component.literal);
                    prompt.push_bytes(self.handle_text(&result, &mut length)?);
                }
                PromptComponentType::Setting => {
                    let value = context.setting(&component.literal)?;
                    prompt.push_bytes(self.handle_text(&value, &mut length)?);
                }
                PromptComponentType::SetColor
                | PromptComponentType::SetHighlightElement
                | PromptComponentType::SetIntensity
                | PromptComponentType::ResetColor => prompt.push_bytes(Self::handle_color(component)),
            }
        }
        Ok(prompt)
    }

    /// `PrintPrompt`: the prompt printed piece by piece with its highlighting.
    pub fn print_prompt(
        &self,
        context: &mut dyn PromptContext,
        out: &Output,
        output: PrintOutput,
    ) -> Result<()> {
        let highlight = ShellHighlight::new(out);
        let mut color = PrintColor::STANDARD;
        let mut intensity = PrintIntensity::Standard;
        let mut length = 0;
        for component in &self.components {
            match component.kind {
                PromptComponentType::Literal => {
                    let text = self.handle_text(&component.literal, &mut length)?;
                    highlight.print_bytes_colored(&text, output, color, intensity);
                }
                PromptComponentType::Sql => {
                    let result = context.execute_sql(&component.literal);
                    let text = self.handle_text(&result, &mut length)?;
                    highlight.print_bytes_colored(&text, output, color, intensity);
                }
                PromptComponentType::Setting => {
                    let result = context.setting(&component.literal)?;
                    let text = self.handle_text(&result, &mut length)?;
                    highlight.print_bytes_colored(&text, output, color, intensity);
                }
                PromptComponentType::SetColor => color = component.color,
                PromptComponentType::SetHighlightElement => {
                    let element = highlight::highlight_element(component.highlight_element);
                    color = element.color;
                    intensity = element.intensity;
                }
                PromptComponentType::SetIntensity => intensity = component.intensity,
                PromptComponentType::ResetColor => {
                    color = PrintColor::STANDARD;
                    intensity = PrintIntensity::Standard;
                }
            }
        }
        Ok(())
    }
}
