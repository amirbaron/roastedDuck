//! In-order iteration over the leaves of an ART for range scans (DuckDB's `Iterator`).

use std::collections::BTreeSet;

use roastedduck_common::{Error, Result};

use super::Art;
use super::key::{ROW_ID_SIZE, decode_row_id};
use super::node::{GateStatus, NType, Node};

/// Whether a scan finished or stopped because its output was full (`ARTScanResult`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScanResult {
    Completed,
    Paused,
}

/// Receives the row ids of a scan.
pub trait ScanOutput {
    fn is_full(&self) -> bool;
    fn add(&mut self, row_id: i64);
}

/// Collects row ids into a set, up to a capacity (`RowIdSetOutput`).
#[derive(Debug)]
pub struct RowIdSetOutput<'a> {
    row_ids: &'a mut BTreeSet<i64>,
    capacity: usize,
}

impl<'a> RowIdSetOutput<'a> {
    pub fn new(row_ids: &'a mut BTreeSet<i64>, capacity: usize) -> Self {
        Self { row_ids, capacity }
    }
}

impl ScanOutput for RowIdSetOutput<'_> {
    fn is_full(&self) -> bool {
        self.row_ids.len() >= self.capacity
    }

    fn add(&mut self, row_id: i64) {
        self.row_ids.insert(row_id);
    }
}

/// Where a paused scan resumes inside a leaf.
#[derive(Debug, Default)]
struct ResumeState {
    cached_row_ids: Vec<i64>,
    cached_position: usize,
    has_cached_row_ids: bool,
    nested_started: bool,
    nested_byte: u8,
}

/// Walks the leaves of an ART in key order.
#[derive(Debug)]
pub struct ArtIterator<'a> {
    art: &'a Art,
    /// The key bytes leading to the top node of the stack.
    current_key: Vec<u8>,
    /// Nodes from the root to the current node, with the byte taken in each.
    nodes: Vec<(Node, u8)>,
    last_leaf: Node,
    /// The row id bytes of a nested leaf.
    row_id: [u8; ROW_ID_SIZE],
    status: GateStatus,
    nested_depth: usize,
    entered_nested_leaf: bool,
    resume: ResumeState,
}

impl<'a> ArtIterator<'a> {
    pub fn new(art: &'a Art) -> Self {
        Self {
            art,
            current_key: Vec::new(),
            nodes: Vec::new(),
            last_leaf: Node::default(),
            row_id: [0; ROW_ID_SIZE],
            status: GateStatus::NotSet,
            nested_depth: 0,
            entered_nested_leaf: false,
            resume: ResumeState::default(),
        }
    }

    /// The key bytes of the current leaf (without the nested row id bytes).
    pub fn current_key(&self) -> &[u8] {
        &self.current_key[..self.current_key.len() - self.nested_depth]
    }

    /// `IteratorKey::GreaterThan` for the current key.
    pub fn current_key_greater_than(&self, key: &[u8], equal: bool) -> bool {
        greater_than(&self.current_key, key, equal, self.nested_depth)
    }

    fn push_byte(&mut self, byte: u8) {
        self.current_key.push(byte);
        if self.status == GateStatus::Set {
            self.row_id[self.nested_depth] = byte;
            self.nested_depth += 1;
        }
    }

    /// Descends to the leftmost leaf below `node` (`Iterator::FindMinimum`).
    pub fn find_minimum(&mut self, node: Node) -> Result<()> {
        let mut node = node;
        while node.has_metadata() {
            if node.is_any_leaf() {
                self.last_leaf = node;
                return Ok(());
            }
            if node.gate_status() == GateStatus::Set {
                self.status = GateStatus::Set;
                self.entered_nested_leaf = true;
                self.nested_depth = 0;
            }
            if node.is(NType::Prefix) {
                let bytes = self.art.prefix_bytes(node)?.to_vec();
                for byte in bytes {
                    self.push_byte(byte);
                }
                self.nodes.push((node, 0));
                node = self.art.prefix_child(node)?;
                continue;
            }
            let (byte, child) = self
                .art
                .get_next_child(node, 0)?
                .ok_or_else(|| Error::internal("ART node without children"))?;
            self.push_byte(byte);
            self.nodes.push((node, byte));
            node = child;
        }
        Err(Error::internal(
            "ART Iterator::FindMinimum: Reached node without metadata",
        ))
    }

    /// Positions the iterator at the first leaf with a key `>= key` (`> key` unless
    /// `equal`); false if there is none (`Iterator::LowerBound`).
    pub fn lower_bound(&mut self, node: Node, key: &[u8], equal: bool) -> Result<bool> {
        let mut node = node;
        let mut depth = 0;
        while node.has_metadata() {
            if node.is_any_leaf() || node.gate_status() == GateStatus::Set {
                if !equal && contains(&self.current_key, key) {
                    return self.next();
                }
                if node.gate_status() == GateStatus::Set {
                    self.find_minimum(node)?;
                } else {
                    self.last_leaf = node;
                }
                return Ok(true);
            }
            if !node.is(NType::Prefix) {
                let Some(&key_byte) = key.get(depth) else {
                    // The bound is a prefix of every key below: all are greater.
                    self.find_minimum(node)?;
                    return Ok(true);
                };
                let Some((byte, child)) = self.art.get_next_child(node, key_byte)? else {
                    return self.next();
                };
                self.current_key.push(byte);
                self.nodes.push((node, byte));
                if byte > key_byte {
                    self.find_minimum(child)?;
                    return Ok(true);
                }
                node = child;
                depth += 1;
                continue;
            }
            let bytes = self.art.prefix_bytes(node)?.to_vec();
            self.current_key.extend_from_slice(&bytes);
            self.nodes.push((node, 0));
            for (i, &byte) in bytes.iter().enumerate() {
                let Some(&key_byte) = key.get(depth + i) else {
                    let child = self.art.prefix_child(node)?;
                    self.find_minimum(child)?;
                    return Ok(true);
                };
                if byte < key_byte {
                    return self.next();
                }
                if byte > key_byte {
                    let child = self.art.prefix_child(node)?;
                    self.find_minimum(child)?;
                    return Ok(true);
                }
            }
            depth += bytes.len();
            node = self.art.prefix_child(node)?;
        }
        Err(Error::internal(
            "ART Iterator::LowerBound: Reached node without metadata",
        ))
    }

    /// Moves to the next leaf (`Iterator::Next`).
    fn next(&mut self) -> Result<bool> {
        while let Some(&(node, byte)) = self.nodes.last() {
            if node.is(NType::Prefix) || byte == u8::MAX {
                self.pop_node()?;
                continue;
            }
            let Some((next_byte, child)) = self.art.get_next_child(node, byte + 1)? else {
                self.pop_node()?;
                continue;
            };
            if let Some(top) = self.nodes.last_mut() {
                top.1 = next_byte;
            }
            self.current_key.pop();
            self.current_key.push(next_byte);
            if self.status == GateStatus::Set {
                self.row_id[self.nested_depth - 1] = next_byte;
            }
            self.find_minimum(child)?;
            return Ok(true);
        }
        Ok(false)
    }

    /// `Iterator::PopNode`.
    fn pop_node(&mut self) -> Result<()> {
        let Some((node, _)) = self.nodes.pop() else {
            return Ok(());
        };
        let popped = if node.is(NType::Prefix) {
            self.art.prefix_bytes(node)?.len()
        } else {
            1
        };
        let len = self.current_key.len().saturating_sub(popped);
        self.current_key.truncate(len);
        if self.status == GateStatus::Set {
            self.nested_depth = self.nested_depth.saturating_sub(popped);
        }
        if node.gate_status() == GateStatus::Set {
            self.status = GateStatus::NotSet;
        }
        Ok(())
    }

    /// Emits the row ids of the leaves from the current one on, until the key exceeds
    /// `upper_bound` (empty: no bound) or the output is full (`Iterator::Scan`).
    pub fn scan(
        &mut self,
        upper_bound: &[u8],
        output: &mut impl ScanOutput,
        equal: bool,
    ) -> Result<ScanResult> {
        loop {
            if !upper_bound.is_empty()
                && (self.status == GateStatus::NotSet || self.entered_nested_leaf)
                && greater_than(&self.current_key, upper_bound, equal, self.nested_depth)
            {
                return Ok(ScanResult::Completed);
            }
            let leaf = self.last_leaf;
            match leaf.ntype()? {
                NType::LeafInlined => {
                    if output.is_full() {
                        return Ok(ScanResult::Paused);
                    }
                    output.add(leaf.row_id());
                }
                NType::Leaf => {
                    if !self.resume.has_cached_row_ids {
                        self.resume.cached_row_ids = self.art.deprecated_row_ids(leaf)?;
                        self.resume.cached_row_ids.sort_unstable();
                        self.resume.cached_position = 0;
                        self.resume.has_cached_row_ids = true;
                    }
                    while let Some(&row_id) = self.resume.cached_row_ids.get(self.resume.cached_position) {
                        if output.is_full() {
                            return Ok(ScanResult::Paused);
                        }
                        output.add(row_id);
                        self.resume.cached_position += 1;
                    }
                    self.resume.has_cached_row_ids = false;
                }
                NType::Node7Leaf | NType::Node15Leaf | NType::Node256Leaf => {
                    if !self.resume.nested_started {
                        self.resume.nested_byte = 0;
                        self.resume.nested_started = true;
                    }
                    while let Some(byte) = self.art.get_next_byte(leaf, self.resume.nested_byte)? {
                        self.resume.nested_byte = byte;
                        if output.is_full() {
                            return Ok(ScanResult::Paused);
                        }
                        self.row_id[ROW_ID_SIZE - 1] = byte;
                        output.add(decode_row_id(&self.row_id));
                        if byte == u8::MAX {
                            break;
                        }
                        self.resume.nested_byte = byte + 1;
                    }
                    self.resume.nested_started = false;
                }
                _ => return Err(Error::internal("Invalid leaf type for index scan.")),
            }
            self.entered_nested_leaf = false;
            if !self.next()? {
                return Ok(ScanResult::Completed);
            }
        }
    }
}

/// `IteratorKey::Contains`.
fn contains(current: &[u8], key: &[u8]) -> bool {
    current.len() >= key.len() && current[..key.len()] == *key
}

/// `IteratorKey::GreaterThan`.
fn greater_than(current: &[u8], key: &[u8], equal: bool, nested_depth: usize) -> bool {
    for (a, b) in current.iter().zip(key) {
        if a > b {
            return true;
        }
        if a < b {
            return false;
        }
    }
    let this_len = current.len() - nested_depth;
    if equal {
        this_len > key.len()
    } else {
        this_len >= key.len()
    }
}
