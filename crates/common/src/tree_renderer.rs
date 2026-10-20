//! Rendering of operator trees for `EXPLAIN` (DuckDB's `common/render_tree.cpp` and
//! `common/tree_renderer/*`).
//!
//! A [`RenderTree`] is a grid of [`RenderTreeNode`]s built from a logical plan, a physical
//! plan or a profiling tree; a renderer turns the grid into text. The box-drawing text
//! renderer and the JSON renderer reproduce DuckDB's output byte for byte.

mod formats;

use std::fmt::Write as _;

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthChar;

use crate::OrderedMap;

/// The key-value lines of a node (DuckDB's `InsertionOrderPreservingMap<string>`).
pub type RenderParams = OrderedMap<String, String>;

/// One box of the rendered tree (DuckDB's `RenderTreeNode`).
#[derive(Clone, Debug, Default)]
pub struct RenderTreeNode {
    pub name: String,
    pub extra_text: RenderParams,
    /// The grid positions of the node's children.
    pub child_positions: Vec<(usize, usize)>,
}

impl RenderTreeNode {
    /// The key of the true cardinality (`RenderTreeNode::CARDINALITY`).
    pub const CARDINALITY: &'static str = "__cardinality__";
    /// The key of the estimated cardinality (`RenderTreeNode::ESTIMATED_CARDINALITY`).
    pub const ESTIMATED_CARDINALITY: &'static str = "__estimated_cardinality__";
    /// The key of the timing (`RenderTreeNode::TIMING`).
    pub const TIMING: &'static str = "__timing__";

    pub fn new(name: impl Into<String>, extra_text: RenderParams) -> Self {
        Self {
            name: name.into(),
            extra_text,
            child_positions: Vec::new(),
        }
    }
}

/// A handle to a tree whose nodes can be rendered (DuckDB's `TreeChildrenIterator` +
/// `CreateNode`), usually a reference.
pub trait RenderSource: Sized {
    /// The node of this operator.
    fn render_node(&self) -> RenderTreeNode;
    /// The operator's children, in rendering order.
    fn render_children(&self) -> Vec<Self>;
}

/// A grid of nodes: a node's first child is right below it, further children to the
/// right of the first child's subtree (DuckDB's `RenderTree`).
#[derive(Clone, Debug)]
pub struct RenderTree {
    nodes: Vec<Option<RenderTreeNode>>,
    pub width: usize,
    pub height: usize,
}

impl RenderTree {
    /// An empty grid.
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            nodes: vec![None; (width + 1) * (height + 1)],
            width,
            height,
        }
    }

    /// The grid of a tree (DuckDB's `RenderTree::CreateRenderTree`).
    pub fn create<T: RenderSource>(root: &T) -> Self {
        let (width, height) = tree_width_height(root);
        let mut tree = Self::new(width, height);
        tree.create_recursive(root, 0, 0);
        tree
    }

    fn create_recursive<T: RenderSource>(&mut self, op: &T, x: usize, y: usize) -> usize {
        let mut node = op.render_node();
        let children = op.render_children();
        if children.is_empty() {
            self.set_node(x, y, node);
            return 1;
        }
        let mut width = 0;
        for child in children {
            let child_x = x + width;
            let child_y = y + 1;
            node.child_positions.push((child_x, child_y));
            width += self.create_recursive(&child, child_x, child_y);
        }
        self.set_node(x, y, node);
        width
    }

    fn position(&self, x: usize, y: usize) -> usize {
        y * self.width + x
    }

    /// The node at a grid position.
    pub fn node(&self, x: usize, y: usize) -> Option<&RenderTreeNode> {
        if x >= self.width || y >= self.height {
            return None;
        }
        self.nodes[self.position(x, y)].as_ref()
    }

    /// Whether a grid position holds a node.
    pub fn has_node(&self, x: usize, y: usize) -> bool {
        self.node(x, y).is_some()
    }

    /// Places a node.
    pub fn set_node(&mut self, x: usize, y: usize, node: RenderTreeNode) {
        let position = self.position(x, y);
        self.nodes[position] = Some(node);
    }

    /// Turns internal key names (`__estimated_cardinality__`) into titles
    /// (`Estimated Cardinality`) for renderers that show keys verbatim
    /// (DuckDB's `RenderTree::SanitizeKeyNames`).
    pub fn sanitize_key_names(&mut self) {
        for node in self.nodes.iter_mut().flatten() {
            let mut sanitized = RenderParams::new();
            for (key, value) in std::mem::take(&mut node.extra_text) {
                let key = if key.starts_with("__") {
                    title(&key.replace("__", "").replace('_', " "))
                } else {
                    key
                };
                if !sanitized.contains_key(&key) {
                    sanitized.push(key, value);
                }
            }
            node.extra_text = sanitized;
        }
    }
}

/// DuckDB's `StringUtil::Title`: the first letter of every word upper case, the rest
/// lower case.
fn title(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut uppercase_next = true;
    for c in text.chars() {
        if c.is_ascii_alphabetic() {
            if uppercase_next {
                result.push(c.to_ascii_uppercase());
                uppercase_next = false;
            } else {
                result.push(c.to_ascii_lowercase());
            }
        } else {
            uppercase_next = true;
            result.push(c);
        }
    }
    result
}

fn tree_width_height<T: RenderSource>(op: &T) -> (usize, usize) {
    let children = op.render_children();
    if children.is_empty() {
        return (1, 1);
    }
    let mut width = 0;
    let mut height = 0;
    for child in children {
        let (child_width, child_height) = tree_width_height(&child);
        width += child_width;
        height = height.max(child_height);
    }
    (width, height + 1)
}

/// The output format of `EXPLAIN` (DuckDB's `ExplainFormat` as rendered).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RendererKind {
    Text,
    Json,
    Html,
    Graphviz,
    Yaml,
    Mermaid,
}

/// Renders a tree in the given format (DuckDB's `TreeRenderer::CreateRenderer` +
/// `ToStream`).
pub fn render(kind: RendererKind, mut tree: RenderTree) -> String {
    match kind {
        RendererKind::Text => TextTreeRenderer::default().render(&tree),
        RendererKind::Json => {
            tree.sanitize_key_names();
            render_json(&tree)
        }
        RendererKind::Html => {
            tree.sanitize_key_names();
            formats::render_html(&tree)
        }
        RendererKind::Graphviz => {
            tree.sanitize_key_names();
            formats::render_graphviz(&tree)
        }
        RendererKind::Yaml => {
            tree.sanitize_key_names();
            formats::render_yaml(&tree)
        }
        RendererKind::Mermaid => {
            tree.sanitize_key_names();
            formats::render_mermaid(&tree)
        }
    }
}

/// The box-drawing text renderer (DuckDB's `TextTreeRenderer` with its default
/// `TextTreeRendererConfig`).
#[derive(Clone, Debug)]
pub struct TextTreeRenderer {
    pub maximum_render_width: usize,
    pub node_render_width: usize,
    pub minimum_render_width: usize,
    pub max_extra_lines: usize,
    pub thousand_separator: Option<char>,
    pub decimal_separator: Option<char>,
}

impl Default for TextTreeRenderer {
    fn default() -> Self {
        Self {
            maximum_render_width: 240,
            node_render_width: 29,
            minimum_render_width: 15,
            max_extra_lines: 30,
            thousand_separator: Some(','),
            decimal_separator: Some('.'),
        }
    }
}

const LTCORNER: &str = "┌";
const RTCORNER: &str = "┐";
const LDCORNER: &str = "└";
const RDCORNER: &str = "┘";
const TMIDDLE: &str = "┬";
const LMIDDLE: &str = "├";
const DMIDDLE: &str = "┴";
const VERTICAL: &str = "│";
const HORIZONTAL: &str = "─";

/// The render width of the grapheme starting at the first char of `grapheme`
/// (DuckDB's `Utf8Proc::RenderWidth`, which uses utf8proc's `charwidth`).
fn grapheme_width(grapheme: &str) -> usize {
    grapheme
        .chars()
        .next()
        .and_then(UnicodeWidthChar::width)
        .unwrap_or(0)
}

/// Whether a line may be broken before this byte (`CanSplitOnThisChar`).
const fn can_split_on(byte: u8) -> bool {
    (byte < b'0' || (byte > b'9' && byte < b'A') || (byte > b'Z' && byte < b'a')) && byte != b'_'
}

const fn is_padding(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | b'\r')
}

/// Trims padding characters (`RemovePadding`).
fn remove_padding(text: &str) -> &str {
    let bytes = text.as_bytes();
    let mut start = 0;
    let mut end = bytes.len();
    while start < bytes.len() && is_padding(bytes[start]) {
        start += 1;
    }
    while end > start && is_padding(bytes[end - 1]) {
        end -= 1;
    }
    &text[start..end]
}

/// Centers `source` in `max_render_width` columns, truncating with `...` if it does not
/// fit (DuckDB's `AdjustTextForRendering`).
fn adjust_text_for_rendering(source: &str, max_render_width: usize) -> String {
    let mut render_width = 0;
    // (end byte offset, cumulative width) per grapheme.
    let mut render_widths: Vec<(usize, usize)> = Vec::new();
    for (offset, grapheme) in source.grapheme_indices(true) {
        render_width += grapheme_width(grapheme);
        render_widths.push((offset + grapheme.len(), render_width));
        if render_width > max_render_width {
            break;
        }
    }
    let mut source = source;
    if render_width > max_render_width {
        for &(start, width) in render_widths.iter().rev() {
            if width + 4 < max_render_width {
                return format!(
                    "{}...{}",
                    &source[..start],
                    " ".repeat(max_render_width - width - 3)
                );
            }
        }
        source = "...";
        render_width = 3;
    }
    let total_spaces = max_render_width.saturating_sub(render_width);
    let half_spaces = total_spaces / 2;
    let extra_left_space = total_spaces % 2;
    format!(
        "{}{}{}",
        " ".repeat(half_spaces + extra_left_space),
        source,
        " ".repeat(half_spaces)
    )
}

impl TextTreeRenderer {
    /// Renders the tree as text (DuckDB's `TextTreeRenderer::ToStreamInternal`).
    pub fn render(mut self, root: &RenderTree) -> String {
        while root.width * self.node_render_width > self.maximum_render_width {
            if self.node_render_width - 2 < self.minimum_render_width {
                break;
            }
            self.node_render_width -= 2;
        }
        let mut out = String::new();
        for y in 0..root.height {
            self.render_top_layer(root, &mut out, y);
            self.render_box_content(root, &mut out, y);
            self.render_bottom_layer(root, &mut out, y);
        }
        out
    }

    fn half(&self) -> usize {
        self.node_render_width / 2
    }

    fn has_adjacent_nodes(root: &RenderTree, x: usize, y: usize) -> bool {
        (x..root.width).any(|i| root.has_node(i, y))
    }

    fn render_top_layer(&self, root: &RenderTree, out: &mut String, y: usize) {
        for x in 0..root.width {
            if x * self.node_render_width >= self.maximum_render_width {
                break;
            }
            if root.has_node(x, y) {
                out.push_str(LTCORNER);
                out.push_str(&HORIZONTAL.repeat(self.half() - 1));
                out.push_str(if y == 0 { HORIZONTAL } else { DMIDDLE });
                out.push_str(&HORIZONTAL.repeat(self.half() - 1));
                out.push_str(RTCORNER);
            } else {
                if !Self::has_adjacent_nodes(root, x, y) {
                    continue;
                }
                out.push_str(&" ".repeat(self.node_render_width));
            }
        }
        out.push('\n');
    }

    fn render_bottom_layer(&self, root: &RenderTree, out: &mut String, y: usize) {
        for x in 0..=root.width {
            if x * self.node_render_width >= self.maximum_render_width {
                break;
            }
            let has_adjacent_nodes = Self::has_adjacent_nodes(root, x, y);
            if root.node(x, y).is_some() {
                out.push_str(LDCORNER);
                out.push_str(&HORIZONTAL.repeat(self.half() - 1));
                out.push_str(if root.has_node(x, y + 1) {
                    TMIDDLE
                } else {
                    HORIZONTAL
                });
                out.push_str(&HORIZONTAL.repeat(self.half() - 1));
                out.push_str(RDCORNER);
            } else if root.has_node(x, y + 1) {
                out.push_str(&" ".repeat(self.half()));
                out.push_str(VERTICAL);
                if has_adjacent_nodes || should_render_whitespace(root, x, y) {
                    out.push_str(&" ".repeat(self.half()));
                }
            } else if has_adjacent_nodes || should_render_whitespace(root, x, y) {
                out.push_str(&" ".repeat(self.node_render_width));
            }
        }
        out.push('\n');
    }

    /// Adds thousand separators (DuckDB's `FormatNumber`).
    fn format_number(&self, input: &str) -> String {
        if self.decimal_separator.is_none() && self.thousand_separator.is_none() {
            return input.to_owned();
        }
        let bytes = input.as_bytes();
        let character_count = bytes.iter().take_while(|b| b.is_ascii_digit()).count();
        let mut separator_position = if character_count % 3 == 0 {
            3
        } else {
            character_count % 3
        };
        let mut result = String::new();
        for (c, &byte) in bytes.iter().enumerate().take(character_count) {
            if c == separator_position
                && let Some(separator) = self.thousand_separator
            {
                result.push(separator);
                separator_position += 3;
            }
            result.push(char::from(byte));
        }
        for c in input[character_count..].chars() {
            match (c, self.decimal_separator) {
                ('.', Some(separator)) => result.push(separator),
                _ => result.push(c),
            }
        }
        result
    }

    fn cardinality_text(&self, prefix: &str, value: &str) -> String {
        format!(
            "{prefix}{} row{}",
            self.format_number(value),
            if value == "1" { "" } else { "s" }
        )
    }

    fn render_box_content(&self, root: &RenderTree, out: &mut String, y: usize) {
        let mut extra_info: Vec<Vec<String>> = vec![Vec::new(); root.width];
        let mut extra_height = 0;
        for (x, info) in extra_info.iter_mut().enumerate() {
            if let Some(node) = root.node(x, y) {
                self.split_up_extra_info(&node.extra_text, info);
                extra_height = extra_height.max(info.len());
            }
        }
        let halfway_point = extra_height.div_ceil(2);
        for render_y in 0..=extra_height {
            for (x, info) in extra_info.iter().enumerate() {
                if x * self.node_render_width >= self.maximum_render_width {
                    break;
                }
                let has_adjacent_nodes = Self::has_adjacent_nodes(root, x, y);
                let Some(node) = root.node(x, y) else {
                    self.render_empty_spot(root, out, x, y, render_y, halfway_point, has_adjacent_nodes);
                    continue;
                };
                out.push_str(VERTICAL);
                let mut render_text = if render_y == 0 {
                    node.name.clone()
                } else {
                    info.get(render_y - 1).cloned().unwrap_or_default()
                };
                if render_y + 1 == extra_height
                    && render_text.is_empty()
                    && let Some(value) = node.extra_text.get(RenderTreeNode::CARDINALITY)
                {
                    render_text = self.cardinality_text("", value);
                }
                if render_y == extra_height && render_text.is_empty() {
                    let has_cardinality = node.extra_text.contains_key(RenderTreeNode::CARDINALITY);
                    if let Some(timing) = node.extra_text.get(RenderTreeNode::TIMING) {
                        render_text = format!("({timing})");
                    }
                    if !has_cardinality
                        && let Some(value) = node.extra_text.get(RenderTreeNode::ESTIMATED_CARDINALITY)
                    {
                        render_text = self.cardinality_text("~", value);
                    }
                }
                out.push_str(&adjust_text_for_rendering(
                    &render_text,
                    self.node_render_width - 2,
                ));
                if render_y == halfway_point && node.child_positions.len() > 1 {
                    out.push_str(LMIDDLE);
                } else {
                    out.push_str(VERTICAL);
                }
            }
            out.push('\n');
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn render_empty_spot(
        &self,
        root: &RenderTree,
        out: &mut String,
        x: usize,
        y: usize,
        render_y: usize,
        halfway_point: usize,
        has_adjacent_nodes: bool,
    ) {
        if render_y == halfway_point {
            let has_child_to_the_right = should_render_whitespace(root, x, y);
            if root.has_node(x, y + 1) {
                out.push_str(&HORIZONTAL.repeat(self.half()));
                if has_child_to_the_right {
                    out.push_str(TMIDDLE);
                    out.push_str(&HORIZONTAL.repeat(self.half()));
                } else {
                    out.push_str(RTCORNER);
                    if has_adjacent_nodes {
                        out.push_str(&" ".repeat(self.half()));
                    }
                }
            } else if has_child_to_the_right {
                out.push_str(&HORIZONTAL.repeat(self.node_render_width));
            } else if has_adjacent_nodes {
                out.push_str(&" ".repeat(self.node_render_width));
            }
        } else if render_y >= halfway_point {
            if root.has_node(x, y + 1) {
                out.push_str(&" ".repeat(self.half()));
                out.push_str(VERTICAL);
                if has_adjacent_nodes || should_render_whitespace(root, x, y) {
                    out.push_str(&" ".repeat(self.half()));
                }
            } else if has_adjacent_nodes || should_render_whitespace(root, x, y) {
                out.push_str(&" ".repeat(self.node_render_width));
            }
        } else if has_adjacent_nodes {
            out.push_str(&" ".repeat(self.node_render_width));
        }
    }

    /// Breaks a line into pieces that fit a box (DuckDB's `SplitStringBuffer`).
    fn split_string_buffer(&self, source: &str, result: &mut Vec<String>) {
        let max_line_render_size = self.node_render_width - 2;
        let bytes = source.as_bytes();
        let mut character_pos = 0;
        let mut start_pos = 0;
        let mut render_width = 0;
        let mut last_possible_split = 0;
        while character_pos < bytes.len() {
            // Every position visited is a grapheme boundary (`NextGraphemeCluster`).
            let grapheme = source[character_pos..].graphemes(true).next().unwrap_or_default();
            let char_render_width = grapheme_width(grapheme);
            let next_character_pos = character_pos + grapheme.len().max(1);
            if render_width + char_render_width > max_line_render_size {
                if start_pos + 8 > last_possible_split {
                    last_possible_split = character_pos;
                }
                result.push(source[start_pos..last_possible_split].to_owned());
                render_width = character_pos - last_possible_split;
                start_pos = last_possible_split;
                character_pos = last_possible_split;
            }
            if can_split_on(bytes[character_pos]) {
                last_possible_split = character_pos;
            }
            character_pos = next_character_pos;
            render_width += char_render_width;
        }
        if bytes.len() > start_pos {
            result.push(source[start_pos..].to_owned());
        }
    }

    /// The lines of a node's key-value text (DuckDB's `SplitUpExtraInfo`).
    fn split_up_extra_info(&self, extra_info: &RenderParams, result: &mut Vec<String>) {
        if extra_info.is_empty() {
            return;
        }
        result.push(HORIZONTAL.repeat(self.node_render_width - 9));
        let mut requires_padding = false;
        let mut was_inlined = false;
        for (key, value) in extra_info.iter() {
            let trimmed = remove_padding(value);
            if trimmed.is_empty() {
                continue;
            }
            let mut text = trimmed.to_owned();
            let mut is_inlined = false;
            if !key.starts_with("__") {
                let available_width = self.node_render_width - 7;
                let total_size = key.len() + text.len() + 2;
                let is_multiline = text.contains('\n');
                if !is_multiline && total_size < available_width {
                    text = format!("{key}: {text}");
                    is_inlined = true;
                } else {
                    text = format!("{key}:\n{text}");
                }
            }
            if is_inlined && was_inlined {
                requires_padding = false;
            }
            if requires_padding {
                result.push(String::new());
            }
            if key == RenderTreeNode::CARDINALITY {
                result.push(String::new());
                if extra_info.contains_key(RenderTreeNode::TIMING) {
                    result.push(String::new());
                }
                continue;
            }
            if key == RenderTreeNode::ESTIMATED_CARDINALITY {
                if extra_info.contains_key(RenderTreeNode::CARDINALITY) {
                    result.pop();
                    continue;
                }
                result.push(String::new());
                continue;
            }
            let mut splits: Vec<&str> = text.split('\n').collect();
            if splits.len() > self.max_extra_lines {
                let half = self.max_extra_lines / 2;
                let mut truncated: Vec<&str> = splits[..half].to_vec();
                truncated.push("...");
                truncated.extend_from_slice(&splits[splits.len() - half..]);
                splits = truncated;
            }
            for split in splits {
                self.split_string_buffer(split, result);
            }
            requires_padding = true;
            was_inlined = is_inlined;
        }
    }
}

/// Whether an empty spot lies between a node and one of its later children
/// (DuckDB's `ShouldRenderWhitespace`).
fn should_render_whitespace(root: &RenderTree, x: usize, y: usize) -> bool {
    let mut found_children = 0;
    let mut x = x;
    loop {
        if root.has_node(x, y + 1) {
            found_children += 1;
        }
        if let Some(node) = root.node(x, y) {
            return node.child_positions.len() > 1 && found_children < node.child_positions.len();
        }
        if x == 0 {
            return false;
        }
        x -= 1;
    }
}

/// Appends a JSON string literal the way yyjson writes it.
fn json_string(out: &mut String, text: &str) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn indent(out: &mut String, level: usize) {
    out.push_str(&"    ".repeat(level));
}

fn render_json_node(tree: &RenderTree, x: usize, y: usize, out: &mut String, level: usize) {
    let Some(node) = tree.node(x, y) else {
        out.push_str("{}");
        return;
    };
    out.push_str("{\n");
    indent(out, level + 1);
    out.push_str("\"name\": ");
    json_string(out, &node.name);
    out.push_str(",\n");
    indent(out, level + 1);
    out.push_str("\"children\": ");
    if node.child_positions.is_empty() {
        out.push_str("[]");
    } else {
        out.push_str("[\n");
        for (i, &(child_x, child_y)) in node.child_positions.iter().enumerate() {
            indent(out, level + 2);
            render_json_node(tree, child_x, child_y, out, level + 2);
            if i + 1 < node.child_positions.len() {
                out.push(',');
            }
            out.push('\n');
        }
        indent(out, level + 1);
        out.push(']');
    }
    out.push_str(",\n");
    indent(out, level + 1);
    out.push_str("\"extra_info\": ");
    if node.extra_text.is_empty() {
        out.push_str("{}");
    } else {
        out.push_str("{\n");
        let count = node.extra_text.len();
        for (i, (key, value)) in node.extra_text.iter().enumerate() {
            indent(out, level + 2);
            json_string(out, key);
            out.push_str(": ");
            let splits: Vec<&str> = value.split('\n').collect();
            if splits.len() > 1 {
                out.push_str("[\n");
                for (j, split) in splits.iter().enumerate() {
                    indent(out, level + 3);
                    json_string(out, split);
                    if j + 1 < splits.len() {
                        out.push(',');
                    }
                    out.push('\n');
                }
                indent(out, level + 2);
                out.push(']');
            } else {
                json_string(out, value);
            }
            if i + 1 < count {
                out.push(',');
            }
            out.push('\n');
        }
        indent(out, level + 1);
        out.push('}');
    }
    out.push('\n');
    indent(out, level);
    out.push('}');
}

/// Renders the tree as a JSON array holding the root node (DuckDB's `JSONTreeRenderer`).
fn render_json(tree: &RenderTree) -> String {
    let mut out = String::from("[\n");
    indent(&mut out, 1);
    render_json_node(tree, 0, 0, &mut out, 1);
    out.push_str("\n]");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Node {
        name: &'static str,
        params: Vec<(&'static str, &'static str)>,
        children: Vec<Node>,
    }

    impl RenderSource for &Node {
        fn render_node(&self) -> RenderTreeNode {
            RenderTreeNode::new(
                self.name,
                self.params
                    .iter()
                    .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                    .collect(),
            )
        }
        fn render_children(&self) -> Vec<Self> {
            self.children.iter().collect()
        }
    }

    #[test]
    fn renders_a_single_node_like_duckdb() {
        let tree = RenderTree::create(&&Node {
            name: "SEQ_SCAN ",
            params: vec![
                ("Table", "memory.main.test"),
                ("Type", "Sequential Scan"),
                ("Projections", "X"),
                ("__estimated_cardinality__", "0"),
            ],
            children: Vec::new(),
        });
        let text = render(RendererKind::Text, tree);
        let expected = "┌───────────────────────────┐\n\
                        │         SEQ_SCAN          │\n\
                        │    ────────────────────   │\n\
                        │           Table:          │\n\
                        │      memory.main.test     │\n\
                        │                           │\n\
                        │   Type: Sequential Scan   │\n\
                        │       Projections: X      │\n\
                        │                           │\n\
                        │          ~0 rows          │\n\
                        └───────────────────────────┘\n";
        assert_eq!(text, expected);
    }

    #[test]
    fn renders_two_children() {
        let leaf = |name| Node {
            name,
            params: Vec::new(),
            children: Vec::new(),
        };
        let tree = RenderTree::create(&&Node {
            name: "HASH_JOIN",
            params: vec![("Join Type", "INNER")],
            children: vec![leaf("A"), leaf("B")],
        });
        let text = render(RendererKind::Text, tree);
        assert!(text.contains("├──────────────┐"), "{text}");
        assert!(
            text.contains("┌─────────────┴─────────────┐┌─────────────┴─────────────┐"),
            "{text}"
        );
    }

    #[test]
    fn formats_numbers_with_separators() {
        let renderer = TextTreeRenderer::default();
        assert_eq!(renderer.format_number("1234567"), "1,234,567");
        assert_eq!(renderer.format_number("123"), "123");
    }
}
