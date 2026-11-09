//! ART node pointers, node types and the byte layouts of the nodes (DuckDB's `Node`,
//! `NType` and the node structs).
//!
//! A node is an [`IndexPointer`] whose metadata byte holds the node type (low 7 bits) and
//! the gate flag (high bit). Inlined leaves store their row id in the pointer itself. All
//! other nodes live in segments of the ART's fixed-size allocators, laid out exactly like
//! DuckDB's structs (little-endian, natural alignment):
//!
//! | node           | layout                                                      |
//! |----------------|-------------------------------------------------------------|
//! | `Node4`        | count u8, key[4], pad, children[4] (offset 8), 40 bytes     |
//! | `Node16`       | count u8, key[16], pad, children[16] (offset 24), 152 bytes |
//! | `Node48`       | count u8, child_index[256], pad, children[48] (offset 264)  |
//! | `Node256`      | count u16, pad, children[256] (offset 8), 2056 bytes        |
//! | `Node7Leaf`    | count u8, key[7], 8 bytes                                   |
//! | `Node15Leaf`   | count u8, key[15], 16 bytes                                 |
//! | `Node256Leaf`  | count u16, pad, mask[32 x u64] (offset 8), 264 bytes        |
//! | `Leaf` (old)   | count u8, pad, row_ids[4] (offset 8), next (offset 40)      |
//! | `Prefix`       | data[prefix_count], count u8, child (unaligned)             |

use roastedduck_common::{Error, Result};

use crate::index::IndexPointer;

/// A node of the ART.
pub type Node = IndexPointer;

/// The node types (DuckDB's `NType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum NType {
    Prefix = 1,
    /// Deprecated leaf storing up to four row ids per segment (storage before v1.2).
    Leaf = 2,
    Node4 = 3,
    Node16 = 4,
    Node48 = 5,
    Node256 = 6,
    LeafInlined = 7,
    Node7Leaf = 8,
    Node15Leaf = 9,
    Node256Leaf = 10,
}

impl NType {
    pub const fn from_u8(value: u8) -> Option<Self> {
        Some(match value {
            1 => Self::Prefix,
            2 => Self::Leaf,
            3 => Self::Node4,
            4 => Self::Node16,
            5 => Self::Node48,
            6 => Self::Node256,
            7 => Self::LeafInlined,
            8 => Self::Node7Leaf,
            9 => Self::Node15Leaf,
            10 => Self::Node256Leaf,
            _ => return None,
        })
    }

    /// The allocator of nodes of this type (`Node::GetAllocatorIdx`).
    pub const fn allocator_index(self) -> Option<usize> {
        Some(match self {
            Self::Prefix => 0,
            Self::Leaf => 1,
            Self::Node4 => 2,
            Self::Node16 => 3,
            Self::Node48 => 4,
            Self::Node256 => 5,
            Self::Node7Leaf => 6,
            Self::Node15Leaf => 7,
            Self::Node256Leaf => 8,
            Self::LeafInlined => return None,
        })
    }

    /// Segment sizes of the allocators other than the prefix allocator, in allocator order.
    pub const fn segment_size(self) -> usize {
        match self {
            Self::Leaf => 48,
            Self::Node4 => 40,
            Self::Node16 => 152,
            Self::Node48 => 648,
            Self::Node256 => 2056,
            Self::Node7Leaf => 8,
            Self::Node15Leaf => 16,
            Self::Node256Leaf => 264,
            Self::Prefix | Self::LeafInlined => 0,
        }
    }
}

/// Number of allocators of an ART.
pub const ALLOCATOR_COUNT: usize = 9;
/// Number of allocators serialized in the v1.0.0 format (no nested leaves).
pub const DEPRECATED_ALLOCATOR_COUNT: usize = ALLOCATOR_COUNT - 3;

/// Whether a node is the root of a nested ART of row ids (DuckDB's `GateStatus`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GateStatus {
    NotSet,
    Set,
}

impl GateStatus {
    #[must_use]
    pub const fn flipped(self) -> Self {
        match self {
            Self::NotSet => Self::Set,
            Self::Set => Self::NotSet,
        }
    }
}

const AND_GATE: u8 = 0x80;
const AND_ROW_ID: u64 = 0x00FF_FFFF_FFFF_FFFF;

impl IndexPointer {
    /// An inlined leaf holding `row_id` (`Leaf::New`).
    #[must_use]
    pub fn inlined_leaf(row_id: i64) -> Self {
        let mut node = Self::from_raw((row_id as u64) & AND_ROW_ID);
        node.set_metadata(NType::LeafInlined as u8);
        node
    }

    /// The raw type bits.
    pub const fn type_bits(self) -> u8 {
        self.metadata() & !AND_GATE
    }

    /// Whether the node has type `ty` (an empty node has no type).
    pub const fn is(self, ty: NType) -> bool {
        self.type_bits() == ty as u8
    }

    /// The node type; an error for corrupted or empty nodes.
    pub fn ntype(self) -> Result<NType> {
        NType::from_u8(self.type_bits())
            .ok_or_else(|| Error::internal(format!("invalid ART node type {}", self.type_bits())))
    }

    pub fn set_type(&mut self, ty: NType) {
        self.set_metadata(ty as u8);
    }

    pub const fn gate_status(self) -> GateStatus {
        if self.metadata() & AND_GATE == 0 {
            GateStatus::NotSet
        } else {
            GateStatus::Set
        }
    }

    pub fn set_gate_status(&mut self, status: GateStatus) {
        let metadata = self.metadata();
        match status {
            GateStatus::Set => self.set_metadata(metadata | AND_GATE),
            GateStatus::NotSet => self.set_metadata(metadata & !AND_GATE),
        }
    }

    /// The row id of an inlined leaf.
    pub const fn row_id(self) -> i64 {
        (self.raw() & AND_ROW_ID) as i64
    }

    /// Whether this is an inner node (`Node::IsNode`).
    pub const fn is_inner(self) -> bool {
        matches!(self.type_bits(), 3..=6)
    }

    /// Whether this is a nested leaf node (`Node::IsLeafNode`).
    pub const fn is_leaf_node(self) -> bool {
        matches!(self.type_bits(), 8..=10)
    }

    /// Whether this is any kind of leaf (`Node::IsAnyLeaf`).
    pub const fn is_any_leaf(self) -> bool {
        matches!(self.type_bits(), 2 | 7..=10)
    }
}

/// The location of a node pointer: the ART root, a child slot inside a segment, or a
/// placeholder that reads as empty (DuckDB's `Node &` references).
#[derive(Clone, Copy, Debug)]
pub enum Slot {
    Root,
    /// Bytes `offset..offset + 8` of the segment of `owner`.
    In {
        owner: Node,
        offset: u16,
    },
    /// An empty placeholder (`Node empty;` in `ARTOperator::Delete`).
    Dummy,
}

// Layout constants.
pub const NODE4_CAPACITY: usize = 4;
pub const NODE16_CAPACITY: usize = 16;
pub const NODE48_CAPACITY: usize = 48;
pub const NODE256_CAPACITY: usize = 256;
pub const NODE7_LEAF_CAPACITY: usize = 7;
pub const NODE15_LEAF_CAPACITY: usize = 15;
/// `Node48::EMPTY_MARKER`.
pub const NODE48_EMPTY: u8 = 48;
/// `Node48::SHRINK_THRESHOLD`.
pub const NODE48_SHRINK: usize = 12;
/// `Node256::SHRINK_THRESHOLD`.
pub const NODE256_SHRINK: usize = 36;
/// Offset of the children of a `Node48`.
pub const NODE48_CHILDREN: usize = 264;
/// Offset of the children of a `Node256` and the mask of a `Node256Leaf`.
pub const NODE256_CHILDREN: usize = 8;
/// Row ids per deprecated leaf segment (`Leaf::LEAF_SIZE`).
pub const LEAF_SIZE: usize = 4;
/// Offset of the row ids of a deprecated leaf.
pub const LEAF_ROW_IDS: usize = 8;
/// Offset of the next pointer of a deprecated leaf.
pub const LEAF_NEXT: usize = 40;

/// Offset of the children of a `Node4` / `Node16`.
pub const fn base_node_children(capacity: usize) -> usize {
    (1 + capacity).div_ceil(8) * 8
}

pub fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    let mut raw = [0u8; 8];
    raw.copy_from_slice(&bytes[offset..offset + 8]);
    u64::from_le_bytes(raw)
}

pub fn write_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

pub fn read_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

pub fn write_u16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

pub fn read_node(bytes: &[u8], offset: usize) -> Node {
    Node::from_raw(read_u64(bytes, offset))
}

pub fn write_node(bytes: &mut [u8], offset: usize, node: Node) {
    write_u64(bytes, offset, node.raw());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_constants() {
        assert_eq!(base_node_children(4), 8);
        assert_eq!(base_node_children(16), 24);
        assert_eq!(base_node_children(4) + 8 * 4, NType::Node4.segment_size());
        assert_eq!(base_node_children(16) + 8 * 16, NType::Node16.segment_size());
        assert_eq!(NODE48_CHILDREN + 8 * 48, NType::Node48.segment_size());
    }

    #[test]
    fn inlined_leaves_and_gates() {
        let mut node = Node::inlined_leaf(123_456);
        assert!(node.is(NType::LeafInlined));
        assert_eq!(node.row_id(), 123_456);
        node.set_type(NType::Node4);
        node.set_gate_status(GateStatus::Set);
        assert!(node.is(NType::Node4));
        assert_eq!(node.gate_status(), GateStatus::Set);
        assert!(!Node::default().has_metadata());
    }
}
