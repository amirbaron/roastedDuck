//! Leaves: merging two inlined leaves into a nested ART of row ids, and converting
//! deprecated leaf chains (DuckDB's `Leaf`).

use roastedduck_common::{Error, ErrorKind, Result};

use super::Art;
use super::key::{ROW_ID_SIZE, row_id_key};
use super::node::{GateStatus, NType, Node, Slot};
use crate::index::IndexAppendMode;

impl Art {
    /// Merges the inlined leaf at `left` with the inlined leaf `right` into a nested ART
    /// (`Leaf::MergeInlined`). Outside a gate the result becomes a gate; inside one, the
    /// row ids branch at their first differing byte at or after `depth`.
    pub(crate) fn merge_inlined(
        &mut self,
        left: Slot,
        right: Node,
        status: GateStatus,
        depth: usize,
    ) -> Result<()> {
        let status = status.flipped();
        let depth = if status == GateStatus::Set { 0 } else { depth };
        let left_row_id = self.get(left)?.row_id();
        let right_row_id = right.row_id();
        let left_key = row_id_key(left_row_id);
        let right_key = row_id_key(right_row_id);
        let pos = (depth..ROW_ID_SIZE)
            .find(|&i| left_key[i] != right_key[i])
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::Fatal,
                    "Corrupted ART index - likely the same row id was inserted twice into the same ART",
                )
            })?;
        self.set(left, Node::default())?;
        let mut node = left;
        if pos != depth {
            node = self.new_prefix_chain(left, &left_key, depth, pos - depth)?;
        }
        let left_byte = left_key[pos];
        let right_byte = right_key[pos];
        if pos == ROW_ID_SIZE - 1 {
            let leaf = self.new_node(NType::Node7Leaf)?;
            self.set(node, leaf)?;
            self.insert_child(node, left_byte, Node::default())?;
            self.insert_child(node, right_byte, Node::default())?;
        } else {
            let node4 = self.new_node(NType::Node4)?;
            self.set(node, node4)?;
            self.insert_child(node, left_byte, Node::inlined_leaf(left_row_id))?;
            self.insert_child(node, right_byte, Node::inlined_leaf(right_row_id))?;
        }
        self.set_gate(left, status)
    }

    /// Replaces the deprecated leaf chain at `slot` by a gate over a nested ART of its row
    /// ids (`Leaf::TransformToNested`).
    pub(crate) fn transform_to_nested(&mut self, slot: Slot) -> Result<()> {
        let node = self.get(slot)?;
        let row_ids = self.deprecated_row_ids(node)?;
        self.free_tree(slot)?;
        for &row_id in &row_ids {
            let key = row_id_key(row_id);
            self.insert_key(
                slot,
                &key,
                0,
                key,
                GateStatus::Set,
                &[],
                IndexAppendMode::InsertDuplicates,
            )?;
        }
        if row_ids.len() > 1 {
            self.set_gate(slot, GateStatus::Set)?;
        }
        Ok(())
    }
}
