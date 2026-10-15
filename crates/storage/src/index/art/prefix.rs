//! Prefix nodes: chains of up to `prefix_count` key bytes in front of a child (DuckDB's
//! `Prefix`).

use rustdb_common::{Error, Result};

use super::Art;
use super::node::{GateStatus, NType, Node, Slot, read_node, write_node};

impl Art {
    fn pc(&self) -> usize {
        usize::from(self.prefix_count)
    }

    /// The slot of the child of prefix `node`.
    pub(crate) fn prefix_child_slot(&self, node: Node) -> Slot {
        Slot::In {
            owner: node,
            offset: (self.pc() + 1) as u16,
        }
    }

    /// The child of prefix `node`.
    pub(crate) fn prefix_child(&self, node: Node) -> Result<Node> {
        Ok(read_node(self.seg(node)?, self.pc() + 1))
    }

    pub(crate) fn set_prefix_child(&mut self, node: Node, child: Node) -> Result<()> {
        let offset = self.pc() + 1;
        write_node(self.seg_mut(node)?, offset, child);
        Ok(())
    }

    /// The key bytes of prefix `node`.
    pub(crate) fn prefix_bytes(&self, node: Node) -> Result<&[u8]> {
        let pc = self.pc();
        let bytes = self.seg(node)?;
        let count = usize::from(bytes[pc]);
        bytes
            .get(..count)
            .filter(|_| count <= pc)
            .ok_or_else(|| Error::internal("corrupted ART prefix count"))
    }

    /// A new prefix holding `data` with an empty child (`Prefix::NewInternal`).
    pub(crate) fn new_prefix(&mut self, data: &[u8]) -> Result<Node> {
        let pc = self.pc();
        let node = self.alloc(NType::Prefix)?;
        let bytes = self.seg_mut(node)?;
        bytes[pc] = data.len() as u8;
        bytes[..data.len()].copy_from_slice(data);
        write_node(bytes, pc + 1, Node::default());
        Ok(node)
    }

    /// Writes a chain of prefixes holding `key[depth..depth + count]` to `slot` and returns
    /// the slot of the last prefix's child, or `slot` itself if `count` is zero
    /// (`Prefix::New`).
    pub(crate) fn new_prefix_chain(
        &mut self,
        slot: Slot,
        key: &[u8],
        depth: usize,
        count: usize,
    ) -> Result<Slot> {
        let mut slot = slot;
        let mut offset = 0;
        let mut remaining = count;
        while remaining > 0 {
            let this_count = remaining.min(self.pc());
            let start = depth + offset;
            let prefix = self.new_prefix(&key[start..start + this_count])?;
            self.set(slot, prefix)?;
            slot = self.prefix_child_slot(prefix);
            offset += this_count;
            remaining -= this_count;
        }
        Ok(slot)
    }

    /// Appends `byte` to prefix `node`, chaining a new prefix when it is full; returns the
    /// prefix holding the byte (`Prefix::Append(byte)`).
    pub(crate) fn prefix_append_byte(&mut self, node: Node, byte: u8) -> Result<Node> {
        let pc = self.pc();
        let mut node = node;
        loop {
            let bytes = self.seg_mut(node)?;
            let count = usize::from(bytes[pc]);
            if count != pc {
                bytes[count] = byte;
                bytes[pc] = (count + 1) as u8;
                return Ok(node);
            }
            let next = self.new_prefix(&[])?;
            self.set_prefix_child(node, next)?;
            node = next;
        }
    }

    /// Appends the prefix chain `other` (up to a gate or a non-prefix node) to prefix
    /// `node`, freeing the appended prefixes (`Prefix::Append(Node)`).
    pub(crate) fn prefix_append_node(&mut self, node: Node, other: Node) -> Result<()> {
        let mut prefix = node;
        let mut other = other;
        while other.is(NType::Prefix) {
            if other.gate_status() == GateStatus::Set {
                return self.set_prefix_child(prefix, other);
            }
            let data = self.prefix_bytes(other)?.to_vec();
            let child = self.prefix_child(other)?;
            for byte in data {
                prefix = self.prefix_append_byte(prefix, byte)?;
            }
            self.set_prefix_child(prefix, child)?;
            self.free_node(other)?;
            other = child;
        }
        Ok(())
    }

    /// The last prefix of the chain starting at `node` (`Prefix::GetTail`).
    fn prefix_tail(&self, node: Node) -> Result<Node> {
        let mut prefix = node;
        loop {
            let child = self.prefix_child(prefix)?;
            if !child.is(NType::Prefix) {
                return Ok(prefix);
            }
            prefix = child;
        }
    }

    /// Splits the prefix at `slot` at position `pos` (`Prefix::Split`). Returns the slot
    /// that must receive the new `Node4`, the child to insert under the old byte, and the
    /// gate status of the new `Node4`.
    pub(crate) fn split_prefix(&mut self, slot: Slot, pos: usize) -> Result<(Slot, Node, GateStatus)> {
        let pc = self.pc();
        let node = self.get(slot)?;
        let (count, ptr) = {
            let bytes = self.seg(node)?;
            (usize::from(bytes[pc]), read_node(bytes, pc + 1))
        };
        if pos + 1 == pc {
            // The split is at the last byte of a full prefix.
            self.seg_mut(node)?[pc] -= 1;
            return Ok((self.prefix_child_slot(node), ptr, GateStatus::NotSet));
        }
        let child = if pos + 1 < count {
            // Move the bytes after pos into a new prefix.
            let rest = self.seg(node)?[pos + 1..count].to_vec();
            let new_prefix = self.new_prefix(&rest)?;
            if ptr.is(NType::Prefix) && ptr.gate_status() == GateStatus::NotSet {
                self.prefix_append_node(new_prefix, ptr)?;
            } else {
                self.set_prefix_child(new_prefix, ptr)?;
            }
            new_prefix
        } else {
            ptr
        };
        self.seg_mut(node)?[pc] = pos as u8;
        if pos == 0 {
            let status = node.gate_status();
            self.free_node(node)?;
            self.set(slot, Node::default())?;
            return Ok((slot, child, status));
        }
        Ok((self.prefix_child_slot(node), child, GateStatus::NotSet))
    }

    /// Reconnects the only remaining `child` of a freed `Node4` (whose slot is `node4`)
    /// under the byte `byte`, merging prefixes where possible (`Prefix::Concat`).
    pub(crate) fn concat(
        &mut self,
        parent: Slot,
        node4: Slot,
        child: Node,
        byte: u8,
        node4_status: GateStatus,
        status: GateStatus,
    ) -> Result<()> {
        if node4_status == GateStatus::Set {
            return self.concat_node4_was_gate(node4, child, byte);
        }
        if child.gate_status() == GateStatus::Set {
            return self.concat_child_is_gate(parent, node4, child, byte);
        }
        self.concat_internal(parent, node4, child, byte, status)
    }

    fn concat_internal(
        &mut self,
        parent: Slot,
        node4: Slot,
        child: Node,
        byte: u8,
        status: GateStatus,
    ) -> Result<()> {
        let parent_node = self.get(parent)?;
        if child.is(NType::LeafInlined) {
            if status == GateStatus::Set {
                if parent_node.is(NType::Prefix) {
                    // Inside a gate, an inlined leaf replaces the whole prefix chain.
                    let mut current = parent_node;
                    while current.is(NType::Prefix) {
                        let next = self.prefix_child(current)?;
                        self.free_node(current)?;
                        self.set(parent, next)?;
                        current = next;
                    }
                    return self.set(parent, child);
                }
                return self.set(node4, child);
            }
            if parent_node.is(NType::Prefix) {
                let tail = self.prefix_tail(parent_node)?;
                let tail = self.prefix_append_byte(tail, byte)?;
                return self.set_prefix_child(tail, child);
            }
            let prefix = self.new_prefix(&[byte])?;
            self.set(node4, prefix)?;
            return self.set_prefix_child(prefix, child);
        }
        if parent_node.is(NType::Prefix) {
            let tail = self.prefix_tail(parent_node)?;
            let tail = self.prefix_append_byte(tail, byte)?;
            if child.is(NType::Prefix) {
                return self.prefix_append_node(tail, child);
            }
            return self.set_prefix_child(tail, child);
        }
        let prefix = self.new_prefix(&[byte])?;
        self.set(node4, prefix)?;
        if child.is(NType::Prefix) {
            return self.prefix_append_node(prefix, child);
        }
        self.set_prefix_child(prefix, child)
    }

    fn concat_node4_was_gate(&mut self, node4: Slot, child: Node, byte: u8) -> Result<()> {
        if child.is(NType::LeafInlined) {
            return self.set(node4, child);
        }
        let prefix = self.new_prefix(&[byte])?;
        self.set(node4, prefix)?;
        if child.is(NType::Prefix) {
            self.prefix_append_node(prefix, child)?;
        } else {
            self.set_prefix_child(prefix, child)?;
        }
        self.set_gate(node4, GateStatus::Set)
    }

    fn concat_child_is_gate(&mut self, parent: Slot, node4: Slot, child: Node, byte: u8) -> Result<()> {
        let parent_node = self.get(parent)?;
        if !parent_node.is(NType::Prefix) {
            let prefix = self.new_prefix(&[byte])?;
            self.set(node4, prefix)?;
            return self.set_prefix_child(prefix, child);
        }
        let tail = self.prefix_tail(parent_node)?;
        let tail = self.prefix_append_byte(tail, byte)?;
        self.set_prefix_child(tail, child)
    }
}
