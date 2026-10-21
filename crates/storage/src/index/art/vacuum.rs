//! Compacting the memory of an ART (DuckDB's `ART::Vacuum`): segments of mostly empty
//! buffers move to fuller ones, and the emptied buffers are freed.

use rustdb_common::{Error, Result};

use super::Art;
use super::node::{LEAF_NEXT, NType, Node, Slot};

impl Art {
    /// `ART::Vacuum`: an empty index frees all its memory; otherwise the allocators whose
    /// free space passes the threshold move their segments out of their emptiest buffers.
    pub fn vacuum(&mut self) -> Result<()> {
        if !self.tree.has_metadata() {
            for allocator in &mut self.allocators {
                allocator.reset();
            }
            return Ok(());
        }
        // `InitializeVacuum`: the allocators that need a vacuum.
        let vacuumed: Vec<bool> = self
            .allocators
            .iter_mut()
            .map(super::super::fixed_size_allocator::FixedSizeAllocator::initialize_vacuum)
            .collect();
        if !vacuumed.contains(&true) {
            return Ok(());
        }
        // Traverse the tree, moving every node that lives in a vacuumed buffer (the
        // `ARTScanner` with `EMPLACE` handling).
        let mut stack = vec![Slot::Root];
        while let Some(slot) = stack.pop() {
            let node = self.get(slot)?;
            if !node.has_metadata() {
                continue;
            }
            let ty = node.ntype()?;
            let descend = match ty {
                NType::LeafInlined => continue,
                NType::Leaf => {
                    if vacuumed[1] {
                        self.vacuum_deprecated_leaf(slot)?;
                    }
                    continue;
                }
                NType::Node7Leaf | NType::Node15Leaf | NType::Node256Leaf => false,
                NType::Prefix | NType::Node4 | NType::Node16 | NType::Node48 | NType::Node256 => true,
            };
            let index = ty
                .allocator_index()
                .ok_or_else(|| Error::internal(format!("invalid node type for Vacuum: {ty:?}")))?;
            let mut current = node;
            if vacuumed[index] && self.allocators[index].needs_vacuum(node) {
                let status = node.gate_status();
                current = self.allocators[index].vacuum_pointer(node)?;
                current.set_type(ty);
                current.set_gate_status(status);
                self.set(slot, current)?;
            }
            if descend {
                if ty == NType::Prefix {
                    stack.push(self.prefix_child_slot(current));
                } else {
                    stack.extend(self.child_slots(current)?);
                }
            }
        }
        // `FinalizeVacuum`.
        for (allocator, vacuumed) in self.allocators.iter_mut().zip(vacuumed) {
            if vacuumed {
                allocator.finalize_vacuum();
            }
        }
        Ok(())
    }

    /// Moves the segments of the deprecated leaf chain at `slot` out of vacuumed buffers
    /// (`Leaf::DeprecatedVacuum`).
    fn vacuum_deprecated_leaf(&mut self, slot: Slot) -> Result<()> {
        let index = NType::Leaf
            .allocator_index()
            .ok_or_else(|| Error::internal("deprecated leaves have an allocator"))?;
        let mut slot = slot;
        loop {
            let node = self.get(slot)?;
            if !node.has_metadata() {
                return Ok(());
            }
            let mut current: Node = node;
            if self.allocators[index].needs_vacuum(node) {
                current = self.allocators[index].vacuum_pointer(node)?;
                current.set_type(NType::Leaf);
                self.set(slot, current)?;
            }
            slot = Slot::In {
                owner: current,
                offset: LEAF_NEXT as u16,
            };
        }
    }
}
