//! The HTML, Graphviz, YAML and Mermaid tree renderers (DuckDB's
//! `HTMLTreeRenderer`, `GRAPHVIZTreeRenderer`, `YAMLTreeRenderer` and
//! `MermaidTreeRenderer`).

use crate::{ByteString, byte_format, byte_write};

use super::{RenderTree, RenderTreeNode, split_newlines};

/// Escapes text for a JSON string (DuckDB's `QueryProfiler::JSONSanitize`).
pub(super) fn json_sanitize(text: &[u8]) -> ByteString {
    let mut result = ByteString::new();
    for &c in text {
        match c {
            8 => result.push_bytes("\\b"),
            12 => result.push_bytes("\\f"),
            b'\n' => result.push_bytes("\\n"),
            b'\r' => result.push_bytes("\\r"),
            b'\t' => result.push_bytes("\\t"),
            b'"' => result.push_bytes("\\\""),
            b'\\' => result.push_bytes("\\\\"),
            c => result.push(c),
        }
    }
    result
}

/// `RenderTree` as a Graphviz digraph (DuckDB's `GRAPHVIZTreeRenderer::ToStreamInternal`).
pub(super) fn render_graphviz(tree: &RenderTree) -> ByteString {
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    for y in 0..tree.height {
        for x in 0..tree.width {
            let Some(node) = tree.node(x, y) else {
                continue;
            };
            let mut body = vec![node.name.clone()];
            for (key, value) in node.extra_text.iter() {
                body.push(byte_format!("{key}:\\n{}", json_sanitize(value)));
            }
            nodes.push(byte_format!(
                "    node_{x}_{y} [label=\"{}\"];",
                ByteString::join(&body, "\\n───\\n")
            ));
            for (child_x, child_y) in &node.child_positions {
                edges.push(byte_format!("    node_{x}_{y} -> node_{child_x}_{child_y};"));
            }
        }
    }
    byte_format!(
        "\ndigraph G {{\n    node [shape=box, style=rounded, fontname=\"Courier New\", fontsize=10];\n{}\n{}\n}}\n\t",
        ByteString::join(&nodes, b"\n"),
        ByteString::join(&edges, b"\n")
    )
}

/// A string as a double-quoted YAML scalar (DuckDB's `EscapedString`).
fn yaml_escaped(text: &[u8]) -> ByteString {
    let mut out = ByteString::new();
    out.push(b'"');
    for &c in text {
        match c {
            8 => out.push_bytes("\\b"),
            12 => out.push_bytes("\\f"),
            b'\n' => out.push_bytes("\\n"),
            b'\r' => out.push_bytes("\\r"),
            b'\t' => out.push_bytes("\\t"),
            b'"' => out.push_bytes("\\\""),
            b'\\' => out.push_bytes("\\\\"),
            c if (c as u32) < 0x20 => {
                out.push_bytes(format!("\\u{c:04x}"));
            }
            c => out.push(c),
        }
    }
    out.push(b'"');
    out
}

fn render_yaml_node(tree: &RenderTree, out: &mut ByteString, indent: usize, x: usize, y: usize) {
    let Some(node) = tree.node(x, y) else {
        return;
    };
    let _ = byte_write!(
        out,
        "{}- name: {}\n",
        " ".repeat(indent),
        yaml_escaped(&node.name)
    );
    let indent = indent + 2;
    for (key, value) in node.extra_text.iter() {
        let splits: Vec<&[u8]> = split_newlines(value);
        let _ = byte_write!(out, "{}{}:", " ".repeat(indent), yaml_escaped(key));
        if splits.len() > 1 {
            out.push(b'\n');
            for split in splits {
                let _ = byte_write!(out, "{}- {}\n", " ".repeat(indent + 2), yaml_escaped(split));
            }
        } else {
            let _ = byte_write!(out, " {}\n", yaml_escaped(value));
        }
    }
    if !node.child_positions.is_empty() {
        let _ = byte_write!(out, "{}children: \n", " ".repeat(indent));
        for &(child_x, child_y) in &node.child_positions {
            render_yaml_node(tree, out, indent + 2, child_x, child_y);
        }
    }
}

/// `RenderTree` as YAML (DuckDB's `YAMLTreeRenderer::ToStreamInternal`).
pub(super) fn render_yaml(tree: &RenderTree) -> ByteString {
    let mut out = ByteString::new();
    render_yaml_node(tree, &mut out, 0, 0, 0);
    out
}

/// Escapes text for a Mermaid label (DuckDB's `SanitizeMermaidLabel`).
fn sanitize_mermaid_label(text: &[u8]) -> ByteString {
    let mut result = ByteString::new();
    let mut chars = text.iter().copied().peekable();
    while let Some(c) = chars.next() {
        match c {
            b'`' => result.push_bytes("\\`"),
            b'"' => result.push_bytes("\\\""),
            b'\\' if chars.peek() == Some(&b'n') => {
                chars.next();
                result.push_bytes("\n\t");
            }
            c => result.push(c),
        }
    }
    result
}

// StringUtil::RTrim tests plain C++ char > 0 before keeping a trailing byte.
// Follow the target C ABI, including removal of trailing non-ASCII bytes on macOS.
fn trim_mermaid_name(mut text: &[u8]) -> &[u8] {
    while text.first().is_some_and(u8::is_ascii_whitespace) {
        text = &text[1..];
    }
    while text
        .last()
        .is_some_and(|byte| (*byte as std::ffi::c_char as i32) <= 0 || byte.is_ascii_whitespace())
    {
        text = &text[..text.len() - 1];
    }
    text
}

/// `RenderTree` as a Mermaid flowchart (DuckDB's `MermaidTreeRenderer::ToStreamInternal`).
pub(super) fn render_mermaid(tree: &RenderTree) -> ByteString {
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    for y in 0..tree.height {
        for x in 0..tree.width {
            let Some(node) = tree.node(x, y) else {
                continue;
            };
            let mut extra_info = ByteString::new();
            for (key, value) in node.extra_text.iter() {
                let _ = byte_write!(
                    extra_info,
                    "\n\t{key}: {}",
                    sanitize_mermaid_label(&json_sanitize(value))
                );
            }
            nodes.push(byte_format!(
                "    node_{x}_{y}[\"`**{}**{extra_info}`\"]",
                sanitize_mermaid_label(trim_mermaid_name(&node.name))
            ));
            for (child_x, child_y) in &node.child_positions {
                edges.push(byte_format!("    node_{x}_{y} --> node_{child_x}_{child_y}"));
            }
        }
    }
    let mut out = ByteString::from("flowchart TD\n");
    for node in nodes {
        out.push_bytes(&node);
        out.push_bytes("\n\n");
    }
    for edge in edges {
        out.push_bytes(&edge);
        out.push(b'\n');
    }
    out
}

const HTML_STYLE: &str = r"
    <style>
        body {
            font-family: Arial, sans-serif;
        }

        .tf-tree .tf-nc {
            padding: 0px;
            border: 1px solid #E5E5E5;
        }

        .tf-nc {
            border-radius: 0.5rem;
            padding: 0px;
            min-width: 150px;
            width: auto;
            background-color: #FAFAFA;
            text-align: center;
            position: relative;
        }

        .collapse_button {
            position:relative;
            color: black;
            z-index: 2;
            width: 2em;
            background-color: white;
            height: 2em;
            border-radius: 50%;
            top: 2.25em;
        }

        .collapse_button:hover {
            background-color: #f0f0f0; /* Light gray */
        }

        .collapse_button:active {
            background-color: #e0e0e0; /* Slightly darker gray */
        }

        .hidden {
            display: none !important;
        }

        .title {
            font-weight: bold;
            padding-bottom: 5px;
            color: #fff100;
            box-sizing: border-box;
            background-color: black;
            border-top-left-radius: 0.5rem;
            border-top-right-radius: 0.5rem;
            padding: 10px;
        }

        .content {
            border-top: 1px solid #000;
            text-align: center;
            border-bottom-left-radius: 0.5rem;
            border-bottom-right-radius: 0.5rem;
            padding: 10px;
        }

        .sub-title {
            color: black;
            font-weight: bold;
            padding-top: 5px;
        }

        .sub-title:not(:first-child) {
            border-top: 1px solid #ADADAD;
        }

        .value {
            margin-left: 10px;
            margin-top: 5px;
            color: #3B3B3B;
            margin-bottom: 5px;
        }

        .tf-tree {
            width: 100%;
            height: 100%;
            overflow: visible;
        }
    </style>
    ";

const HTML_SCRIPT: &str = r"
<script>
function toggleDisplay(button) {
    const parentLi = button.closest('li');
    const nestedUl = parentLi.querySelector('ul');
    if (nestedUl) {
        const currentDisplay = getComputedStyle(nestedUl).getPropertyValue('display');
        if (currentDisplay === 'none') {
            nestedUl.classList.toggle('hidden');
            button.textContent = '-';
        } else {
            nestedUl.classList.toggle('hidden');
            button.textContent = '+';
        }
    }
}
</script>
";

/// The boxes of a node's key-value lines (DuckDB's `CreateGridItemContent`).
fn html_item_content(node: &RenderTreeNode) -> ByteString {
    let mut items = Vec::new();
    for (key, value) in node.extra_text.iter() {
        if value.is_empty() {
            continue;
        }
        items.push(byte_format!(
            "                <div class=\"sub-title\">{key}</div>"
        ));
        for split in split_newlines(value) {
            items.push(byte_format!("                <div class=\"value\">{split}</div>"));
        }
    }
    let mut result = ByteString::new();
    if !items.is_empty() {
        result = byte_format!(
            "\n            <div class=\"content\">\n{}\n            </div>\n    ",
            ByteString::join(&items, b"\n")
        );
    }
    if !node.child_positions.is_empty() {
        result.push_bytes("<button class=\"collapse_button\", onclick=\"toggleDisplay(this)\">-</button>");
    }
    result
}

/// DuckDB's `CreateTreeRecursive`.
fn html_tree(tree: &RenderTree, x: usize, y: usize, out: &mut ByteString) {
    out.push_bytes("<li>");
    if let Some(node) = tree.node(x, y) {
        let _ = byte_write!(
            out,
            "\n        <div class=\"tf-nc\">\n            <div class=\"title\">{}</div>{}\n        </div>\n    ",
            node.name,
            html_item_content(node)
        );
        if !node.child_positions.is_empty() {
            out.push_bytes("<ul class=\"list-inline\">");
            for &(child_x, child_y) in &node.child_positions {
                html_tree(tree, child_x, child_y, out);
            }
            out.push_bytes("</ul>");
        }
    }
    out.push_bytes("</li>");
}

/// `RenderTree` as an HTML page (DuckDB's `HTMLTreeRenderer::ToStreamInternal`).
pub(super) fn render_html(tree: &RenderTree) -> ByteString {
    let mut body = ByteString::new();
    html_tree(tree, 0, 0, &mut body);
    byte_format!(
        "\n<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n    <meta charset=\"UTF-8\">\n    <meta name=\"viewport\" content=\"width=device-width, initial-scale=1.0\">\n    <link rel=\"stylesheet\" href=\"https://unpkg.com/treeflex/dist/css/treeflex.css\">\n    <title>DuckDB Query Plan</title>\n    {HTML_STYLE}\n</head>\n    \n<body>\n    <div class=\"tf-tree\">\n        <ul>{body}</ul>\n    </div>\n{HTML_SCRIPT}\n</body>\n</html>\n    "
    )
}
