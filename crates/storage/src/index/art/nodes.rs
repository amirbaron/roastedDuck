//! Operations on inner nodes and nested leaf nodes: child lookup, insertion with growth,
//! deletion with shrinking, and freeing subtrees (DuckDB's `Node`, `BaseNode`, `Node48`,
//! `Node256`, `BaseLeaf` and `Node256Leaf`).

use rustdb_common::{Error, Result};

use super::Art;
use super::key::ROW_ID_SIZE;
use super::node::{
    GateStatus, LEAF_NEXT, LEAF_ROW_IDS, NODE4_CAPACITY, NODE7_LEAF_CAPACITY, NODE15_LEAF_CAPACITY,
    NODE16_CAPACITY, NODE48_CAPACITY, NODE48_CHILDREN, NODE48_EMPTY, NODE48_SHRINK, NODE256_CAPACITY,
    NODE256_CHILDREN, NODE256_SHRINK, NType, Node, Slot, base_node_children, read_node, read_u16, read_u64,
    write_node, write_u16, write_u64,
};

fn slot_of(owner: Node, offset: usize) -> Slot {
    Slot::In {
        owner,
        // segment offsets are below 2056
        offset: offset as u16,
    }
}

const fn base_capacity(ty: NType) -> usize {
    match ty {
        NType::Node4 => NODE4_CAPACITY,
        _ => NODE16_CAPACITY,
    }
}

fn mask_is_set(bytes: &[u8], byte: u8) -> bool {
    let word = read_u64(bytes, NODE256_CHILDREN + usize::from(byte / 64) * 8);
    word & (1 << (byte % 64)) != 0
}

fn mask_set(bytes: &mut [u8], byte: u8, valid: bool) {
    let offset = NODE256_CHILDREN + usize::from(byte / 64) * 8;
    let mut word = read_u64(bytes, offset);
    if valid {
        word |= 1 << (byte % 64);
    } else {
        word &= !(1 << (byte % 64));
    }
    write_u64(bytes, offset, word);
}

impl Art {
    /// Allocates an empty node of type `ty` (`Node::New`).
    pub(crate) fn new_node(&mut self, ty: NType) -> Result<Node> {
        let node = self.alloc(ty)?;
        if ty == NType::Node48 {
            self.seg_mut(node)?[1..=NODE256_CAPACITY].fill(NODE48_EMPTY);
        }
        Ok(node)
    }

    /// The slot of the child of inner node `node` at `byte` (`Node::GetChild`).
    pub(crate) fn get_child(&self, node: Node, byte: u8) -> Result<Option<Slot>> {
        let ty = node.ntype()?;
        let bytes = self.seg(node)?;
        let offset = match ty {
            NType::Node4 | NType::Node16 => {
                let children = base_node_children(base_capacity(ty));
                let count = usize::from(bytes[0]);
                match (0..count).find(|&i| bytes[1 + i] == byte) {
                    Some(i) => children + 8 * i,
                    None => return Ok(None),
                }
            }
            NType::Node48 => {
                let index = bytes[1 + usize::from(byte)];
                if index == NODE48_EMPTY {
                    return Ok(None);
                }
                NODE48_CHILDREN + 8 * usize::from(index)
            }
            NType::Node256 => {
                let offset = NODE256_CHILDREN + 8 * usize::from(byte);
                if !read_node(bytes, offset).has_metadata() {
                    return Ok(None);
                }
                offset
            }
            _ => {
                return Err(Error::internal(format!(
                    "Invalid node type for GetChildInternal: {ty:?}."
                )));
            }
        };
        if !read_node(bytes, offset).has_metadata() {
            return Err(Error::internal(format!(
                "empty child for byte {byte} in {ty:?}::GetChild"
            )));
        }
        Ok(Some(slot_of(node, offset)))
    }

    /// The first child of inner node `node` at a byte `>= byte`, with that byte
    /// (`Node::GetNextChild`).
    pub(crate) fn get_next_child(&self, node: Node, byte: u8) -> Result<Option<(u8, Node)>> {
        let ty = node.ntype()?;
        let bytes = self.seg(node)?;
        match ty {
            NType::Node4 | NType::Node16 => {
                let children = base_node_children(base_capacity(ty));
                let count = usize::from(bytes[0]);
                Ok((0..count)
                    .find(|&i| bytes[1 + i] >= byte)
                    .map(|i| (bytes[1 + i], read_node(bytes, children + 8 * i))))
            }
            NType::Node48 => Ok((usize::from(byte)..NODE256_CAPACITY)
                .find(|&b| bytes[1 + b] != NODE48_EMPTY)
                .map(|b| {
                    let index = usize::from(bytes[1 + b]);
                    (b as u8, read_node(bytes, NODE48_CHILDREN + 8 * index))
                })),
            NType::Node256 => Ok((usize::from(byte)..NODE256_CAPACITY)
                .map(|b| (b, read_node(bytes, NODE256_CHILDREN + 8 * b)))
                .find(|(_, child)| child.has_metadata())
                .map(|(b, child)| (b as u8, child))),
            _ => Err(Error::internal(format!(
                "Invalid node type for GetNextChildInternal: {ty:?}."
            ))),
        }
    }

    /// The children of inner node `node` in key order.
    pub(crate) fn children(&self, node: Node) -> Result<Vec<(u8, Node)>> {
        let mut children = Vec::new();
        let mut byte = 0u8;
        while let Some((found, child)) = self.get_next_child(node, byte)? {
            children.push((found, child));
            if found == u8::MAX {
                break;
            }
            byte = found + 1;
        }
        Ok(children)
    }

    /// Whether nested leaf node `node` holds `byte` (`Node::HasByte`).
    pub(crate) fn has_byte(&self, node: Node, byte: u8) -> Result<bool> {
        let ty = node.ntype()?;
        let bytes = self.seg(node)?;
        match ty {
            NType::Node7Leaf | NType::Node15Leaf => {
                let count = usize::from(bytes[0]);
                Ok(bytes[1..=count].contains(&byte))
            }
            NType::Node256Leaf => Ok(mask_is_set(bytes, byte)),
            _ => Err(Error::internal(format!("Invalid node type for HasByte: {ty:?}."))),
        }
    }

    /// The first byte `>= byte` of nested leaf node `node` (`Node::GetNextByte`).
    pub(crate) fn get_next_byte(&self, node: Node, byte: u8) -> Result<Option<u8>> {
        let ty = node.ntype()?;
        let bytes = self.seg(node)?;
        match ty {
            NType::Node7Leaf | NType::Node15Leaf => {
                let count = usize::from(bytes[0]);
                Ok(bytes[1..=count].iter().copied().find(|&b| b >= byte))
            }
            NType::Node256Leaf => Ok((usize::from(byte)..NODE256_CAPACITY)
                .find(|&b| mask_is_set(bytes, b as u8))
                .map(|b| b as u8)),
            _ => Err(Error::internal(format!(
                "Invalid node type for GetNextByte: {ty:?}."
            ))),
        }
    }

    /// Inserts `child` at `byte` into the node at `slot`, growing it when full; for
    /// nested leaf nodes only the byte is inserted (`Node::InsertChild`).
    pub(crate) fn insert_child(&mut self, slot: Slot, byte: u8, child: Node) -> Result<()> {
        let node = self.get(slot)?;
        match node.ntype()? {
            NType::Node4 => {
                if !self.base_insert(node, NODE4_CAPACITY, byte, child)? {
                    let grown = self.grow_base(slot, node, NType::Node16)?;
                    self.base_insert(grown, NODE16_CAPACITY, byte, child)?;
                }
            }
            NType::Node16 => {
                if !self.base_insert(node, NODE16_CAPACITY, byte, child)? {
                    let grown = self.grow_node16(slot, node)?;
                    self.node48_insert(grown, byte, child)?;
                }
            }
            NType::Node48 => {
                if !self.node48_insert(node, byte, child)? {
                    let grown = self.grow_node48(slot, node)?;
                    self.node256_insert(grown, byte, child)?;
                }
            }
            NType::Node256 => self.node256_insert(node, byte, child)?,
            NType::Node7Leaf => {
                if !self.leaf_insert_byte(node, NODE7_LEAF_CAPACITY, byte)? {
                    let grown = self.grow_leaf(slot, node, NType::Node15Leaf)?;
                    self.leaf_insert_byte(grown, NODE15_LEAF_CAPACITY, byte)?;
                }
            }
            NType::Node15Leaf => {
                if !self.leaf_insert_byte(node, NODE15_LEAF_CAPACITY, byte)? {
                    let grown = self.grow_node15_leaf(slot, node)?;
                    self.node256_leaf_insert(grown, byte)?;
                }
            }
            NType::Node256Leaf => self.node256_leaf_insert(node, byte)?,
            ty => {
                return Err(Error::internal(format!(
                    "Invalid node type for InsertChild: {ty:?}."
                )));
            }
        }
        Ok(())
    }

    /// `BaseNode::InsertChildInternal`; false if the node is full.
    fn base_insert(&mut self, node: Node, capacity: usize, byte: u8, child: Node) -> Result<bool> {
        let children = base_node_children(capacity);
        let bytes = self.seg_mut(node)?;
        let count = usize::from(bytes[0]);
        if count == capacity {
            return Ok(false);
        }
        let mut pos = 0;
        while pos < count && bytes[1 + pos] < byte {
            pos += 1;
        }
        for i in (pos + 1..=count).rev() {
            bytes[1 + i] = bytes[i];
            let moved = read_node(bytes, children + 8 * (i - 1));
            write_node(bytes, children + 8 * i, moved);
        }
        bytes[1 + pos] = byte;
        write_node(bytes, children + 8 * pos, child);
        bytes[0] = (count + 1) as u8;
        Ok(true)
    }

    /// Replaces the base node at `slot` by a base node of type `to` with the same
    /// children (`Node16::GrowNode4`, `Node4::ShrinkNode16`).
    fn grow_base(&mut self, slot: Slot, old: Node, to: NType) -> Result<Node> {
        let from_children = base_node_children(base_capacity(old.ntype()?));
        let (count, keys, children) = {
            let bytes = self.seg(old)?;
            let count = usize::from(bytes[0]);
            let keys = bytes[1..=count].to_vec();
            let children: Vec<Node> = (0..count)
                .map(|i| read_node(bytes, from_children + 8 * i))
                .collect();
            (count, keys, children)
        };
        let mut new = self.new_node(to)?;
        new.set_gate_status(old.gate_status());
        let to_children = base_node_children(base_capacity(to));
        let bytes = self.seg_mut(new)?;
        bytes[0] = count as u8;
        bytes[1..=count].copy_from_slice(&keys);
        for (i, child) in children.iter().enumerate() {
            write_node(bytes, to_children + 8 * i, *child);
        }
        self.set(slot, new)?;
        self.free_node(old)?;
        Ok(new)
    }

    /// `Node48::GrowNode16`.
    fn grow_node16(&mut self, slot: Slot, old: Node) -> Result<Node> {
        let children = self.base_children(old)?;
        let mut new = self.new_node(NType::Node48)?;
        new.set_gate_status(old.gate_status());
        let bytes = self.seg_mut(new)?;
        bytes[0] = children.len() as u8;
        for (i, (byte, child)) in children.iter().enumerate() {
            bytes[1 + usize::from(*byte)] = i as u8;
            write_node(bytes, NODE48_CHILDREN + 8 * i, *child);
        }
        self.set(slot, new)?;
        self.free_node(old)?;
        Ok(new)
    }

    /// The keys and children of a `Node4`/`Node16`, in order.
    fn base_children(&self, node: Node) -> Result<Vec<(u8, Node)>> {
        let children = base_node_children(base_capacity(node.ntype()?));
        let bytes = self.seg(node)?;
        let count = usize::from(bytes[0]);
        Ok((0..count)
            .map(|i| (bytes[1 + i], read_node(bytes, children + 8 * i)))
            .collect())
    }

    /// `Node48::InsertChild` without growth; false if the node is full.
    fn node48_insert(&mut self, node: Node, byte: u8, child: Node) -> Result<bool> {
        let bytes = self.seg_mut(node)?;
        let count = usize::from(bytes[0]);
        if count == NODE48_CAPACITY {
            return Ok(false);
        }
        let mut pos = count;
        if read_node(bytes, NODE48_CHILDREN + 8 * pos).has_metadata() {
            pos = 0;
            while read_node(bytes, NODE48_CHILDREN + 8 * pos).has_metadata() {
                pos += 1;
            }
        }
        write_node(bytes, NODE48_CHILDREN + 8 * pos, child);
        bytes[1 + usize::from(byte)] = pos as u8;
        bytes[0] = (count + 1) as u8;
        Ok(true)
    }

    /// `Node256::GrowNode48`.
    fn grow_node48(&mut self, slot: Slot, old: Node) -> Result<Node> {
        let (count, children) = {
            let bytes = self.seg(old)?;
            let children: Vec<(usize, Node)> = (0..NODE256_CAPACITY)
                .filter(|&b| bytes[1 + b] != NODE48_EMPTY)
                .map(|b| {
                    (
                        b,
                        read_node(bytes, NODE48_CHILDREN + 8 * usize::from(bytes[1 + b])),
                    )
                })
                .collect();
            (u16::from(bytes[0]), children)
        };
        let mut new = self.new_node(NType::Node256)?;
        new.set_gate_status(old.gate_status());
        let bytes = self.seg_mut(new)?;
        write_u16(bytes, 0, count);
        for (byte, child) in children {
            write_node(bytes, NODE256_CHILDREN + 8 * byte, child);
        }
        self.set(slot, new)?;
        self.free_node(old)?;
        Ok(new)
    }

    /// `Node256::InsertChild`.
    fn node256_insert(&mut self, node: Node, byte: u8, child: Node) -> Result<()> {
        let bytes = self.seg_mut(node)?;
        let count = read_u16(bytes, 0);
        write_u16(bytes, 0, count + 1);
        write_node(bytes, NODE256_CHILDREN + 8 * usize::from(byte), child);
        Ok(())
    }

    /// `BaseLeaf::InsertByteInternal`; false if the node is full.
    fn leaf_insert_byte(&mut self, node: Node, capacity: usize, byte: u8) -> Result<bool> {
        let bytes = self.seg_mut(node)?;
        let count = usize::from(bytes[0]);
        if count == capacity {
            return Ok(false);
        }
        let mut pos = 0;
        while pos < count && bytes[1 + pos] < byte {
            pos += 1;
        }
        for i in (pos + 1..=count).rev() {
            bytes[1 + i] = bytes[i];
        }
        bytes[1 + pos] = byte;
        bytes[0] = (count + 1) as u8;
        Ok(true)
    }

    /// Replaces the `Node7Leaf`/`Node15Leaf` at `slot` by one of type `to` with the same
    /// bytes (`Node15Leaf::GrowNode7Leaf`, `Node7Leaf::ShrinkNode15Leaf`).
    fn grow_leaf(&mut self, slot: Slot, old: Node, to: NType) -> Result<Node> {
        let keys = {
            let bytes = self.seg(old)?;
            bytes[1..=usize::from(bytes[0])].to_vec()
        };
        let mut new = self.new_node(to)?;
        new.set_gate_status(old.gate_status());
        let bytes = self.seg_mut(new)?;
        bytes[0] = keys.len() as u8;
        bytes[1..=keys.len()].copy_from_slice(&keys);
        self.set(slot, new)?;
        self.free_node(old)?;
        Ok(new)
    }

    /// `Node256Leaf::GrowNode15Leaf`.
    fn grow_node15_leaf(&mut self, slot: Slot, old: Node) -> Result<Node> {
        let keys = {
            let bytes = self.seg(old)?;
            bytes[1..=usize::from(bytes[0])].to_vec()
        };
        let mut new = self.new_node(NType::Node256Leaf)?;
        new.set_gate_status(old.gate_status());
        let bytes = self.seg_mut(new)?;
        write_u16(bytes, 0, keys.len() as u16);
        for byte in keys {
            mask_set(bytes, byte, true);
        }
        self.set(slot, new)?;
        self.free_node(old)?;
        Ok(new)
    }

    /// `Node256Leaf::InsertByte`.
    fn node256_leaf_insert(&mut self, node: Node, byte: u8) -> Result<()> {
        let bytes = self.seg_mut(node)?;
        let count = read_u16(bytes, 0);
        write_u16(bytes, 0, count + 1);
        mask_set(bytes, byte, true);
        Ok(())
    }

    /// Deletes the child at `byte` of the node at `slot`, shrinking or compressing the
    /// node; `prefix` is the slot of the node's parent (`Node::DeleteChild`).
    pub(crate) fn delete_child(
        &mut self,
        slot: Slot,
        prefix: Slot,
        byte: u8,
        status: GateStatus,
        row_id: &[u8; ROW_ID_SIZE],
    ) -> Result<()> {
        let node = self.get(slot)?;
        match node.ntype()? {
            NType::Node4 => self.node4_delete_child(slot, node, prefix, byte, status),
            NType::Node16 => {
                let count = self.base_delete(node, byte)?;
                if count < NODE4_CAPACITY {
                    self.grow_base(slot, node, NType::Node4)?;
                }
                Ok(())
            }
            NType::Node48 => {
                let index = usize::from(self.seg(node)?[1 + usize::from(byte)]);
                self.free_tree(slot_of(node, NODE48_CHILDREN + 8 * index))?;
                let bytes = self.seg_mut(node)?;
                bytes[1 + usize::from(byte)] = NODE48_EMPTY;
                bytes[0] = bytes[0].saturating_sub(1);
                if usize::from(bytes[0]) < NODE48_SHRINK {
                    self.shrink_node48(slot, node)?;
                }
                Ok(())
            }
            NType::Node256 => {
                self.free_tree(slot_of(node, NODE256_CHILDREN + 8 * usize::from(byte)))?;
                let bytes = self.seg_mut(node)?;
                let count = read_u16(bytes, 0).saturating_sub(1);
                write_u16(bytes, 0, count);
                if usize::from(count) <= NODE256_SHRINK {
                    self.shrink_node256(slot, node)?;
                }
                Ok(())
            }
            NType::Node7Leaf => self.node7_leaf_delete_byte(slot, node, prefix, byte, row_id),
            NType::Node15Leaf => {
                let count = self.leaf_delete_byte(node, byte)?;
                if count < NODE7_LEAF_CAPACITY {
                    self.grow_leaf(slot, node, NType::Node7Leaf)?;
                }
                Ok(())
            }
            NType::Node256Leaf => {
                let bytes = self.seg_mut(node)?;
                let count = read_u16(bytes, 0).saturating_sub(1);
                write_u16(bytes, 0, count);
                mask_set(bytes, byte, false);
                if usize::from(count) <= NODE48_SHRINK {
                    self.shrink_node256_leaf(slot, node)?;
                }
                Ok(())
            }
            ty => Err(Error::internal(format!(
                "Invalid node type for DeleteChild: {ty:?}."
            ))),
        }
    }

    /// `BaseNode::DeleteChildInternal`: frees the subtree at `byte` and closes the gap;
    /// returns the new count.
    fn base_delete(&mut self, node: Node, byte: u8) -> Result<usize> {
        let children = base_node_children(base_capacity(node.ntype()?));
        let (count, pos) = {
            let bytes = self.seg(node)?;
            let count = usize::from(bytes[0]);
            let pos = (0..count).find(|&i| bytes[1 + i] == byte).unwrap_or(count);
            (count, pos)
        };
        if pos < count {
            self.free_tree(slot_of(node, children + 8 * pos))?;
        }
        let bytes = self.seg_mut(node)?;
        let count = count.saturating_sub(1);
        bytes[0] = count as u8;
        for i in pos..count {
            bytes[1 + i] = bytes[2 + i];
            let moved = read_node(bytes, children + 8 * (i + 1));
            write_node(bytes, children + 8 * i, moved);
        }
        Ok(count)
    }

    /// `Node4::DeleteChild`: a `Node4` left with one child is replaced by a prefix
    /// leading to that child.
    fn node4_delete_child(
        &mut self,
        slot: Slot,
        node: Node,
        prefix: Slot,
        byte: u8,
        status: GateStatus,
    ) -> Result<()> {
        let count = self.base_delete(node, byte)?;
        if count != 1 {
            return Ok(());
        }
        let (remaining_byte, child) = {
            let bytes = self.seg(node)?;
            (bytes[1], read_node(bytes, base_node_children(NODE4_CAPACITY)))
        };
        let previous_status = node.gate_status();
        self.free_node(node)?;
        self.set(slot, Node::default())?;
        self.concat(prefix, slot, child, remaining_byte, previous_status, status)
    }

    /// `Node16::ShrinkNode48`.
    fn shrink_node48(&mut self, slot: Slot, old: Node) -> Result<()> {
        let children = self.children(old)?;
        let mut new = self.new_node(NType::Node16)?;
        new.set_gate_status(old.gate_status());
        let offset = base_node_children(NODE16_CAPACITY);
        let bytes = self.seg_mut(new)?;
        bytes[0] = children.len() as u8;
        for (i, (byte, child)) in children.iter().enumerate() {
            bytes[1 + i] = *byte;
            write_node(bytes, offset + 8 * i, *child);
        }
        self.set(slot, new)?;
        self.free_node(old)
    }

    /// `Node48::ShrinkNode256`.
    fn shrink_node256(&mut self, slot: Slot, old: Node) -> Result<()> {
        let children = self.children(old)?;
        let mut new = self.new_node(NType::Node48)?;
        new.set_gate_status(old.gate_status());
        let bytes = self.seg_mut(new)?;
        bytes[0] = children.len() as u8;
        for (i, (byte, child)) in children.iter().enumerate() {
            bytes[1 + usize::from(*byte)] = i as u8;
            write_node(bytes, NODE48_CHILDREN + 8 * i, *child);
        }
        self.set(slot, new)?;
        self.free_node(old)
    }

    /// `BaseLeaf::DeleteByteInternal`; returns the new count.
    fn leaf_delete_byte(&mut self, node: Node, byte: u8) -> Result<usize> {
        let bytes = self.seg_mut(node)?;
        let count = usize::from(bytes[0]);
        let pos = (0..count).find(|&i| bytes[1 + i] == byte).unwrap_or(count);
        let count = count.saturating_sub(1);
        bytes[0] = count as u8;
        for i in pos..count {
            bytes[1 + i] = bytes[2 + i];
        }
        Ok(count)
    }

    /// `Node7Leaf::DeleteByte`: a leaf left with one row id becomes an inlined leaf.
    fn node7_leaf_delete_byte(
        &mut self,
        slot: Slot,
        node: Node,
        prefix: Slot,
        byte: u8,
        row_id: &[u8; ROW_ID_SIZE],
    ) -> Result<()> {
        let count = self.leaf_delete_byte(node, byte)?;
        if count != 1 {
            return Ok(());
        }
        let last = self.seg(node)?[1];
        let mut remaining = *row_id;
        remaining[ROW_ID_SIZE - 1] = last;
        let remainder = super::key::decode_row_id(&remaining);
        if self.get(prefix)?.is(NType::Prefix) {
            self.free_tree(prefix)?;
            return self.set(prefix, Node::inlined_leaf(remainder));
        }
        self.free_node(node)?;
        self.set(slot, Node::inlined_leaf(remainder))
    }

    /// `Node15Leaf::ShrinkNode256Leaf`.
    fn shrink_node256_leaf(&mut self, slot: Slot, old: Node) -> Result<()> {
        let keys: Vec<u8> = {
            let bytes = self.seg(old)?;
            (0..NODE256_CAPACITY)
                .filter(|&b| mask_is_set(bytes, b as u8))
                .map(|b| b as u8)
                .collect()
        };
        let mut new = self.new_node(NType::Node15Leaf)?;
        new.set_gate_status(old.gate_status());
        let bytes = self.seg_mut(new)?;
        bytes[0] = keys.len() as u8;
        bytes[1..=keys.len()].copy_from_slice(&keys);
        self.set(slot, new)?;
        self.free_node(old)
    }

    /// Frees the subtree at `slot` and clears the slot (`Node::FreeTree`).
    pub(crate) fn free_tree(&mut self, slot: Slot) -> Result<()> {
        let root = self.get(slot)?;
        if root.has_metadata() {
            self.free_subtree(root)?;
        }
        if !matches!(slot, Slot::Dummy) {
            self.set(slot, Node::default())?;
        }
        Ok(())
    }

    /// Frees every segment reachable from `root`.
    pub(crate) fn free_subtree(&mut self, root: Node) -> Result<()> {
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            if !node.has_metadata() {
                continue;
            }
            match node.ntype()? {
                NType::LeafInlined => {}
                NType::Leaf => {
                    let next = read_node(self.seg(node)?, LEAF_NEXT);
                    stack.push(next);
                    self.free_node(node)?;
                }
                NType::Prefix => {
                    stack.push(self.prefix_child(node)?);
                    self.free_node(node)?;
                }
                NType::Node4 | NType::Node16 | NType::Node48 | NType::Node256 => {
                    for (_, child) in self.children(node)? {
                        stack.push(child);
                    }
                    self.free_node(node)?;
                }
                NType::Node7Leaf | NType::Node15Leaf | NType::Node256Leaf => self.free_node(node)?,
            }
        }
        Ok(())
    }

    /// The row ids of a deprecated leaf chain (`Leaf::DeprecatedGetRowIds`).
    pub(crate) fn deprecated_row_ids(&self, node: Node) -> Result<Vec<i64>> {
        let mut row_ids = Vec::new();
        let mut current = node;
        while current.has_metadata() {
            let bytes = self.seg(current)?;
            let count = usize::from(bytes[0]).min(super::node::LEAF_SIZE);
            for i in 0..count {
                row_ids.push(read_u64(bytes, LEAF_ROW_IDS + 8 * i) as i64);
            }
            current = read_node(bytes, LEAF_NEXT);
        }
        Ok(row_ids)
    }
}
