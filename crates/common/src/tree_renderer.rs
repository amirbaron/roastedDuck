//! Rendering of operator trees for `EXPLAIN` (DuckDB's `common/render_tree.cpp` and
//! `common/tree_renderer/*`).
//!
//! A [`RenderTree`] is a grid of [`RenderTreeNode`]s built from a logical plan, a physical
//! plan or a profiling tree; a renderer turns the grid into text. The box-drawing text
//! renderer and the JSON renderer reproduce DuckDB's output byte for byte.

mod formats;
pub(crate) mod unicode;

use crate::{ByteString, Error, OrderedMap, Result, byte_format};

/// The key-value lines of a node (DuckDB's `InsertionOrderPreservingMap<string>`).
#[derive(Clone, Debug, Default)]
pub struct RenderParams(OrderedMap<ByteString, ByteString>);
impl RenderParams {
    pub const fn new() -> Self {
        Self(OrderedMap::new())
    }
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn iter(&self) -> impl ExactSizeIterator<Item = (&ByteString, &ByteString)> {
        self.0.iter()
    }
    pub fn get(&self, key: impl AsRef<[u8]>) -> Option<&ByteString> {
        self.0
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key.as_ref()))
            .map(|(_, value)| value)
    }
    pub fn get_mut(&mut self, key: impl AsRef<[u8]>) -> Option<&mut ByteString> {
        self.0
            .iter_mut()
            .find(|(k, _)| k.eq_ignore_ascii_case(key.as_ref()))
            .map(|(_, value)| value)
    }
    pub fn contains_key(&self, key: impl AsRef<[u8]>) -> bool {
        self.get(key).is_some()
    }
    /// C++ insertion keeps the first value of an ASCII-case-insensitive key.
    pub fn insert(&mut self, key: impl Into<ByteString>, value: impl Into<ByteString>) -> Option<ByteString> {
        let key = key.into();
        if let Some(existing) = self.get(&key) {
            return Some(existing.clone());
        }
        self.0.push(key, value.into());
        None
    }
    pub fn push(&mut self, key: impl Into<ByteString>, value: impl Into<ByteString>) {
        self.0.push(key.into(), value.into());
    }
}
impl<K: Into<ByteString>, V: Into<ByteString>> FromIterator<(K, V)> for RenderParams {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
        let mut result = Self::new();
        for (key, value) in iter {
            result.insert(key, value);
        }
        result
    }
}
impl IntoIterator for RenderParams {
    type Item = (ByteString, ByteString);
    type IntoIter = <OrderedMap<ByteString, ByteString> as IntoIterator>::IntoIter;
    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

/// One box of the rendered tree (DuckDB's `RenderTreeNode`).
#[derive(Clone, Debug, Default)]
pub struct RenderTreeNode {
    pub name: ByteString,
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

    pub fn new(name: impl Into<ByteString>, extra_text: RenderParams) -> Self {
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
                let key = if key.starts_with(b"__") {
                    title(&sanitize_internal_key(&key))
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
fn sanitize_internal_key(key: &[u8]) -> ByteString {
    let mut result = ByteString::new();
    let mut offset = 0;
    while offset < key.len() {
        if key[offset..].starts_with(b"__") {
            offset += 2;
            continue;
        }
        result.push(if key[offset] == b'_' { b' ' } else { key[offset] });
        offset += 1;
    }
    result
}
fn title(text: &[u8]) -> ByteString {
    let mut result = ByteString::new();
    let mut uppercase_next = true;
    for &c in text {
        if c.is_ascii_alphabetic() {
            result.push(if uppercase_next {
                c.to_ascii_uppercase()
            } else {
                c.to_ascii_lowercase()
            });
            uppercase_next = false;
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
pub fn render(kind: RendererKind, mut tree: RenderTree) -> Result<ByteString> {
    if kind == RendererKind::Text {
        return TextTreeRenderer::default().render(&tree);
    }
    tree.sanitize_key_names();
    Ok(match kind {
        RendererKind::Text => unreachable!(),
        RendererKind::Json => return render_json(&tree),
        RendererKind::Html => formats::render_html(&tree),
        RendererKind::Graphviz => formats::render_graphviz(&tree),
        RendererKind::Yaml => formats::render_yaml(&tree),
        RendererKind::Mermaid => formats::render_mermaid(&tree),
    })
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

/// Whether a line may be broken before this byte (`CanSplitOnThisChar`).
const fn can_split_on(byte: u8) -> bool {
    // The reference compares plain C++ char, whose signedness follows the target ABI.
    let byte = byte as std::ffi::c_char as i32;
    (byte < 48 || (byte > 57 && byte < 65) || (byte > 90 && byte < 97)) && byte != 95
}

const fn is_padding(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | b'\r')
}

/// Trims padding characters (`RemovePadding`).
fn remove_padding(bytes: &[u8]) -> &[u8] {
    let mut start = 0;
    let mut end = bytes.len();
    while start < end && is_padding(bytes[start]) {
        start += 1;
    }
    while end > start && is_padding(bytes[end - 1]) {
        end -= 1;
    }
    &bytes[start..end]
}

fn adjust_text_for_rendering(source: &[u8], max_render_width: usize) -> Result<ByteString> {
    let mut render_width = 0;
    let mut render_widths = Vec::new();
    let mut offset = 0;
    while offset < source.len() {
        let (next, width) = unicode::cluster(source, offset)?;
        render_width += width;
        offset = next;
        render_widths.push((offset, render_width));
        if render_width > max_render_width {
            break;
        }
    }
    let mut source = source;
    if render_width > max_render_width {
        for &(start, width) in render_widths.iter().rev() {
            if width + 4 < max_render_width {
                return Ok(byte_format!(
                    "{}...{}",
                    &source[..start],
                    " ".repeat(max_render_width - width - 3)
                ));
            }
        }
        source = b"...";
        render_width = 3;
    }
    let total_spaces = max_render_width.saturating_sub(render_width);
    let half_spaces = total_spaces / 2;
    Ok(byte_format!(
        "{}{}{}",
        " ".repeat(half_spaces + total_spaces % 2),
        source,
        " ".repeat(half_spaces)
    ))
}

impl TextTreeRenderer {
    /// Renders the tree as text (DuckDB's `TextTreeRenderer::ToStreamInternal`).
    pub fn render(mut self, root: &RenderTree) -> Result<ByteString> {
        while root.width * self.node_render_width > self.maximum_render_width {
            if self.node_render_width - 2 < self.minimum_render_width {
                break;
            }
            self.node_render_width -= 2;
        }
        let mut out = ByteString::new();
        for y in 0..root.height {
            self.render_top_layer(root, &mut out, y);
            self.render_box_content(root, &mut out, y)?;
            self.render_bottom_layer(root, &mut out, y);
        }
        Ok(out)
    }

    fn half(&self) -> usize {
        self.node_render_width / 2
    }

    fn has_adjacent_nodes(root: &RenderTree, x: usize, y: usize) -> bool {
        (x..root.width).any(|i| root.has_node(i, y))
    }

    fn render_top_layer(&self, root: &RenderTree, out: &mut ByteString, y: usize) {
        for x in 0..root.width {
            if x * self.node_render_width >= self.maximum_render_width {
                break;
            }
            if root.has_node(x, y) {
                out.push_bytes(LTCORNER);
                out.push_bytes(HORIZONTAL.repeat(self.half() - 1));
                out.push_bytes(if y == 0 { HORIZONTAL } else { DMIDDLE });
                out.push_bytes(HORIZONTAL.repeat(self.half() - 1));
                out.push_bytes(RTCORNER);
            } else {
                if !Self::has_adjacent_nodes(root, x, y) {
                    continue;
                }
                out.push_bytes(" ".repeat(self.node_render_width));
            }
        }
        out.push(b'\n');
    }

    fn render_bottom_layer(&self, root: &RenderTree, out: &mut ByteString, y: usize) {
        for x in 0..=root.width {
            if x * self.node_render_width >= self.maximum_render_width {
                break;
            }
            let has_adjacent_nodes = Self::has_adjacent_nodes(root, x, y);
            if root.node(x, y).is_some() {
                out.push_bytes(LDCORNER);
                out.push_bytes(HORIZONTAL.repeat(self.half() - 1));
                out.push_bytes(if root.has_node(x, y + 1) {
                    TMIDDLE
                } else {
                    HORIZONTAL
                });
                out.push_bytes(HORIZONTAL.repeat(self.half() - 1));
                out.push_bytes(RDCORNER);
            } else if root.has_node(x, y + 1) {
                out.push_bytes(" ".repeat(self.half()));
                out.push_bytes(VERTICAL);
                if has_adjacent_nodes || should_render_whitespace(root, x, y) {
                    out.push_bytes(" ".repeat(self.half()));
                }
            } else if has_adjacent_nodes || should_render_whitespace(root, x, y) {
                out.push_bytes(" ".repeat(self.node_render_width));
            }
        }
        out.push(b'\n');
    }

    /// Adds thousand separators (DuckDB's `FormatNumber`).
    fn format_number(&self, input: &[u8]) -> ByteString {
        if self.decimal_separator.is_none() && self.thousand_separator.is_none() {
            return input.into();
        }
        let character_count = input.iter().take_while(|b| b.is_ascii_digit()).count();
        let mut separator_position = if character_count % 3 == 0 {
            3
        } else {
            character_count % 3
        };
        let mut result = ByteString::new();
        let mut encoded = [0; 4];
        for (c, &byte) in input.iter().enumerate().take(character_count) {
            if c == separator_position
                && let Some(separator) = self.thousand_separator
            {
                result.push_bytes(separator.encode_utf8(&mut encoded));
                separator_position += 3;
            }
            result.push(byte);
        }
        for &byte in &input[character_count..] {
            if byte == b'.'
                && let Some(separator) = self.decimal_separator
            {
                result.push_bytes(separator.encode_utf8(&mut encoded));
            } else {
                result.push(byte);
            }
        }
        result
    }

    fn cardinality_text(&self, prefix: &str, value: &[u8]) -> ByteString {
        byte_format!(
            "{prefix}{} row{}",
            self.format_number(value),
            if value == b"1" { "" } else { "s" }
        )
    }

    fn render_box_content(&self, root: &RenderTree, out: &mut ByteString, y: usize) -> Result<()> {
        let mut extra_info: Vec<Vec<ByteString>> = vec![Vec::new(); root.width];
        let mut extra_height = 0;
        for (x, info) in extra_info.iter_mut().enumerate() {
            if let Some(node) = root.node(x, y) {
                self.split_up_extra_info(&node.extra_text, info)?;
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
                out.push_bytes(VERTICAL);
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
                        render_text = byte_format!("({timing})");
                    }
                    if !has_cardinality
                        && let Some(value) = node.extra_text.get(RenderTreeNode::ESTIMATED_CARDINALITY)
                    {
                        render_text = self.cardinality_text("~", value);
                    }
                }
                out.push_bytes(&adjust_text_for_rendering(
                    &render_text,
                    self.node_render_width - 2,
                )?);
                if render_y == halfway_point && node.child_positions.len() > 1 {
                    out.push_bytes(LMIDDLE);
                } else {
                    out.push_bytes(VERTICAL);
                }
            }
            out.push(b'\n');
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn render_empty_spot(
        &self,
        root: &RenderTree,
        out: &mut ByteString,
        x: usize,
        y: usize,
        render_y: usize,
        halfway_point: usize,
        has_adjacent_nodes: bool,
    ) {
        if render_y == halfway_point {
            let has_child_to_the_right = should_render_whitespace(root, x, y);
            if root.has_node(x, y + 1) {
                out.push_bytes(HORIZONTAL.repeat(self.half()));
                if has_child_to_the_right {
                    out.push_bytes(TMIDDLE);
                    out.push_bytes(HORIZONTAL.repeat(self.half()));
                } else {
                    out.push_bytes(RTCORNER);
                    if has_adjacent_nodes {
                        out.push_bytes(" ".repeat(self.half()));
                    }
                }
            } else if has_child_to_the_right {
                out.push_bytes(HORIZONTAL.repeat(self.node_render_width));
            } else if has_adjacent_nodes {
                out.push_bytes(" ".repeat(self.node_render_width));
            }
        } else if render_y >= halfway_point {
            if root.has_node(x, y + 1) {
                out.push_bytes(" ".repeat(self.half()));
                out.push_bytes(VERTICAL);
                if has_adjacent_nodes || should_render_whitespace(root, x, y) {
                    out.push_bytes(" ".repeat(self.half()));
                }
            } else if has_adjacent_nodes || should_render_whitespace(root, x, y) {
                out.push_bytes(" ".repeat(self.node_render_width));
            }
        } else if has_adjacent_nodes {
            out.push_bytes(" ".repeat(self.node_render_width));
        }
    }

    /// Breaks a line into pieces that fit a box (DuckDB's `SplitStringBuffer`).
    fn split_string_buffer(&self, source: &[u8], result: &mut Vec<ByteString>) -> Result<()> {
        let max_line_render_size = self.node_render_width - 2;
        let mut character_pos = 0;
        let mut start_pos = 0;
        let mut render_width = 0;
        let mut last_possible_split = 0;
        while character_pos < source.len() {
            let (next_character_pos, char_render_width) = unicode::cluster(source, character_pos)?;
            if render_width + char_render_width > max_line_render_size {
                if start_pos + 8 > last_possible_split {
                    last_possible_split = character_pos;
                }
                result.push(source[start_pos..last_possible_split].into());
                render_width = character_pos - last_possible_split;
                start_pos = last_possible_split;
                character_pos = last_possible_split;
            }
            if can_split_on(source[character_pos]) {
                last_possible_split = character_pos;
            }
            character_pos = next_character_pos;
            render_width += char_render_width;
        }
        if source.len() > start_pos {
            result.push(source[start_pos..].into());
        }
        Ok(())
    }

    /// The lines of a node's key-value text (DuckDB's `SplitUpExtraInfo`).
    fn split_up_extra_info(&self, extra_info: &RenderParams, result: &mut Vec<ByteString>) -> Result<()> {
        if extra_info.is_empty()
            || extra_info
                .iter()
                .any(|(_, value)| std::str::from_utf8(value).is_err())
        {
            return Ok(());
        }
        result.push(HORIZONTAL.repeat(self.node_render_width - 9).into());
        let mut requires_padding = false;
        let mut was_inlined = false;
        for (key, value) in extra_info.iter() {
            let trimmed = remove_padding(value);
            if trimmed.is_empty() {
                continue;
            }
            let mut text = ByteString::from(trimmed);
            let mut is_inlined = false;
            if !key.starts_with(b"__") {
                let available_width = self.node_render_width - 7;
                let total_size = key.len() + text.len() + 2;
                let is_multiline = text.contains(&b'\n');
                if !is_multiline && total_size < available_width {
                    text = byte_format!("{key}: {text}");
                    is_inlined = true;
                } else {
                    text = byte_format!("{key}:\n{text}");
                }
            }
            if is_inlined && was_inlined {
                requires_padding = false;
            }
            if requires_padding {
                result.push(ByteString::new());
            }
            if key == RenderTreeNode::CARDINALITY {
                result.push(ByteString::new());
                if extra_info.contains_key(RenderTreeNode::TIMING) {
                    result.push(ByteString::new());
                }
                continue;
            }
            if key == RenderTreeNode::ESTIMATED_CARDINALITY {
                if extra_info.contains_key(RenderTreeNode::CARDINALITY) {
                    result.pop();
                    continue;
                }
                result.push(ByteString::new());
                continue;
            }
            let mut splits: Vec<&[u8]> = split_newlines(&text);
            if splits.len() > self.max_extra_lines {
                let half = self.max_extra_lines / 2;
                let mut truncated: Vec<&[u8]> = splits[..half].to_vec();
                truncated.push(b"...");
                truncated.extend_from_slice(&splits[splits.len() - half..]);
                splits = truncated;
            }
            for split in splits {
                self.split_string_buffer(split, result)?;
            }
            requires_padding = true;
            was_inlined = is_inlined;
        }
        Ok(())
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

// StringUtil::Split(string, string) omits empty pieces, and preserves the input
// as one piece when every piece is empty.
fn split_newlines(text: &[u8]) -> Vec<&[u8]> {
    let parts: Vec<_> = text
        .split(|byte| *byte == b'\n')
        .filter(|part| !part.is_empty())
        .collect();
    if parts.is_empty() { vec![text] } else { parts }
}

/// Appends a JSON string literal the way yyjson writes it.
fn json_string(out: &mut ByteString, text: &[u8]) -> Result<()> {
    let text = text.split(|b| *b == 0).next().unwrap_or_default();
    if std::str::from_utf8(text).is_err() {
        return Err(Error::internal(
            "The plan could not be rendered as JSON, yyjson failed",
        ));
    }
    out.push(b'"');
    for &c in text {
        match c {
            b'"' => out.push_bytes(b"\\\""),
            b'\\' => out.push_bytes(b"\\\\"),
            b'\n' => out.push_bytes(b"\\n"),
            b'\r' => out.push_bytes(b"\\r"),
            b'\t' => out.push_bytes(b"\\t"),
            8 => out.push_bytes(b"\\b"),
            12 => out.push_bytes(b"\\f"),
            c if c < 0x20 => {
                out.push_bytes(format!("\\u{c:04X}"));
            }
            c => out.push(c),
        }
    }
    out.push(b'"');
    Ok(())
}

fn indent(out: &mut ByteString, level: usize) {
    out.push_bytes("    ".repeat(level));
}

fn render_json_node(tree: &RenderTree, x: usize, y: usize, out: &mut ByteString, level: usize) -> Result<()> {
    let Some(node) = tree.node(x, y) else {
        out.push_bytes("{}");
        return Ok(());
    };
    out.push_bytes("{\n");
    indent(out, level + 1);
    out.push_bytes("\"name\": ");
    json_string(out, &node.name)?;
    out.push_bytes(",\n");
    indent(out, level + 1);
    out.push_bytes("\"children\": ");
    if node.child_positions.is_empty() {
        out.push_bytes("[]");
    } else {
        out.push_bytes("[\n");
        for (i, &(child_x, child_y)) in node.child_positions.iter().enumerate() {
            indent(out, level + 2);
            render_json_node(tree, child_x, child_y, out, level + 2)?;
            if i + 1 < node.child_positions.len() {
                out.push(b',');
            }
            out.push(b'\n');
        }
        indent(out, level + 1);
        out.push(b']');
    }
    out.push_bytes(",\n");
    indent(out, level + 1);
    out.push_bytes("\"extra_info\": ");
    if node.extra_text.is_empty() {
        out.push_bytes("{}");
    } else {
        out.push_bytes("{\n");
        let count = node.extra_text.len();
        for (i, (key, value)) in node.extra_text.iter().enumerate() {
            indent(out, level + 2);
            json_string(out, key)?;
            out.push_bytes(": ");
            let splits: Vec<&[u8]> = split_newlines(value);
            if splits.len() > 1 {
                out.push_bytes("[\n");
                for (j, split) in splits.iter().enumerate() {
                    indent(out, level + 3);
                    json_string(out, split)?;
                    if j + 1 < splits.len() {
                        out.push(b',');
                    }
                    out.push(b'\n');
                }
                indent(out, level + 2);
                out.push(b']');
            } else {
                json_string(out, value)?;
            }
            if i + 1 < count {
                out.push(b',');
            }
            out.push(b'\n');
        }
        indent(out, level + 1);
        out.push(b'}');
    }
    out.push(b'\n');
    indent(out, level);
    out.push(b'}');
    Ok(())
}

/// Renders the tree as a JSON array holding the root node (DuckDB's `JSONTreeRenderer`).
fn render_json(tree: &RenderTree) -> Result<ByteString> {
    let mut out = ByteString::from("[\n");
    indent(&mut out, 1);
    render_json_node(tree, 0, 0, &mut out, 1)?;
    out.push_bytes("\n]");
    Ok(out)
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
        let text = render(RendererKind::Text, tree).unwrap();
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
        let text = render(RendererKind::Text, tree).unwrap();
        assert!(
            text.windows("├──────────────┐".len())
                .any(|part| part == "├──────────────┐".as_bytes()),
            "{text:?}"
        );
        assert!(
            text.windows("┌─────────────┴─────────────┐┌─────────────┴─────────────┐".len())
                .any(|part| part == "┌─────────────┴─────────────┐┌─────────────┴─────────────┐".as_bytes()),
            "{text:?}"
        );
    }

    #[test]
    fn formats_numbers_with_separators() {
        let renderer = TextTreeRenderer::default();
        assert_eq!(renderer.format_number(b"1234567"), "1,234,567");
        assert_eq!(renderer.format_number(b"123"), "123");
    }
}
