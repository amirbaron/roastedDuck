//! The HTML, Graphviz, YAML and Mermaid tree renderers (DuckDB's
//! `HTMLTreeRenderer`, `GRAPHVIZTreeRenderer`, `YAMLTreeRenderer` and
//! `MermaidTreeRenderer`).

use std::fmt::Write as _;

use super::{RenderTree, RenderTreeNode};

/// Escapes text for a JSON string (DuckDB's `QueryProfiler::JSONSanitize`).
pub(super) fn json_sanitize(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\u{8}' => result.push_str("\\b"),
            '\u{c}' => result.push_str("\\f"),
            '\n' => result.push_str("\\n"),
            '\r' => result.push_str("\\r"),
            '\t' => result.push_str("\\t"),
            '"' => result.push_str("\\\""),
            '\\' => result.push_str("\\\\"),
            c => result.push(c),
        }
    }
    result
}

/// `RenderTree` as a Graphviz digraph (DuckDB's `GRAPHVIZTreeRenderer::ToStreamInternal`).
pub(super) fn render_graphviz(tree: &RenderTree) -> String {
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    for y in 0..tree.height {
        for x in 0..tree.width {
            let Some(node) = tree.node(x, y) else {
                continue;
            };
            let mut body = vec![node.name.clone()];
            for (key, value) in node.extra_text.iter() {
                body.push(format!("{key}:\\n{}", json_sanitize(value)));
            }
            nodes.push(format!(
                "    node_{x}_{y} [label=\"{}\"];",
                body.join("\\n───\\n")
            ));
            for (child_x, child_y) in &node.child_positions {
                edges.push(format!("    node_{x}_{y} -> node_{child_x}_{child_y};"));
            }
        }
    }
    format!(
        "\ndigraph G {{\n    node [shape=box, style=rounded, fontname=\"Courier New\", fontsize=10];\n{}\n{}\n}}\n\t",
        nodes.join("\n"),
        edges.join("\n")
    )
}

/// A string as a double-quoted YAML scalar (DuckDB's `EscapedString`).
fn yaml_escaped(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn render_yaml_node(tree: &RenderTree, out: &mut String, indent: usize, x: usize, y: usize) {
    let Some(node) = tree.node(x, y) else {
        return;
    };
    let _ = writeln!(out, "{}- name: {}", " ".repeat(indent), yaml_escaped(&node.name));
    let indent = indent + 2;
    for (key, value) in node.extra_text.iter() {
        let splits: Vec<&str> = value.split('\n').collect();
        let _ = write!(out, "{}{}:", " ".repeat(indent), yaml_escaped(key));
        if splits.len() > 1 {
            out.push('\n');
            for split in splits {
                let _ = writeln!(out, "{}- {}", " ".repeat(indent + 2), yaml_escaped(split));
            }
        } else {
            let _ = writeln!(out, " {}", yaml_escaped(value));
        }
    }
    if !node.child_positions.is_empty() {
        let _ = writeln!(out, "{}children: ", " ".repeat(indent));
        for &(child_x, child_y) in &node.child_positions {
            render_yaml_node(tree, out, indent + 2, child_x, child_y);
        }
    }
}

/// `RenderTree` as YAML (DuckDB's `YAMLTreeRenderer::ToStreamInternal`).
pub(super) fn render_yaml(tree: &RenderTree) -> String {
    let mut out = String::new();
    render_yaml_node(tree, &mut out, 0, 0, 0);
    out
}

/// Escapes text for a Mermaid label (DuckDB's `SanitizeMermaidLabel`).
fn sanitize_mermaid_label(text: &str) -> String {
    let mut result = String::with_capacity(text.len() * 2);
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '`' => result.push_str("\\`"),
            '"' => result.push_str("\\\""),
            '\\' if chars.peek() == Some(&'n') => {
                chars.next();
                result.push_str("\n\t");
            }
            c => result.push(c),
        }
    }
    result
}

/// `RenderTree` as a Mermaid flowchart (DuckDB's `MermaidTreeRenderer::ToStreamInternal`).
pub(super) fn render_mermaid(tree: &RenderTree) -> String {
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    for y in 0..tree.height {
        for x in 0..tree.width {
            let Some(node) = tree.node(x, y) else {
                continue;
            };
            let mut extra_info = String::new();
            for (key, value) in node.extra_text.iter() {
                let _ = write!(
                    extra_info,
                    "\n\t{key}: {}",
                    sanitize_mermaid_label(&json_sanitize(value))
                );
            }
            nodes.push(format!(
                "    node_{x}_{y}[\"`**{}**{extra_info}`\"]",
                sanitize_mermaid_label(node.name.trim())
            ));
            for (child_x, child_y) in &node.child_positions {
                edges.push(format!("    node_{x}_{y} --> node_{child_x}_{child_y}"));
            }
        }
    }
    let mut out = String::from("flowchart TD\n");
    for node in nodes {
        out.push_str(&node);
        out.push_str("\n\n");
    }
    for edge in edges {
        out.push_str(&edge);
        out.push('\n');
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
fn html_item_content(node: &RenderTreeNode) -> String {
    let mut items = Vec::new();
    for (key, value) in node.extra_text.iter() {
        if value.is_empty() {
            continue;
        }
        items.push(format!("                <div class=\"sub-title\">{key}</div>"));
        for split in value.split('\n') {
            items.push(format!("                <div class=\"value\">{split}</div>"));
        }
    }
    let mut result = String::new();
    if !items.is_empty() {
        result = format!(
            "\n            <div class=\"content\">\n{}\n            </div>\n    ",
            items.join("\n")
        );
    }
    if !node.child_positions.is_empty() {
        result.push_str("<button class=\"collapse_button\", onclick=\"toggleDisplay(this)\">-</button>");
    }
    result
}

/// DuckDB's `CreateTreeRecursive`.
fn html_tree(tree: &RenderTree, x: usize, y: usize, out: &mut String) {
    out.push_str("<li>");
    if let Some(node) = tree.node(x, y) {
        let _ = write!(
            out,
            "\n        <div class=\"tf-nc\">\n            <div class=\"title\">{}</div>{}\n        </div>\n    ",
            node.name,
            html_item_content(node)
        );
        if !node.child_positions.is_empty() {
            out.push_str("<ul class=\"list-inline\">");
            for &(child_x, child_y) in &node.child_positions {
                html_tree(tree, child_x, child_y, out);
            }
            out.push_str("</ul>");
        }
    }
    out.push_str("</li>");
}

/// `RenderTree` as an HTML page (DuckDB's `HTMLTreeRenderer::ToStreamInternal`).
pub(super) fn render_html(tree: &RenderTree) -> String {
    let mut body = String::new();
    html_tree(tree, 0, 0, &mut body);
    format!(
        "\n<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n    <meta charset=\"UTF-8\">\n    <meta name=\"viewport\" content=\"width=device-width, initial-scale=1.0\">\n    <link rel=\"stylesheet\" href=\"https://unpkg.com/treeflex/dist/css/treeflex.css\">\n    <title>DuckDB Query Plan</title>\n    {HTML_STYLE}\n</head>\n    \n<body>\n    <div class=\"tf-tree\">\n        <ul>{body}</ul>\n    </div>\n{HTML_SCRIPT}\n</body>\n</html>\n    "
    )
}
