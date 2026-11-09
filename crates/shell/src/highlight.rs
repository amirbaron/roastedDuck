//! Highlighting of the shell's output (DuckDB's `tools/shell/shell_highlight.cpp`): the
//! colors of the elements (keywords, layout, errors, ...), terminal color codes, and
//! highlighted error messages.

use std::fmt::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex, PoisonError};

use roastedduck_parser::highlight::{HighlightToken, HighlightTokenType, tokenize_error};

use crate::output::{Output, PrintOutput};

/// The intensity of printed text (`PrintIntensity`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrintIntensity {
    Standard,
    Bold,
    Underline,
    BoldUnderline,
}

/// A terminal color (`PrintColor`): one of the 256 colors of most terminals, of which
/// 0 to 15 are the classic ones, or the terminal's standard color.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PrintColor(u16);

impl PrintColor {
    pub const STANDARD: Self = Self(256);
    pub const RED: Self = Self(1);
    pub const GREEN: Self = Self(2);
    pub const YELLOW: Self = Self(3);
    pub const BLUE: Self = Self(4);
    pub const GRAY: Self = Self(8);
    pub const DEEPSKYBLUE6: Self = Self(25);
    pub const DODGERBLUE2: Self = Self(27);
    pub const DODGERBLUE1: Self = Self(33);
    pub const DEEPSKYBLUE1: Self = Self(39);
    pub const ORANGE4: Self = Self(58);
    pub const DARKMAGENTA: Self = Self(90);
    pub const RED3: Self = Self(124);
    pub const DARKORANGE4: Self = Self(166);
    pub const ORANGE3: Self = Self(172);
    pub const INDIANRED3: Self = Self(203);
    pub const DARKORANGE: Self = Self(208);
    pub const ORCHID2: Self = Self(212);
    pub const GOLD1: Self = Self(220);

    /// The color with terminal code `code`.
    pub const fn from_code(code: u8) -> Self {
        Self(code as u16)
    }

    /// The terminal code of the color, `None` for the standard color.
    pub fn code(self) -> Option<u8> {
        u8::try_from(self.0).ok()
    }
}

/// An element of the output that is highlighted (`HighlightElementType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HighlightElementType {
    ErrorToken = 0,
    Keyword,
    NumericConstant,
    StringConstant,
    LineIndicator,
    DatabaseName,
    SchemaName,
    TableName,
    ColumnName,
    ColumnType,
    NumericValue,
    StringValue,
    TemporalValue,
    NullValue,
    Footer,
    Layout,
    StartupText,
    StartupVersion,
    Continuation,
    ContinuationSelected,
    Bracket,
    Comment,
    SuggestionCatalogName,
    SuggestionSchemaName,
    SuggestionTableName,
    SuggestionColumnName,
    SuggestionFileName,
    SuggestionDirectoryName,
    SuggestionFunctionName,
    SuggestionSettingName,
    TableLayout,
    ViewLayout,
    PrimaryKeyColumn,
    Prompt,
    ErrorEmphasis,
    ErrorSuggestion,
    LogTrace,
    LogDebug,
    LogInfo,
    LogWarning,
    None,
}

/// The elements in the order of their configuration.
const ELEMENT_TYPES: [HighlightElementType; 41] = {
    use HighlightElementType as H;
    [
        H::ErrorToken,
        H::Keyword,
        H::NumericConstant,
        H::StringConstant,
        H::LineIndicator,
        H::DatabaseName,
        H::SchemaName,
        H::TableName,
        H::ColumnName,
        H::ColumnType,
        H::NumericValue,
        H::StringValue,
        H::TemporalValue,
        H::NullValue,
        H::Footer,
        H::Layout,
        H::StartupText,
        H::StartupVersion,
        H::Continuation,
        H::ContinuationSelected,
        H::Bracket,
        H::Comment,
        H::SuggestionCatalogName,
        H::SuggestionSchemaName,
        H::SuggestionTableName,
        H::SuggestionColumnName,
        H::SuggestionFileName,
        H::SuggestionDirectoryName,
        H::SuggestionFunctionName,
        H::SuggestionSettingName,
        H::TableLayout,
        H::ViewLayout,
        H::PrimaryKeyColumn,
        H::Prompt,
        H::ErrorEmphasis,
        H::ErrorSuggestion,
        H::LogTrace,
        H::LogDebug,
        H::LogInfo,
        H::LogWarning,
        H::None,
    ]
};

/// How an element is highlighted (`HighlightElement`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HighlightElement {
    pub name: &'static str,
    pub color: PrintColor,
    pub intensity: PrintIntensity,
}

const fn element(name: &'static str, color: PrintColor, intensity: PrintIntensity) -> HighlightElement {
    HighlightElement {
        name,
        color,
        intensity,
    }
}

/// The default highlighting of every element (`highlight_elements`).
const DEFAULT_ELEMENTS: [HighlightElement; 41] = {
    use PrintColor as C;
    use PrintIntensity::{Bold, Standard, Underline};
    [
        element("error", C::RED, Standard),
        element("keyword", C::GREEN, Standard),
        element("numeric_constant", C::YELLOW, Standard),
        element("string_constant", C::YELLOW, Standard),
        element("line_indicator", C::STANDARD, Bold),
        element("database_name", C::ORANGE3, Standard),
        element("schema_name", C::DEEPSKYBLUE1, Standard),
        element("table_name", C::STANDARD, Bold),
        element("column_name", C::STANDARD, Standard),
        element("column_type", C::GRAY, Standard),
        element("numeric_value", C::STANDARD, Standard),
        element("string_value", C::STANDARD, Standard),
        element("temporal_value", C::STANDARD, Standard),
        element("null_value", C::GRAY, Standard),
        element("footer", C::GRAY, Standard),
        element("layout", C::GRAY, Standard),
        element("startup_text", C::GRAY, Standard),
        element("startup_version", C::STANDARD, Standard),
        element("continuation", C::GRAY, Standard),
        element("continuation_selected", C::GREEN, Standard),
        element("bracket", C::STANDARD, Underline),
        element("comment", C::GRAY, Standard),
        element("suggestion_catalog_name", C::ORANGE3, Standard),
        element("suggestion_schema_name", C::DEEPSKYBLUE1, Standard),
        element("suggestion_table_name", C::STANDARD, Standard),
        element("suggestion_column_name", C::STANDARD, Standard),
        element("suggestion_file_name", C::STANDARD, Standard),
        element("suggestion_directory_name", C::STANDARD, Bold),
        element("suggestion_function_name", C::STANDARD, Standard),
        element("suggestion_setting_name", C::STANDARD, Standard),
        element("table_layout", C::GRAY, Standard),
        element("view_layout", C::STANDARD, Standard),
        element("primary_key_column", C::STANDARD, Underline),
        element("prompt", C::DARKORANGE, Bold),
        element("error_emphasis", C::RED, Bold),
        element("error_suggestion", C::RED, Bold),
        element("log_trace", C::BLUE, Bold),
        element("log_debug", C::YELLOW, Bold),
        element("log_info", C::GREEN, Bold),
        element("log_warning", C::GRAY, Standard),
        element("none", C::STANDARD, Standard),
    ]
};

/// The highlighting of the elements as configured (the mutable `highlight_elements`),
/// and which elements the user configured (`ShellState::user_configured_elements`).
#[derive(Debug)]
struct HighlightConfig {
    elements: [HighlightElement; 41],
    user_configured: Vec<HighlightElementType>,
}

static CONFIG: LazyLock<Mutex<HighlightConfig>> = LazyLock::new(|| {
    Mutex::new(HighlightConfig {
        elements: DEFAULT_ELEMENTS,
        user_configured: Vec::new(),
    })
});

/// `ShellState::highlighting_enabled`.
static ENABLED: AtomicBool = AtomicBool::new(true);

/// How the color scheme is chosen (`HighlightMode`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HighlightMode {
    Automatic,
    MixedMode,
    DarkMode,
    LightMode,
}

/// A color of the terminal palette (`HighlightColorInfo`).
#[derive(Clone, Copy, Debug)]
pub struct ColorInfo {
    pub color_name: &'static str,
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl ColorInfo {
    const fn new(color_name: &'static str, r: u8, g: u8, b: u8) -> Self {
        Self { color_name, r, g, b }
    }
}

/// The names and values of the 256 colors, by code (`highlight_color_info`).
pub const COLOR_INFO: [ColorInfo; 256] = [
    ColorInfo::new("black", 0x00, 0x00, 0x00),
    ColorInfo::new("red", 0x80, 0x00, 0x00),
    ColorInfo::new("green", 0x00, 0x80, 0x00),
    ColorInfo::new("yellow", 0x80, 0x80, 0x00),
    ColorInfo::new("blue", 0x00, 0x00, 0x80),
    ColorInfo::new("magenta", 0x80, 0x00, 0x80),
    ColorInfo::new("cyan", 0x00, 0x80, 0x80),
    ColorInfo::new("brightgray", 0xC0, 0xC0, 0xC0),
    ColorInfo::new("gray", 0x80, 0x80, 0x80),
    ColorInfo::new("brightred", 0xFF, 0x00, 0x00),
    ColorInfo::new("brightgreen", 0x00, 0xFF, 0x00),
    ColorInfo::new("brightyellow", 0xFF, 0xFF, 0x00),
    ColorInfo::new("brightblue", 0x00, 0x00, 0xFF),
    ColorInfo::new("brightmagenta", 0xFF, 0x00, 0xFF),
    ColorInfo::new("brightcyan", 0x00, 0xFF, 0xFF),
    ColorInfo::new("white", 0xFF, 0xFF, 0xFF),
    ColorInfo::new("grey0", 0x00, 0x00, 0x00),
    ColorInfo::new("navyblue", 0x00, 0x00, 0x5F),
    ColorInfo::new("darkblue", 0x00, 0x00, 0x87),
    ColorInfo::new("blue3", 0x00, 0x00, 0xAF),
    ColorInfo::new("blue4", 0x00, 0x00, 0xD7),
    ColorInfo::new("blue1", 0x00, 0x00, 0xFF),
    ColorInfo::new("darkgreen", 0x00, 0x5F, 0x00),
    ColorInfo::new("deepskyblue4", 0x00, 0x5F, 0x5F),
    ColorInfo::new("deepskyblue5", 0x00, 0x5F, 0x87),
    ColorInfo::new("deepskyblue6", 0x00, 0x5F, 0xAF),
    ColorInfo::new("dodgerblue3", 0x00, 0x5F, 0xD7),
    ColorInfo::new("dodgerblue2", 0x00, 0x5F, 0xFF),
    ColorInfo::new("green4", 0x00, 0x87, 0x00),
    ColorInfo::new("springgreen4", 0x00, 0x87, 0x5F),
    ColorInfo::new("turquoise4", 0x00, 0x87, 0x87),
    ColorInfo::new("deepskyblue3", 0x00, 0x87, 0xAF),
    ColorInfo::new("deepskyblue7", 0x00, 0x87, 0xD7),
    ColorInfo::new("dodgerblue1", 0x00, 0x87, 0xFF),
    ColorInfo::new("green3", 0x00, 0xAF, 0x00),
    ColorInfo::new("springgreen3", 0x00, 0xAF, 0x5F),
    ColorInfo::new("darkcyan", 0x00, 0xAF, 0x87),
    ColorInfo::new("lightseagreen", 0x00, 0xAF, 0xAF),
    ColorInfo::new("deepskyblue2", 0x00, 0xAF, 0xD7),
    ColorInfo::new("deepskyblue1", 0x00, 0xAF, 0xFF),
    ColorInfo::new("green5", 0x00, 0xD7, 0x00),
    ColorInfo::new("springgreen5", 0x00, 0xD7, 0x5F),
    ColorInfo::new("springgreen2", 0x00, 0xD7, 0x87),
    ColorInfo::new("cyan3", 0x00, 0xD7, 0xAF),
    ColorInfo::new("darkturquoise", 0x00, 0xD7, 0xD7),
    ColorInfo::new("turquoise2", 0x00, 0xD7, 0xFF),
    ColorInfo::new("green1", 0x00, 0xFF, 0x00),
    ColorInfo::new("springgreen6", 0x00, 0xFF, 0x5F),
    ColorInfo::new("springgreen1", 0x00, 0xFF, 0x87),
    ColorInfo::new("mediumspringgreen", 0x00, 0xFF, 0xAF),
    ColorInfo::new("cyan2", 0x00, 0xFF, 0xD7),
    ColorInfo::new("cyan1", 0x00, 0xFF, 0xFF),
    ColorInfo::new("darkred1", 0x5F, 0x00, 0x00),
    ColorInfo::new("deeppink4", 0x5F, 0x00, 0x5F),
    ColorInfo::new("purple4", 0x5F, 0x00, 0x87),
    ColorInfo::new("purple5", 0x5F, 0x00, 0xAF),
    ColorInfo::new("purple3", 0x5F, 0x00, 0xD7),
    ColorInfo::new("blueviolet", 0x5F, 0x00, 0xFF),
    ColorInfo::new("orange4", 0x5F, 0x5F, 0x00),
    ColorInfo::new("grey37", 0x5F, 0x5F, 0x5F),
    ColorInfo::new("mediumpurple4", 0x5F, 0x5F, 0x87),
    ColorInfo::new("slateblue3", 0x5F, 0x5F, 0xAF),
    ColorInfo::new("slateblue4", 0x5F, 0x5F, 0xD7),
    ColorInfo::new("royalblue1", 0x5F, 0x5F, 0xFF),
    ColorInfo::new("chartreuse4", 0x5F, 0x87, 0x00),
    ColorInfo::new("darkseagreen4", 0x5F, 0x87, 0x5F),
    ColorInfo::new("paleturquoise4", 0x5F, 0x87, 0x87),
    ColorInfo::new("steelblue", 0x5F, 0x87, 0xAF),
    ColorInfo::new("steelblue3", 0x5F, 0x87, 0xD7),
    ColorInfo::new("cornflowerblue", 0x5F, 0x87, 0xFF),
    ColorInfo::new("chartreuse3", 0x5F, 0xAF, 0x00),
    ColorInfo::new("darkseagreen5", 0x5F, 0xAF, 0x5F),
    ColorInfo::new("cadetblue", 0x5F, 0xAF, 0x87),
    ColorInfo::new("cadetblue2", 0x5F, 0xAF, 0xAF),
    ColorInfo::new("skyblue3", 0x5F, 0xAF, 0xD7),
    ColorInfo::new("steelblue1", 0x5F, 0xAF, 0xFF),
    ColorInfo::new("chartreuse5", 0x5F, 0xD7, 0x00),
    ColorInfo::new("palegreen3", 0x5F, 0xD7, 0x5F),
    ColorInfo::new("seagreen3", 0x5F, 0xD7, 0x87),
    ColorInfo::new("aquamarine3", 0x5F, 0xD7, 0xAF),
    ColorInfo::new("mediumturquoise", 0x5F, 0xD7, 0xD7),
    ColorInfo::new("steelblue2", 0x5F, 0xD7, 0xFF),
    ColorInfo::new("chartreuse2", 0x5F, 0xFF, 0x00),
    ColorInfo::new("seagreen2", 0x5F, 0xFF, 0x5F),
    ColorInfo::new("seagreen1", 0x5F, 0xFF, 0x87),
    ColorInfo::new("seagreen4", 0x5F, 0xFF, 0xAF),
    ColorInfo::new("aquamarine1", 0x5F, 0xFF, 0xD7),
    ColorInfo::new("darkslategray2", 0x5F, 0xFF, 0xFF),
    ColorInfo::new("darkred2", 0x87, 0x00, 0x00),
    ColorInfo::new("deeppink5", 0x87, 0x00, 0x5F),
    ColorInfo::new("darkmagenta", 0x87, 0x00, 0x87),
    ColorInfo::new("darkmagenta2", 0x87, 0x00, 0xAF),
    ColorInfo::new("darkviolet1", 0x87, 0x00, 0xD7),
    ColorInfo::new("purple2", 0x87, 0x00, 0xFF),
    ColorInfo::new("orange5", 0x87, 0x5F, 0x00),
    ColorInfo::new("lightpink4", 0x87, 0x5F, 0x5F),
    ColorInfo::new("plum4", 0x87, 0x5F, 0x87),
    ColorInfo::new("mediumpurple3", 0x87, 0x5F, 0xAF),
    ColorInfo::new("mediumpurple5", 0x87, 0x5F, 0xD7),
    ColorInfo::new("slateblue1", 0x87, 0x5F, 0xFF),
    ColorInfo::new("yellow4", 0x87, 0x87, 0x00),
    ColorInfo::new("wheat4", 0x87, 0x87, 0x5F),
    ColorInfo::new("grey53", 0x87, 0x87, 0x87),
    ColorInfo::new("lightslategrey", 0x87, 0x87, 0xAF),
    ColorInfo::new("mediumpurple", 0x87, 0x87, 0xD7),
    ColorInfo::new("lightslateblue", 0x87, 0x87, 0xFF),
    ColorInfo::new("yellow5", 0x87, 0xAF, 0x00),
    ColorInfo::new("darkolivegreen3", 0x87, 0xAF, 0x5F),
    ColorInfo::new("darkseagreen", 0x87, 0xAF, 0x87),
    ColorInfo::new("lightskyblue3", 0x87, 0xAF, 0xAF),
    ColorInfo::new("lightskyblue4", 0x87, 0xAF, 0xD7),
    ColorInfo::new("skyblue2", 0x87, 0xAF, 0xFF),
    ColorInfo::new("chartreuse6", 0x87, 0xD7, 0x00),
    ColorInfo::new("darkolivegreen4", 0x87, 0xD7, 0x5F),
    ColorInfo::new("palegreen4", 0x87, 0xD7, 0x87),
    ColorInfo::new("darkseagreen3", 0x87, 0xD7, 0xAF),
    ColorInfo::new("darkslategray3", 0x87, 0xD7, 0xD7),
    ColorInfo::new("skyblue1", 0x87, 0xD7, 0xFF),
    ColorInfo::new("chartreuse1", 0x87, 0xFF, 0x00),
    ColorInfo::new("lightgreen1", 0x87, 0xFF, 0x5F),
    ColorInfo::new("lightgreen2", 0x87, 0xFF, 0x87),
    ColorInfo::new("palegreen1", 0x87, 0xFF, 0xAF),
    ColorInfo::new("aquamarine2", 0x87, 0xFF, 0xD7),
    ColorInfo::new("darkslategray1", 0x87, 0xFF, 0xFF),
    ColorInfo::new("red3", 0xAF, 0x00, 0x00),
    ColorInfo::new("deeppink6", 0xAF, 0x00, 0x5F),
    ColorInfo::new("mediumvioletred", 0xAF, 0x00, 0x87),
    ColorInfo::new("magenta3", 0xAF, 0x00, 0xAF),
    ColorInfo::new("darkviolet2", 0xAF, 0x00, 0xD7),
    ColorInfo::new("purple6", 0xAF, 0x00, 0xFF),
    ColorInfo::new("darkorange3", 0xAF, 0x5F, 0x00),
    ColorInfo::new("indianred1", 0xAF, 0x5F, 0x5F),
    ColorInfo::new("hotpink3", 0xAF, 0x5F, 0x87),
    ColorInfo::new("mediumorchid3", 0xAF, 0x5F, 0xAF),
    ColorInfo::new("mediumorchid", 0xAF, 0x5F, 0xD7),
    ColorInfo::new("mediumpurple2", 0xAF, 0x5F, 0xFF),
    ColorInfo::new("darkgoldenrod", 0xAF, 0x87, 0x00),
    ColorInfo::new("lightsalmon3", 0xAF, 0x87, 0x5F),
    ColorInfo::new("rosybrown", 0xAF, 0x87, 0x87),
    ColorInfo::new("grey63", 0xAF, 0x87, 0xAF),
    ColorInfo::new("mediumpurple6", 0xAF, 0x87, 0xD7),
    ColorInfo::new("mediumpurple1", 0xAF, 0x87, 0xFF),
    ColorInfo::new("gold3", 0xAF, 0xAF, 0x00),
    ColorInfo::new("darkkhaki", 0xAF, 0xAF, 0x5F),
    ColorInfo::new("navajowhite3", 0xAF, 0xAF, 0x87),
    ColorInfo::new("grey69", 0xAF, 0xAF, 0xAF),
    ColorInfo::new("lightsteelblue3", 0xAF, 0xAF, 0xD7),
    ColorInfo::new("lightsteelblue", 0xAF, 0xAF, 0xFF),
    ColorInfo::new("yellow3", 0xAF, 0xD7, 0x00),
    ColorInfo::new("darkolivegreen5", 0xAF, 0xD7, 0x5F),
    ColorInfo::new("darkseagreen6", 0xAF, 0xD7, 0x87),
    ColorInfo::new("darkseagreen2", 0xAF, 0xD7, 0xAF),
    ColorInfo::new("lightcyan3", 0xAF, 0xD7, 0xD7),
    ColorInfo::new("lightskyblue1", 0xAF, 0xD7, 0xFF),
    ColorInfo::new("greenyellow", 0xAF, 0xFF, 0x00),
    ColorInfo::new("darkolivegreen2", 0xAF, 0xFF, 0x5F),
    ColorInfo::new("palegreen2", 0xAF, 0xFF, 0x87),
    ColorInfo::new("darkseagreen7", 0xAF, 0xFF, 0xAF),
    ColorInfo::new("darkseagreen1", 0xAF, 0xFF, 0xD7),
    ColorInfo::new("paleturquoise1", 0xAF, 0xFF, 0xFF),
    ColorInfo::new("red4", 0xD7, 0x00, 0x00),
    ColorInfo::new("deeppink3", 0xD7, 0x00, 0x5F),
    ColorInfo::new("deeppink7", 0xD7, 0x00, 0x87),
    ColorInfo::new("magenta4", 0xD7, 0x00, 0xAF),
    ColorInfo::new("magenta5", 0xD7, 0x00, 0xD7),
    ColorInfo::new("magenta2", 0xD7, 0x00, 0xFF),
    ColorInfo::new("darkorange4", 0xD7, 0x5F, 0x00),
    ColorInfo::new("indianred2", 0xD7, 0x5F, 0x5F),
    ColorInfo::new("hotpink4", 0xD7, 0x5F, 0x87),
    ColorInfo::new("hotpink2", 0xD7, 0x5F, 0xAF),
    ColorInfo::new("orchid", 0xD7, 0x5F, 0xD7),
    ColorInfo::new("mediumorchid1", 0xD7, 0x5F, 0xFF),
    ColorInfo::new("orange3", 0xD7, 0x87, 0x00),
    ColorInfo::new("lightsalmon4", 0xD7, 0x87, 0x5F),
    ColorInfo::new("lightpink3", 0xD7, 0x87, 0x87),
    ColorInfo::new("pink3", 0xD7, 0x87, 0xAF),
    ColorInfo::new("plum3", 0xD7, 0x87, 0xD7),
    ColorInfo::new("violet", 0xD7, 0x87, 0xFF),
    ColorInfo::new("gold4", 0xD7, 0xAF, 0x00),
    ColorInfo::new("lightgoldenrod3", 0xD7, 0xAF, 0x5F),
    ColorInfo::new("tan", 0xD7, 0xAF, 0x87),
    ColorInfo::new("mistyrose3", 0xD7, 0xAF, 0xAF),
    ColorInfo::new("thistle3", 0xD7, 0xAF, 0xD7),
    ColorInfo::new("plum2", 0xD7, 0xAF, 0xFF),
    ColorInfo::new("yellow6", 0xD7, 0xD7, 0x00),
    ColorInfo::new("khaki3", 0xD7, 0xD7, 0x5F),
    ColorInfo::new("lightgoldenrod2", 0xD7, 0xD7, 0x87),
    ColorInfo::new("lightyellow3", 0xD7, 0xD7, 0xAF),
    ColorInfo::new("grey84", 0xD7, 0xD7, 0xD7),
    ColorInfo::new("lightsteelblue1", 0xD7, 0xD7, 0xFF),
    ColorInfo::new("yellow2", 0xD7, 0xFF, 0x00),
    ColorInfo::new("darkolivegreen1", 0xD7, 0xFF, 0x5F),
    ColorInfo::new("darkolivegreen6", 0xD7, 0xFF, 0x87),
    ColorInfo::new("darkseagreen8", 0xD7, 0xFF, 0xAF),
    ColorInfo::new("honeydew2", 0xD7, 0xFF, 0xD7),
    ColorInfo::new("lightcyan1", 0xD7, 0xFF, 0xFF),
    ColorInfo::new("red1", 0xFF, 0x00, 0x00),
    ColorInfo::new("deeppink2", 0xFF, 0x00, 0x5F),
    ColorInfo::new("deeppink1", 0xFF, 0x00, 0x87),
    ColorInfo::new("deeppink8", 0xFF, 0x00, 0xAF),
    ColorInfo::new("magenta6", 0xFF, 0x00, 0xD7),
    ColorInfo::new("magenta1", 0xFF, 0x00, 0xFF),
    ColorInfo::new("orangered1", 0xFF, 0x5F, 0x00),
    ColorInfo::new("indianred3", 0xFF, 0x5F, 0x5F),
    ColorInfo::new("indianred4", 0xFF, 0x5F, 0x87),
    ColorInfo::new("hotpink", 0xFF, 0x5F, 0xAF),
    ColorInfo::new("hotpink5", 0xFF, 0x5F, 0xD7),
    ColorInfo::new("mediumorchid2", 0xFF, 0x5F, 0xFF),
    ColorInfo::new("darkorange", 0xFF, 0x87, 0x00),
    ColorInfo::new("salmon1", 0xFF, 0x87, 0x5F),
    ColorInfo::new("lightcoral", 0xFF, 0x87, 0x87),
    ColorInfo::new("palevioletred1", 0xFF, 0x87, 0xAF),
    ColorInfo::new("orchid2", 0xFF, 0x87, 0xD7),
    ColorInfo::new("orchid1", 0xFF, 0x87, 0xFF),
    ColorInfo::new("orange1", 0xFF, 0xAF, 0x00),
    ColorInfo::new("sandybrown", 0xFF, 0xAF, 0x5F),
    ColorInfo::new("lightsalmon1", 0xFF, 0xAF, 0x87),
    ColorInfo::new("lightpink1", 0xFF, 0xAF, 0xAF),
    ColorInfo::new("pink1", 0xFF, 0xAF, 0xD7),
    ColorInfo::new("plum1", 0xFF, 0xAF, 0xFF),
    ColorInfo::new("gold1", 0xFF, 0xD7, 0x00),
    ColorInfo::new("lightgoldenrod4", 0xFF, 0xD7, 0x5F),
    ColorInfo::new("lightgoldenrod5", 0xFF, 0xD7, 0x87),
    ColorInfo::new("navajowhite1", 0xFF, 0xD7, 0xAF),
    ColorInfo::new("mistyrose1", 0xFF, 0xD7, 0xD7),
    ColorInfo::new("thistle1", 0xFF, 0xD7, 0xFF),
    ColorInfo::new("yellow1", 0xFF, 0xFF, 0x00),
    ColorInfo::new("lightgoldenrod1", 0xFF, 0xFF, 0x5F),
    ColorInfo::new("khaki1", 0xFF, 0xFF, 0x87),
    ColorInfo::new("wheat1", 0xFF, 0xFF, 0xAF),
    ColorInfo::new("cornsilk1", 0xFF, 0xFF, 0xD7),
    ColorInfo::new("grey100", 0xFF, 0xFF, 0xFF),
    ColorInfo::new("grey3", 0x08, 0x08, 0x08),
    ColorInfo::new("grey7", 0x12, 0x12, 0x12),
    ColorInfo::new("grey11", 0x1C, 0x1C, 0x1C),
    ColorInfo::new("grey15", 0x26, 0x26, 0x26),
    ColorInfo::new("grey19", 0x30, 0x30, 0x30),
    ColorInfo::new("grey23", 0x3A, 0x3A, 0x3A),
    ColorInfo::new("grey27", 0x44, 0x44, 0x44),
    ColorInfo::new("grey30", 0x4E, 0x4E, 0x4E),
    ColorInfo::new("grey35", 0x58, 0x58, 0x58),
    ColorInfo::new("grey39", 0x62, 0x62, 0x62),
    ColorInfo::new("grey42", 0x6C, 0x6C, 0x6C),
    ColorInfo::new("grey46", 0x76, 0x76, 0x76),
    ColorInfo::new("grey50", 0x80, 0x80, 0x80),
    ColorInfo::new("grey54", 0x8A, 0x8A, 0x8A),
    ColorInfo::new("grey58", 0x94, 0x94, 0x94),
    ColorInfo::new("grey62", 0x9E, 0x9E, 0x9E),
    ColorInfo::new("grey66", 0xA8, 0xA8, 0xA8),
    ColorInfo::new("grey70", 0xB2, 0xB2, 0xB2),
    ColorInfo::new("grey74", 0xBC, 0xBC, 0xBC),
    ColorInfo::new("grey78", 0xC6, 0xC6, 0xC6),
    ColorInfo::new("grey82", 0xD0, 0xD0, 0xD0),
    ColorInfo::new("grey85", 0xDA, 0xDA, 0xDA),
    ColorInfo::new("grey89", 0xE4, 0xE4, 0xE4),
    ColorInfo::new("grey93", 0xEE, 0xEE, 0xEE),
];

/// The highlighting of `element` (`ShellHighlight::GetHighlightElement`).
pub fn highlight_element(element: HighlightElementType) -> HighlightElement {
    CONFIG.lock().unwrap_or_else(PoisonError::into_inner).elements[element as usize]
}

/// `ShellHighlight::IsEnabled`.
pub fn is_enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// `ShellHighlight::SetHighlighting`.
pub fn set_highlighting(enabled: bool) {
    ENABLED.store(enabled, Ordering::Relaxed);
}

/// `ShellHighlight::TryGetHighlightElement`: the element named `name` (any case), or the
/// error listing the names.
pub fn try_get_highlight_element(
    name: impl AsRef<[u8]>,
) -> Result<HighlightElementType, roastedduck_common::ByteString> {
    let name = name.as_ref();
    if let Some(index) = DEFAULT_ELEMENTS
        .iter()
        .position(|element| element.name.as_bytes().eq_ignore_ascii_case(name))
    {
        return Ok(ELEMENT_TYPES[index]);
    }
    let supported_options: Vec<&str> = DEFAULT_ELEMENTS.iter().map(|element| element.name).collect();
    Err(roastedduck_common::byte_format!(
        "Unknown element '{name}', supported options: {}\n",
        supported_options.join(", ")
    ))
}

/// `ShellHighlight::TryGetPrintColor`: the color named `name` (any case), or the error
/// with the closest names.
pub fn try_get_print_color(name: impl AsRef<[u8]>) -> Result<PrintColor, roastedduck_common::ByteString> {
    let name = name.as_ref();
    if let Some(code) = COLOR_INFO
        .iter()
        .position(|info| info.color_name.as_bytes().eq_ignore_ascii_case(name))
    {
        return Ok(PrintColor::from_code(code as u8));
    }
    let color_names: Vec<String> = COLOR_INFO.iter().map(|info| info.color_name.to_owned()).collect();
    let candidates =
        roastedduck_common::string_util::candidates_error_message_bytes(&color_names, name, b"Did you mean");
    Err(roastedduck_common::byte_format!(
        "Unknown highlighting color '{name}'\n{candidates}\nRun '.display_colors' for a list of available colors.\n"
    ))
}

/// `ShellHighlight::TerminalCode`: the escape sequence that switches to `color` and
/// `intensity`.
pub fn terminal_code(color: PrintColor, intensity: PrintIntensity) -> String {
    let mut terminal_code = match intensity {
        PrintIntensity::Bold => "\x1b[1m".to_owned(),
        PrintIntensity::Underline => "\x1b[4m".to_owned(),
        PrintIntensity::BoldUnderline => "\x1b[1m\x1b[4m".to_owned(),
        PrintIntensity::Standard => String::new(),
    };
    if let Some(code) = color.code() {
        terminal_code.push_str("\x1b[");
        match code {
            // The standard colors are \033[31m through \033[37m.
            1..=7 => terminal_code.push_str(&(30 + u16::from(code)).to_string()),
            // The bright colors are \033[90m through \033[97m.
            8..=15 => terminal_code.push_str(&(82 + u16::from(code)).to_string()),
            // The extended colors (and black) are \033[38;5;{code}m.
            _ => {
                let _ = write!(terminal_code, "38;5;{code}");
            }
        }
        terminal_code.push('m');
    }
    terminal_code
}

/// `ShellHighlight::ResetTerminalCode`.
pub const RESET_TERMINAL_CODE: &str = "\x1b[00m";

/// Sets the highlighting of `element` (`ShellHighlight::SetColor`); a color scheme
/// (`user_configured` false) leaves the elements the user configured alone.
pub fn set_element_color(
    element: HighlightElementType,
    color: PrintColor,
    intensity: PrintIntensity,
    user_configured: bool,
) {
    let mut config = CONFIG.lock().unwrap_or_else(PoisonError::into_inner);
    if user_configured {
        if !config.user_configured.contains(&element) {
            config.user_configured.push(element);
        }
    } else if config.user_configured.contains(&element) {
        return;
    }
    let entry = &mut config.elements[element as usize];
    entry.color = color;
    entry.intensity = intensity;
}

/// `ShellHighlight::ToggleMode`: the colors of a scheme.
pub fn toggle_mode(mode: HighlightMode) {
    use HighlightElementType as H;
    use PrintColor as C;
    use PrintIntensity::{Bold, Standard};
    let scheme: &[(HighlightElementType, PrintColor, PrintIntensity)] = match mode {
        HighlightMode::MixedMode => &[
            (H::Keyword, C::GREEN, Standard),
            (H::StringConstant, C::YELLOW, Standard),
            (H::NumericConstant, C::YELLOW, Standard),
            (H::ContinuationSelected, C::GREEN, Standard),
            (H::Prompt, C::DARKORANGE, Bold),
            (H::DatabaseName, C::ORANGE3, Standard),
            (H::SchemaName, C::DEEPSKYBLUE1, Standard),
        ],
        HighlightMode::DarkMode => &[
            (H::Keyword, C::DODGERBLUE1, Bold),
            (H::StringConstant, C::GOLD1, Standard),
            (H::NumericConstant, C::ORCHID2, Standard),
            (H::ContinuationSelected, C::DODGERBLUE1, Standard),
            (H::ErrorToken, C::INDIANRED3, Standard),
            (H::ErrorEmphasis, C::INDIANRED3, Bold),
            (H::ErrorSuggestion, C::INDIANRED3, Bold),
        ],
        HighlightMode::LightMode => &[
            (H::Keyword, C::DODGERBLUE2, Bold),
            (H::StringConstant, C::ORANGE4, Standard),
            (H::NumericConstant, C::DARKMAGENTA, Standard),
            (H::ContinuationSelected, C::DODGERBLUE2, Standard),
            (H::Prompt, C::DARKORANGE4, Bold),
            (H::ErrorToken, C::RED3, Standard),
            (H::ErrorEmphasis, C::RED3, Bold),
            (H::ErrorSuggestion, C::RED3, Bold),
            (H::DatabaseName, C::DARKORANGE4, Standard),
            (H::SchemaName, C::DEEPSKYBLUE6, Standard),
        ],
        HighlightMode::Automatic => &[],
    };
    for &(element, color, intensity) in scheme {
        set_element_color(element, color, intensity, false);
    }
}

/// Prints highlighted text to the shell's output (`ShellHighlight`).
#[derive(Debug)]
pub struct ShellHighlight<'a> {
    out: &'a Output,
}

impl<'a> ShellHighlight<'a> {
    pub fn new(out: &'a Output) -> Self {
        Self { out }
    }

    /// `PrintText(text, output, color, intensity)`.
    pub fn print_text_colored(
        &self,
        text: &str,
        output: PrintOutput,
        color: PrintColor,
        intensity: PrintIntensity,
    ) {
        if (color == PrintColor::STANDARD && intensity == PrintIntensity::Standard) || !is_enabled() {
            self.out.print(output, text);
            return;
        }
        let terminal_code = terminal_code(color, intensity);
        if terminal_code.is_empty() {
            self.out.print(output, text);
            return;
        }
        self.out
            .print(output, format!("{terminal_code}{text}{RESET_TERMINAL_CODE}"));
    }

    /// Native message bytes with the selected terminal style.
    pub fn print_bytes_colored(
        &self,
        text: &[u8],
        output: PrintOutput,
        color: PrintColor,
        intensity: PrintIntensity,
    ) {
        let code = terminal_code(color, intensity);
        if (color == PrintColor::STANDARD && intensity == PrintIntensity::Standard)
            || !is_enabled()
            || code.is_empty()
        {
            self.out.print(output, text);
        } else {
            self.out.print(
                output,
                roastedduck_common::byte_format!("{code}{text}{RESET_TERMINAL_CODE}"),
            );
        }
    }
    pub fn print_bytes(&self, text: &[u8], output: PrintOutput, element_type: HighlightElementType) {
        let element = highlight_element(element_type);
        self.print_bytes_colored(text, output, element.color, element.intensity);
    }

    /// `PrintText(text, output, type)`.
    pub fn print_text(&self, text: &str, output: PrintOutput, element_type: HighlightElementType) {
        let element = highlight_element(element_type);
        self.print_text_colored(text, output, element.color, element.intensity);
    }

    /// `PrintError`: an error message to standard error, its type emphasized and the
    /// tokens of the query it quotes highlighted.
    pub fn print_error(&self, error_msg: &str) {
        if error_msg.is_empty() {
            return;
        }
        let mut error_msg = error_msg;
        let mut error_type = None;
        if let Some(error_location) = error_msg.find("Error: ") {
            error_type = Some(&error_msg[..error_location + 6]);
            error_msg = &error_msg[error_location + 7..];
        }
        let mut tokens = tokenize_error(error_msg);
        if tokens.first().is_some_and(|token| token.start > 0) || (tokens.is_empty() && !error_msg.is_empty())
        {
            tokens.insert(
                0,
                HighlightToken {
                    kind: HighlightTokenType::Identifier,
                    start: 0,
                },
            );
        }
        if let Some(error_type) = error_type {
            self.print_text(
                &format!("{error_type}\n"),
                PrintOutput::Stderr,
                HighlightElementType::ErrorEmphasis,
            );
        }
        for (i, token) in tokens.iter().enumerate() {
            let element_type = match token.kind {
                HighlightTokenType::Identifier | HighlightTokenType::Operator => HighlightElementType::None,
                HighlightTokenType::Error => HighlightElementType::ErrorToken,
                HighlightTokenType::ErrorEmphasis => HighlightElementType::ErrorEmphasis,
                HighlightTokenType::ErrorSuggestion => HighlightElementType::ErrorSuggestion,
                HighlightTokenType::NumericConstant => HighlightElementType::NumericConstant,
                HighlightTokenType::StringConstant => HighlightElementType::StringConstant,
                HighlightTokenType::Keyword => HighlightElementType::Keyword,
                HighlightTokenType::Comment => HighlightElementType::LineIndicator,
            };
            let start = token.start;
            let end = tokens.get(i + 1).map_or(error_msg.len(), |next| next.start);
            if end > start
                && let Some(text) = error_msg.get(start..end)
            {
                self.print_text(text, PrintOutput::Stderr, element_type);
            }
        }
        self.print_text_colored(
            "\n",
            PrintOutput::Stderr,
            PrintColor::STANDARD,
            PrintIntensity::Standard,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_codes() {
        assert_eq!(
            terminal_code(PrintColor::RED, PrintIntensity::Standard),
            "\x1b[31m"
        );
        assert_eq!(
            terminal_code(PrintColor::GRAY, PrintIntensity::Standard),
            "\x1b[90m"
        );
        assert_eq!(
            terminal_code(PrintColor::DARKORANGE, PrintIntensity::Bold),
            "\x1b[1m\x1b[38;5;208m"
        );
        assert_eq!(
            terminal_code(PrintColor::STANDARD, PrintIntensity::Underline),
            "\x1b[4m"
        );
        assert_eq!(COLOR_INFO[208].color_name, "darkorange");
        assert_eq!(try_get_print_color("GOLD1"), Ok(PrintColor::GOLD1));
    }
}
