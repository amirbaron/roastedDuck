//! Serializing an ART to a checkpoint or the WAL (DuckDB's `ART::TransformToDeprecated`,
//! `PrepareSerialize`, `SerializeToDisk` and `SerializeToWAL`).
//!
//! Files of storage versions before v1.2.0 (`v1_0_0_storage`) store indexes in the old
//! layout: row id sets as chains of four-row-id `Leaf` segments instead of nested ARTs,
//! and prefix segments of 15 bytes. The live index is converted in place, as DuckDB does;
//! the ART operations handle both layouts.

use std::collections::BTreeSet;
use std::sync::Arc;

use rustdb_common::{CaseInsensitiveMap, Error, Result};
use rustdb_types::Value;

use super::iterator::{ArtIterator, RowIdSetOutput};
use super::node::{
    DEPRECATED_ALLOCATOR_COUNT, GateStatus, LEAF_NEXT, LEAF_ROW_IDS, LEAF_SIZE, NType, Node, Slot,
    write_node, write_u64,
};
use super::{Art, DEPRECATED_PREFIX_COUNT, PREFIX_METADATA_SIZE};
use crate::index::fixed_size_allocator::{
    BufferLoader, FixedSizeAllocator, FixedSizeAllocatorInfo, IndexBufferWriter,
};

/// The serialized form of an ART (DuckDB's `IndexStorageInfo` without the buffers).
#[derive(Clone, Debug, Default)]
pub struct ArtStorageInfo {
    pub name: String,
    /// The root node pointer.
    pub root: u64,
    pub allocator_infos: Vec<FixedSizeAllocatorInfo>,
    pub options: CaseInsensitiveMap<Value>,
}

/// Whether `options` ask for the v1.0.0 layout (the default when the option is absent).
fn wants_v1_0_0_storage(options: &CaseInsensitiveMap<Value>) -> bool {
    options
        .get("v1_0_0_storage")
        .is_none_or(|value| value.as_bool() != Some(false))
}

impl Art {
    /// Restores `ART::ART` / `InitAllocators` from a checkpoint. The node buffers are
    /// registered now and fetched only when traversal first touches them.
    pub fn load_from_storage(&mut self, info: &ArtStorageInfo, loader: Arc<dyn BufferLoader>) -> Result<()> {
        if !matches!(
            info.allocator_infos.len(),
            DEPRECATED_ALLOCATOR_COUNT | super::node::ALLOCATOR_COUNT
        ) {
            return Err(Error::serialization(
                "invalid allocator count in index storage information",
            ));
        }
        let prefix_size = info.allocator_infos[0].segment_size;
        self.prefix_count = prefix_size
            .checked_sub(PREFIX_METADATA_SIZE as u64)
            .and_then(|count| u8::try_from(count).ok())
            .filter(|&count| count != 0)
            .ok_or_else(|| Error::serialization("invalid prefix size in index storage information"))?;
        self.tree = Node::default();
        self.buffer_loader = Some(loader);
        self.init_allocators()?;
        for (allocator, stored) in self.allocators.iter_mut().zip(&info.allocator_infos) {
            if stored.segment_size != allocator.segment_size() as u64 {
                return Err(Error::serialization(
                    "invalid segment size in index storage information",
                ));
            }
            allocator.init(stored)?;
        }
        self.tree = Node::from_raw(info.root);
        self.storage_version = info.options.get("storage_version").and_then(Value::as_u64);
        Ok(())
    }

    /// Converts nested leaves to deprecated leaf chains and, if needed, prefixes to the
    /// 15-byte layout (`ART::TransformToDeprecated`).
    pub fn transform_to_deprecated(&mut self) -> Result<()> {
        if self.tree.has_metadata() {
            self.leaves_to_deprecated()?;
        }
        if self.prefix_count != DEPRECATED_PREFIX_COUNT {
            let prefix_size = usize::from(DEPRECATED_PREFIX_COUNT) + PREFIX_METADATA_SIZE;
            let mut deprecated =
                FixedSizeAllocator::new(prefix_size, self.block_size, self.buffer_manager.clone())?;
            if let Some(loader) = &self.buffer_loader {
                deprecated.set_loader(Arc::clone(loader));
            }
            if self.tree.has_metadata() {
                self.prefixes_to_deprecated(&mut deprecated)?;
            }
            if !self.allocators[0].is_empty() {
                return Err(Error::internal(
                    "prefix allocator not empty after the transformation",
                ));
            }
            self.allocators[0] = deprecated;
            self.prefix_count = DEPRECATED_PREFIX_COUNT;
        }
        Ok(())
    }

    /// The slots of the children of inner node `node`.
    pub(super) fn child_slots(&self, node: Node) -> Result<Vec<Slot>> {
        let mut slots = Vec::new();
        for (byte, _) in self.children(node)? {
            if let Some(slot) = self.get_child(node, byte)? {
                slots.push(slot);
            }
        }
        Ok(slots)
    }

    /// Replaces every gate by a deprecated leaf chain (`Node::TransformToDeprecated` for
    /// leaves).
    fn leaves_to_deprecated(&mut self) -> Result<()> {
        let mut stack = vec![Slot::Root];
        while let Some(slot) = stack.pop() {
            let node = self.get(slot)?;
            if !node.has_metadata() {
                continue;
            }
            if node.gate_status() == GateStatus::Set {
                self.leaf_to_deprecated(slot, node)?;
                continue;
            }
            match node.ntype()? {
                NType::Prefix => stack.push(self.prefix_child_slot(node)),
                NType::LeafInlined | NType::Leaf => {}
                NType::Node4 | NType::Node16 | NType::Node48 | NType::Node256 => {
                    stack.extend(self.child_slots(node)?);
                }
                other => {
                    return Err(Error::internal(format!(
                        "invalid node type for TransformToDeprecated: {other:?}"
                    )));
                }
            }
        }
        Ok(())
    }

    /// `Leaf::TransformToDeprecated`: the row ids of the gate at `slot` become a chain of
    /// `Leaf` segments of up to four row ids, in ascending order.
    fn leaf_to_deprecated(&mut self, slot: Slot, node: Node) -> Result<()> {
        let mut row_ids = BTreeSet::new();
        {
            let mut it = ArtIterator::new(self);
            it.find_minimum(node)?;
            let mut output = RowIdSetOutput::new(&mut row_ids, usize::MAX);
            it.scan(&[], &mut output, false)?;
        }
        self.free_tree(slot)?;
        let row_ids: Vec<i64> = row_ids.into_iter().collect();
        let mut target = slot;
        for chunk in row_ids.chunks(LEAF_SIZE) {
            let leaf = self.alloc(NType::Leaf)?;
            self.set(target, leaf)?;
            let bytes = self.seg_mut(leaf)?;
            bytes[0] = chunk.len() as u8;
            for (i, &row_id) in chunk.iter().enumerate() {
                write_u64(bytes, LEAF_ROW_IDS + 8 * i, row_id as u64);
            }
            write_node(bytes, LEAF_NEXT, Node::default());
            target = Slot::In {
                owner: leaf,
                offset: LEAF_NEXT as u16,
            };
        }
        Ok(())
    }

    /// Rewrites every prefix chain into `deprecated`, 15 bytes per segment
    /// (`PrefixHandle::TransformToDeprecated`). Runs after the leaves were converted, so
    /// no gates remain.
    fn prefixes_to_deprecated(&mut self, deprecated: &mut FixedSizeAllocator) -> Result<()> {
        let capacity = usize::from(DEPRECATED_PREFIX_COUNT);
        let mut stack = vec![Slot::Root];
        while let Some(slot) = stack.pop() {
            let node = self.get(slot)?;
            if !node.has_metadata() {
                continue;
            }
            let mut child = node;
            if node.is(NType::Prefix) {
                // Collect the whole chain, then write it anew.
                let mut bytes = Vec::new();
                while child.is(NType::Prefix) {
                    bytes.extend_from_slice(self.prefix_bytes(child)?);
                    let next = self.prefix_child(child)?;
                    self.free_node(child)?;
                    child = next;
                }
                let mut head = Node::default();
                let mut previous: Option<Node> = None;
                for part in bytes.chunks(capacity) {
                    let mut segment = deprecated.new_segment()?;
                    segment.set_type(NType::Prefix);
                    let data = deprecated.segment_mut(segment)?;
                    data[..part.len()].copy_from_slice(part);
                    data[capacity] = part.len() as u8;
                    match previous {
                        None => head = segment,
                        Some(previous) => {
                            write_node(deprecated.segment_mut(previous)?, capacity + 1, segment);
                        }
                    }
                    previous = Some(segment);
                }
                match previous {
                    Some(last) => {
                        write_node(deprecated.segment_mut(last)?, capacity + 1, child);
                        self.set(slot, head)?;
                    }
                    None => self.set(slot, child)?,
                }
            }
            if child.has_metadata() && child.is_inner() {
                stack.extend(self.child_slots(child)?);
            }
        }
        Ok(())
    }

    /// `ART::PrepareSerialize`: converts to the requested layout and describes the root.
    fn prepare_serialize(&mut self, options: &CaseInsensitiveMap<Value>) -> Result<ArtStorageInfo> {
        if wants_v1_0_0_storage(options) {
            self.transform_to_deprecated()?;
        }
        let mut options = options.clone();
        // Indexes loaded without a storage version do not get one.
        if let Some(version) = self.storage_version {
            options.insert("storage_version", Value::ubigint(version));
        }
        for allocator in &mut self.allocators {
            allocator.remove_empty_buffers();
        }
        Ok(ArtStorageInfo {
            name: self.name.clone(),
            root: self.tree.raw(),
            allocator_infos: Vec::new(),
            options,
        })
    }

    /// The number of allocators a layout serializes.
    fn serialized_allocators(options: &CaseInsensitiveMap<Value>) -> usize {
        if wants_v1_0_0_storage(options) {
            DEPRECATED_ALLOCATOR_COUNT
        } else {
            super::node::ALLOCATOR_COUNT
        }
    }

    /// Writes the changed index data with `write` (which returns where the bytes went) and
    /// returns its description (`ART::SerializeToDisk`); `keep` receives the block of each
    /// buffer that is on disk unchanged.
    pub fn serialize_to_disk(
        &mut self,
        options: &CaseInsensitiveMap<Value>,
        writer: &mut dyn IndexBufferWriter,
        keep: &mut dyn FnMut(rustdb_buffer::BlockId),
    ) -> Result<ArtStorageInfo> {
        let mut info = self.prepare_serialize(options)?;
        let count = Self::serialized_allocators(options);
        for allocator in self.allocators.iter_mut().take(count) {
            allocator.serialize_buffers(&mut |data: &[u8]| writer.write(data), keep)?;
        }
        // The caller still holds the index lock here. No reader may observe the new
        // locations until their partial blocks have been flushed (`WritePartialBlocks`).
        writer.flush()?;
        info.allocator_infos = self.allocator_infos(count);
        Ok(info)
    }

    /// The description of the index and the bytes of its buffers per allocator, for a
    /// WAL entry (`ART::SerializeToWAL`).
    pub fn serialize_to_wal(
        &mut self,
        options: &CaseInsensitiveMap<Value>,
    ) -> Result<(ArtStorageInfo, Vec<Vec<Vec<u8>>>)> {
        let mut info = self.prepare_serialize(options)?;
        let count = Self::serialized_allocators(options);
        let buffers = self
            .allocators
            .iter_mut()
            .take(count)
            .map(FixedSizeAllocator::wal_buffers)
            .collect::<Result<Vec<_>>>()?;
        info.allocator_infos = self.allocator_infos(count);
        Ok((info, buffers))
    }
}
