//! Compare native renderer outputs with frozen C++ reference TSV records.
use rustdb_common::ByteString;
use rustdb_common::tree_renderer::{RenderParams, RenderTree, RenderTreeNode, RendererKind, render};
fn unhex(text: &str) -> Result<Vec<u8>, &'static str> {
    fn digit(byte: u8) -> Result<u8, &'static str> {
        match byte {
            b'0'..=b'9' => Ok(byte - b'0'),
            b'a'..=b'f' => Ok(byte - b'a' + 10),
            _ => Err("invalid hex digit"),
        }
    }
    let (pairs, remainder) = text.as_bytes().as_chunks::<2>();
    if !remainder.is_empty() {
        return Err("odd hex length");
    }
    pairs
        .iter()
        .map(|pair| Ok((digit(pair[0])? << 4) | digit(pair[1])?))
        .collect()
}
fn verify(input: &str) -> Result<usize, Box<dyn std::error::Error>> {
    let formats = [
        RendererKind::Text,
        RendererKind::Json,
        RendererKind::Html,
        RendererKind::Graphviz,
        RendererKind::Yaml,
        RendererKind::Mermaid,
    ];
    let mut count = 0;
    let mut mismatches = 0;
    for line in input.lines() {
        let fields: Vec<_> = line.split('\t').collect();
        assert_eq!(fields.len(), 5);
        let id = fields[0];
        let extended = id.starts_with("hex:") || id.starts_with("tree:");
        let branched = id.starts_with("tree:");
        let field: u8 = fields[1].parse()?;
        let format: usize = fields[2].parse()?;
        let sample = if extended {
            unhex(id.split_once(':').ok_or("invalid reference identifier")?.1)?
        } else {
            let mut sample = b"pre".to_vec();
            sample.push(id.parse()?);
            sample.extend_from_slice(b"post");
            sample
        };
        let mut params = RenderParams::new();
        params.insert(
            if field == 1 { sample.as_slice() } else { b"Key" },
            if field == 2 { sample.as_slice() } else { b"Value" },
        );
        if extended {
            params.insert("kEY", "duplicate");
            params.insert("__estimated_cardinality__", "1234");
        }
        let mut tree = RenderTree::new(if branched { 2 } else { 1 }, if branched { 2 } else { 1 });
        let mut root = RenderTreeNode::new(if field == 0 { sample.as_slice() } else { b"NODE" }, params);
        if branched {
            root.child_positions = vec![(0, 1), (1, 1)];
            let mut left = RenderParams::new();
            let mut right = RenderParams::new();
            left.insert("__cardinality__", "1000");
            left.insert("__timing__", "0.12s");
            right.insert("__estimated_cardinality__", "1");
            tree.set_node(0, 1, RenderTreeNode::new("LEFT", left));
            tree.set_node(1, 1, RenderTreeNode::new("RIGHT", right));
        }
        tree.set_node(0, 0, root);
        let (status, actual): (&str, ByteString) = match render(formats[format], tree) {
            Ok(bytes) => ("ok", bytes),
            Err(error) => ("error", error.raw_message_bytes().into()),
        };
        let expected = unhex(fields[4])?;
        if status != fields[3] || actual.as_bytes() != expected {
            if mismatches < 16 {
                let offset = actual.iter().zip(&expected).position(|(a, b)| a != b);
                eprintln!(
                    "case {id}/{field}/{format}: {status}/{}; lengths {}/{}; first difference {offset:?}",
                    fields[3],
                    actual.len(),
                    expected.len()
                );
            }
            mismatches += 1;
        }
        count += 1;
    }
    if mismatches != 0 {
        return Err(format!("{mismatches}/{count} renderer comparisons failed").into());
    }
    Ok(count)
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).ok_or("expected reference TSV path")?;
    let count = verify(&std::fs::read_to_string(path)?)?;
    println!("{count} C++ native renderer comparisons passed");
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn frozen_cpp_renderer_outcomes() {
        let fixture = include_str!("../tests/data/tree_renderer_1.5.6.tsv");
        assert_eq!(super::verify(fixture).unwrap(), fixture.lines().count());
    }
}
