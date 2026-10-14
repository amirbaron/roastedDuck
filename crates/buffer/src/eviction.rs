//! Eviction queues (DuckDB's `EvictionQueue`).
//!
//! Unpinned blocks are pushed with the block's current eviction sequence number. A later
//! push or the block's destruction makes the old entry "dead"; dead entries are skipped
//! when dequeuing and purged in bulk. DuckDB uses a lock-free MPMC queue; here each queue
//! is split into shards selected by thread so concurrent unpins rarely contend, and
//! dequeuing takes the oldest head across shards to keep the LRU order.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, Weak};

use crate::block::BlockMemory;
use crate::os;

/// One queue entry: a weak reference to a block and the sequence number it had when
/// it was enqueued.
pub(crate) struct EvictionNode {
    pub(crate) memory: Weak<BlockMemory>,
    pub(crate) sequence: u64,
}

impl EvictionNode {
    fn is_dead(&self) -> bool {
        self.memory
            .upgrade()
            .is_none_or(|memory| memory.eviction_sequence() != self.sequence)
    }
}

const SHARDS: usize = 16;
/// A purge is considered every `INSERT_INTERVAL` insertions.
const INSERT_INTERVAL: u64 = 4096;
/// Nodes inspected per purge iteration.
const PURGE_SIZE: usize = INSERT_INTERVAL as usize * 2;
/// Queues shorter than `PURGE_SIZE * EARLY_OUT_MULTIPLIER` are never purged.
const EARLY_OUT_MULTIPLIER: usize = 4;
/// Purging continues while dead nodes outnumber `ALIVE_NODE_MULTIPLIER - 1` times the
/// alive ones.
const ALIVE_NODE_MULTIPLIER: usize = 4;

struct Shard {
    nodes: Mutex<VecDeque<(u64, EvictionNode)>>,
    /// Insertion stamp of the front node, `u64::MAX` when empty.
    head: AtomicU64,
}

impl Shard {
    fn refresh_head(&self, nodes: &VecDeque<(u64, EvictionNode)>) {
        let head = nodes.front().map_or(u64::MAX, |(stamp, _)| *stamp);
        self.head.store(head, Ordering::Relaxed);
    }
}

/// A FIFO of eviction candidates.
pub(crate) struct EvictionQueue {
    shards: Box<[Shard]>,
    clock: AtomicU64,
    len: AtomicUsize,
    insertions: AtomicU64,
    /// Entries that no longer refer to a live block version (may briefly dip below zero).
    dead_nodes: AtomicI64,
    purge_lock: Mutex<()>,
}

impl EvictionQueue {
    pub(crate) fn new() -> Self {
        Self {
            shards: (0..SHARDS)
                .map(|_| Shard {
                    nodes: Mutex::new(VecDeque::new()),
                    head: AtomicU64::new(u64::MAX),
                })
                .collect(),
            clock: AtomicU64::new(0),
            len: AtomicUsize::new(0),
            insertions: AtomicU64::new(0),
            dead_nodes: AtomicI64::new(0),
            purge_lock: Mutex::new(()),
        }
    }

    /// Enqueues `node`; returns whether the queue is due for a purge.
    pub(crate) fn push(&self, node: EvictionNode) -> bool {
        let stamp = self.clock.fetch_add(1, Ordering::Relaxed);
        let shard = &self.shards[os::thread_slot() % SHARDS];
        if let Ok(mut nodes) = shard.nodes.lock() {
            nodes.push_back((stamp, node));
            if nodes.len() == 1 {
                shard.refresh_head(&nodes);
            }
            self.len.fetch_add(1, Ordering::Relaxed);
        }
        (self.insertions.fetch_add(1, Ordering::Relaxed) + 1).is_multiple_of(INSERT_INTERVAL)
    }

    /// Dequeues the oldest node across shards.
    pub(crate) fn pop(&self) -> Option<EvictionNode> {
        loop {
            let (index, head) = self
                .shards
                .iter()
                .enumerate()
                .map(|(index, shard)| (index, shard.head.load(Ordering::Relaxed)))
                .min_by_key(|&(_, head)| head)?;
            if head == u64::MAX {
                return None;
            }
            let shard = &self.shards[index];
            let Ok(mut nodes) = shard.nodes.lock() else {
                return None;
            };
            if let Some((_, node)) = nodes.pop_front() {
                shard.refresh_head(&nodes);
                self.len.fetch_sub(1, Ordering::Relaxed);
                return Some(node);
            }
            // Raced with another consumer; look again.
            shard.refresh_head(&nodes);
        }
    }

    pub(crate) fn increment_dead_nodes(&self) {
        self.dead_nodes.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn decrement_dead_nodes(&self) {
        self.dead_nodes.fetch_sub(1, Ordering::Relaxed);
    }

    /// Garbage-collects dead nodes; only one thread purges at a time.
    pub(crate) fn purge(&self) {
        let Ok(_guard) = self.purge_lock.try_lock() else {
            return;
        };
        let mut approx_len = self.len.load(Ordering::Relaxed);
        if approx_len < PURGE_SIZE * EARLY_OUT_MULTIPLIER {
            return;
        }
        let mut max_purges = approx_len / PURGE_SIZE;
        while max_purges != 0 {
            self.purge_iteration();
            approx_len = self.len.load(Ordering::Relaxed);
            if approx_len < PURGE_SIZE * EARLY_OUT_MULTIPLIER {
                break;
            }
            let dead = (self.dead_nodes.load(Ordering::Relaxed).max(0) as usize).min(approx_len);
            let alive = approx_len - dead;
            if alive * (ALIVE_NODE_MULTIPLIER - 1) > dead {
                break;
            }
            max_purges -= 1;
        }
    }

    /// Takes up to `PURGE_SIZE` nodes off the fronts of the shards and puts the alive ones
    /// back at the end.
    fn purge_iteration(&self) {
        let per_shard = PURGE_SIZE / SHARDS;
        let mut removed = 0;
        for shard in &*self.shards {
            let Ok(mut nodes) = shard.nodes.lock() else {
                continue;
            };
            let take = per_shard.min(nodes.len());
            for _ in 0..take {
                let Some((stamp, node)) = nodes.pop_front() else {
                    break;
                };
                if node.is_dead() {
                    removed += 1;
                } else {
                    nodes.push_back((stamp, node));
                }
            }
            shard.refresh_head(&nodes);
        }
        self.len.fetch_sub(removed, Ordering::Relaxed);
        self.dead_nodes.fetch_sub(removed as i64, Ordering::Relaxed);
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.len.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dead_node() -> EvictionNode {
        EvictionNode {
            memory: Weak::new(),
            sequence: 0,
        }
    }

    #[test]
    fn purges_dead_nodes_once_the_queue_is_large() {
        let queue = EvictionQueue::new();
        let mut purge_due = 0;
        for _ in 0..(PURGE_SIZE * EARLY_OUT_MULTIPLIER + 100) {
            queue.increment_dead_nodes();
            purge_due += usize::from(queue.push(dead_node()));
        }
        assert_eq!(
            purge_due,
            (PURGE_SIZE * EARLY_OUT_MULTIPLIER + 100) / INSERT_INTERVAL as usize
        );
        queue.purge();
        // Purging stops once the queue is below the early-out size.
        assert!(queue.len() < PURGE_SIZE * EARLY_OUT_MULTIPLIER);
        let remaining = queue.len();
        let popped = std::iter::from_fn(|| queue.pop()).count();
        assert_eq!(popped, remaining);
        assert_eq!(queue.len(), 0);
    }
}
