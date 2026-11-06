//! Lookup, insertion and deletion of keys (DuckDB's `ARTOperator`).

use rustdb_common::{Error, ErrorKind, Result};

use super::key::ROW_ID_SIZE;
use super::node::{GateStatus, NType, Node, Slot};
use super::{Art, ConflictType};
use crate::index::IndexAppendMode;

/// The leaf (or gate) of `key` below `node`, starting at `depth` (`ARTOperator::Lookup`).
pub(crate) fn lookup(art: &Art, node: Node, key: &[u8], depth: usize) -> Result<Option<Node>> {
    let mut node = node;
    let mut depth = depth;
    while node.has_metadata() {
        if node.is_any_leaf() || node.gate_status() == GateStatus::Set {
            return Ok(Some(node));
        }
        if node.is(NType::Prefix) {
            for &byte in art.prefix_bytes(node)? {
                if key.get(depth) != Some(&byte) {
                    return Ok(None);
                }
                depth += 1;
            }
            node = art.prefix_child(node)?;
            continue;
        }
        let Some(&byte) = key.get(depth) else {
            return Ok(None);
        };
        let Some(child) = art.get_child(node, byte)? else {
            return Ok(None);
        };
        node = art.get(child)?;
        depth += 1;
    }
    Ok(None)
}

/// Whether the leaf `node` holds `row_id` (`ARTOperator::LookupInLeaf`).
pub(crate) fn lookup_in_leaf(art: &Art, node: Node, row_id: [u8; ROW_ID_SIZE]) -> Result<bool> {
    let mut node = node;
    let mut depth = 0;
    while node.has_metadata() {
        match node.ntype()? {
            NType::LeafInlined => return Ok(node.row_id() == super::key::decode_row_id(&row_id)),
            NType::Leaf => {
                return Ok(art
                    .deprecated_row_ids(node)?
                    .contains(&super::key::decode_row_id(&row_id)));
            }
            NType::Node7Leaf | NType::Node15Leaf | NType::Node256Leaf => {
                return art.has_byte(node, row_id[ROW_ID_SIZE - 1]);
            }
            NType::Node4 | NType::Node16 | NType::Node48 | NType::Node256 => {
                let Some(&byte) = row_id.get(depth) else {
                    return Ok(false);
                };
                match art.get_child(node, byte)? {
                    Some(child) => {
                        node = art.get(child)?;
                        depth += 1;
                    }
                    None => return Ok(false),
                }
            }
            NType::Prefix => {
                for &byte in art.prefix_bytes(node)? {
                    if row_id.get(depth) != Some(&byte) {
                        return Ok(false);
                    }
                    depth += 1;
                }
                node = art.prefix_child(node)?;
            }
        }
    }
    Ok(false)
}

/// Inserts `key` with `row_id` below `slot`, starting at `depth` of the key; `status`
/// tells whether the insert happens inside a gate (`ARTOperator::Insert`).
#[allow(clippy::too_many_arguments, reason = "mirrors ARTOperator::Insert")]
pub(crate) fn insert(
    art: &mut Art,
    slot: Slot,
    key: &[u8],
    depth: usize,
    row_id: [u8; ROW_ID_SIZE],
    status: GateStatus,
    delete_arts: &[&Art],
    mode: IndexAppendMode,
) -> Result<ConflictType> {
    let mut slot = slot;
    let mut depth = depth;
    let mut status = status;
    let mut active_key: &[u8] = key;
    if !art.get(slot)?.has_metadata() {
        if status == GateStatus::Set {
            art.set(slot, Node::inlined_leaf(super::key::decode_row_id(&row_id)))?;
            return Ok(ConflictType::NoConflict);
        }
        let tail = art.new_prefix_chain(slot, key, depth, key.len() - depth)?;
        art.set(tail, Node::inlined_leaf(super::key::decode_row_id(&row_id)))?;
        return Ok(ConflictType::NoConflict);
    }
    loop {
        let node = art.get(slot)?;
        if !node.has_metadata() {
            return Err(Error::internal("node without metadata in ARTOperator::Insert"));
        }
        if status == GateStatus::NotSet && node.gate_status() == GateStatus::Set {
            if !art.is_unique() {
                // Enter the nested ART of row ids.
                active_key = &row_id;
                depth = 0;
                status = GateStatus::Set;
                continue;
            }
            return Err(Error::new_bytes(
                ErrorKind::Fatal,
                rustdb_common::byte_format!(
                    "Corrupted unique ART index \"{}\": encountered an existing gated leaf in unique index while inserting",
                    art.name()
                ).into_bytes(),
            ));
        }
        match node.ntype()? {
            NType::LeafInlined => {
                return insert_into_inlined(art, slot, key, row_id, depth, status, delete_arts, mode);
            }
            NType::Leaf => {
                art.transform_to_nested(slot)?;
            }
            NType::Node7Leaf | NType::Node15Leaf | NType::Node256Leaf => {
                // Row ids are unique: the last byte is never present yet.
                let byte = active_key[ROW_ID_SIZE - 1];
                art.insert_child(slot, byte, Node::default())?;
                return Ok(ConflictType::NoConflict);
            }
            NType::Node4 | NType::Node16 | NType::Node48 | NType::Node256 => {
                let byte = *active_key
                    .get(depth)
                    .ok_or_else(|| Error::internal("ART key shorter than its path"))?;
                if let Some(child) = art.get_child(node, byte)? {
                    slot = child;
                    depth += 1;
                    continue;
                }
                insert_into_node(art, slot, key, row_id, depth, status)?;
                return Ok(ConflictType::NoConflict);
            }
            NType::Prefix => {
                let data = art.prefix_bytes(node)?.to_vec();
                for (i, &byte) in data.iter().enumerate() {
                    if active_key.get(depth) != Some(&byte) {
                        insert_into_prefix(art, slot, active_key, row_id, i, depth, status)?;
                        return Ok(ConflictType::NoConflict);
                    }
                    depth += 1;
                }
                slot = art.prefix_child_slot(node);
            }
        }
    }
}

/// `ARTOperator::InsertIntoInlined`: a second row id for an existing key.
#[allow(clippy::too_many_arguments, reason = "mirrors ARTOperator::InsertIntoInlined")]
fn insert_into_inlined(
    art: &mut Art,
    slot: Slot,
    key: &[u8],
    row_id: [u8; ROW_ID_SIZE],
    depth: usize,
    status: GateStatus,
    delete_arts: &[&Art],
    mode: IndexAppendMode,
) -> Result<ConflictType> {
    let row_id_node = Node::inlined_leaf(super::key::decode_row_id(&row_id));
    if !art.is_unique() || mode == IndexAppendMode::InsertDuplicates {
        art.merge_inlined(slot, row_id_node, status, depth)?;
        return Ok(ConflictType::NoConflict);
    }
    let this_row_id = art.get(slot)?.row_id();
    for delete_art in delete_arts {
        let Some(deleted) = delete_art.lookup(key)? else {
            continue;
        };
        if deleted.row_id() != this_row_id {
            // The local index holds a newer row id: a real conflict.
            continue;
        }
        // The existing row was deleted by this transaction.
        art.merge_inlined(slot, row_id_node, status, depth)?;
        return Ok(ConflictType::NoConflict);
    }
    if mode == IndexAppendMode::IgnoreDuplicates {
        return Ok(ConflictType::NoConflict);
    }
    Ok(ConflictType::Constraint)
}

/// `ARTOperator::InsertIntoNode`: a new child of the inner node at `slot`.
fn insert_into_node(
    art: &mut Art,
    slot: Slot,
    key: &[u8],
    row_id: [u8; ROW_ID_SIZE],
    depth: usize,
    status: GateStatus,
) -> Result<()> {
    let leaf = Node::inlined_leaf(super::key::decode_row_id(&row_id));
    if status == GateStatus::Set {
        // Inside gates, inlined row ids hang directly below the node.
        return art.insert_child(slot, row_id[depth], leaf);
    }
    let child = if depth + 1 < key.len() {
        let pc = usize::from(art.prefix_count);
        let count = key.len() - depth - 1;
        // Build the prefix chain bottom-up from its first segment.
        let first_len = count.min(pc);
        let head = art.new_prefix(&key[depth + 1..depth + 1 + first_len])?;
        let tail = art.new_prefix_chain(
            art.prefix_child_slot(head),
            key,
            depth + 1 + first_len,
            count - first_len,
        )?;
        art.set(tail, leaf)?;
        head
    } else {
        leaf
    };
    art.insert_child(slot, key[depth], child)
}

/// `ARTOperator::InsertIntoPrefix`: splits the prefix at `slot` where the key differs.
fn insert_into_prefix(
    art: &mut Art,
    slot: Slot,
    key: &[u8],
    row_id: [u8; ROW_ID_SIZE],
    pos: usize,
    depth: usize,
    status: GateStatus,
) -> Result<()> {
    let byte = art.prefix_bytes(art.get(slot)?)?[pos];
    let (node_slot, child, split_status) = art.split_prefix(slot, pos)?;
    let mut node4 = art.new_node(NType::Node4)?;
    node4.set_gate_status(split_status);
    art.set(node_slot, node4)?;
    art.insert_child(node_slot, byte, child)?;
    insert_into_node(art, node_slot, key, row_id, depth, status)
}

/// What a step of [`delete`] holds as the current key.
#[derive(Clone, Copy)]
enum ActiveKey<'a> {
    Key(&'a [u8]),
    RowId(&'a [u8; ROW_ID_SIZE]),
}

impl ActiveKey<'_> {
    fn get(self, i: usize) -> Option<u8> {
        match self {
            Self::Key(key) => key.get(i).copied(),
            Self::RowId(row_id) => row_id.get(i).copied(),
        }
    }
}

/// Removes `row_id` from `key`, compressing nodes on the way back (`ARTOperator::Delete`).
pub(crate) fn delete(art: &mut Art, key: &[u8], row_id: [u8; ROW_ID_SIZE]) -> Result<bool> {
    let mut greatgrandparent = Slot::Dummy;
    let mut grandparent = Slot::Dummy;
    let mut parent = Slot::Root;
    let mut current = Slot::Root;
    let mut current_key = ActiveKey::Key(key);
    let mut grandparent_depth = 0;
    let mut parent_depth = 0;
    let mut depth = 0;
    let mut status = GateStatus::NotSet;
    let mut passed_node = false;
    let row_id_value = super::key::decode_row_id(&row_id);
    let missing = || Error::internal("ART key shorter than its path");
    loop {
        let node = art.get(current)?;
        if !node.has_metadata() {
            return Ok(false);
        }
        if status == GateStatus::NotSet && node.gate_status() == GateStatus::Set {
            status = GateStatus::Set;
            current_key = ActiveKey::RowId(&row_id);
            depth = 0;
            continue;
        }
        match node.ntype()? {
            NType::LeafInlined => {
                if node.row_id() != row_id_value {
                    return Ok(false);
                }
                let parent_node = art.get(parent)?;
                if !passed_node && parent_node.is(NType::Prefix) {
                    // The tree holds exactly this key.
                    art.free_tree(parent)?;
                    return Ok(true);
                }
                if parent_node.is(NType::Prefix) {
                    // Possibly compress PREFIX - Node4 - PREFIX - LEAF.
                    let byte = current_key.get(grandparent_depth).ok_or_else(missing)?;
                    art.delete_child(grandparent, greatgrandparent, byte, status, row_id)?;
                    return Ok(true);
                }
                let byte = current_key.get(parent_depth).ok_or_else(missing)?;
                art.delete_child(parent, grandparent, byte, status, row_id)?;
                return Ok(true);
            }
            NType::Leaf => {
                art.transform_to_nested(current)?;
            }
            NType::Prefix => {
                greatgrandparent = grandparent;
                grandparent = parent;
                parent = current;
                grandparent_depth = parent_depth;
                parent_depth = depth;
                // Traverse the prefix chain up to the next non-prefix node or gate.
                loop {
                    let prefix = art.get(current)?;
                    if !prefix.is(NType::Prefix) {
                        break;
                    }
                    for &byte in art.prefix_bytes(prefix)? {
                        if current_key.get(depth) != Some(byte) {
                            return Ok(false);
                        }
                        depth += 1;
                    }
                    current = art.prefix_child_slot(prefix);
                    if art.get(current)?.gate_status() == GateStatus::Set {
                        break;
                    }
                }
            }
            NType::Node4 | NType::Node16 | NType::Node48 | NType::Node256 => {
                passed_node = true;
                greatgrandparent = grandparent;
                grandparent = parent;
                parent = current;
                grandparent_depth = parent_depth;
                parent_depth = depth;
                let byte = current_key.get(depth).ok_or_else(missing)?;
                let Some(child) = art.get_child(node, byte)? else {
                    return Ok(false);
                };
                current = child;
                depth += 1;
            }
            NType::Node7Leaf | NType::Node15Leaf | NType::Node256Leaf => {
                let byte = current_key.get(depth).ok_or_else(missing)?;
                if art.has_byte(node, byte)? {
                    art.delete_child(current, parent, byte, status, row_id)?;
                    return Ok(true);
                }
                return Ok(false);
            }
        }
    }
}
