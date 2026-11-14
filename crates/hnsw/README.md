# Rust HNSW core

Internal graph implementation for roastedDuck's compatible VSS implementation.
This crate is enabled normally and depends only on the existing Rust common crate
and the standard library. It has no C/C++ dependency, no USearch code, no extension
binary loader, and no persistence format.

Construction and search implement the hierarchical graph algorithms described by
[Malkov and Yashunin, arXiv:1603.09320](https://arxiv.org/abs/1603.09320): exponentially
distributed levels, greedy upper-layer descent, best-first bounded candidate search,
diversity-based neighbor selection, reciprocal links, and degree pruning. Base and
upper layers obey their separate M0/M degree limits. Diversity selection gives
one representative per identical vector priority before filling remaining capacity
with rejected candidates; duplicate row labels remain separately searchable. Equal-distance neighbor ties
use deterministic pair hashing so a group of duplicate vectors does not permanently
favor its oldest nodes. Traversal stopping compares distances, not arbitrary tie
identities. There is no exact scan or exact-search fallback inside graph search.

`Hnsw::new(Config::new(dimensions, metric))` supplies defaults M=16, M0=32,
ef_construction=128 and ef_search=64. A fixed seed makes construction reproducible.
Search accepts a per-query ef override and reports actual node visits, expanded
nodes and distance evaluations. ef is at least k and is bounded by the stored-node
count. Counters include upper-layer and cross-layer revisits; they are work counts,
not unique-row counts.

Queries finish the frontier work they have already admitted, even if a queued
candidate subsequently falls outside the retained ef radius. Such a candidate
can still lead to a better node. Construction retains the classical radius stop.
The result buffer remains bounded by ef; additional query work is graph-edge
traversal, never an array scan or exact fallback. Distance counters expose this
quality/work tradeoff, and ef is not advertised as a distance-evaluation limit.

Bounded result retention uses distance first, then reverse discovery order among
admitted ties, matching the pinned extension's equal-distance buffer convention.
Once the buffer is full, only a strictly closer successor is newly admitted; an
equal-radius successor need not enter the candidate pool. Frontier traversal keeps
its separate deterministic ordering. This policy is independent of vector
coordinates, user IDs and graph seeds, and is not a promise of globally descending
row IDs or identical tied subsets across independently constructed graphs.
Evidence is under `.context/artifacts/vss-fts/tie-policy-probes/`; earlier valid
but different tied outputs and the original failing fixture receipt remain intact.

Labels are signed 64-bit row identifiers. Insertion copies the vector. Single and
batch insertion reject dimensionality and duplicate-live-label errors before
changing any graph, label or random-generator state. Removal is idempotent: a node
stays as a routing tombstone but its label is no longer returned. Reinsertion after
removal creates a new live node, leaving the old node inactive. Compaction builds
a fresh hierarchy from live nodes before replacing the original. The storage
wrapper controls retirement and must retain old-visible rows until MVCC cleanup.

Navigation supports squared L2, cosine distance and negative inner product.
Inner-product candidate ranking stays inner-product based during construction and
querying. Its link-diversity test uses squared Euclidean separation between a
candidate, the center, and previously selected neighbors: raw negative inner
product is not a metric and can otherwise let a large-norm vector suppress every
routing direction. This changes graph construction, not SQL scores or M/ef settings.
L2/IP use f64 accumulation from f32 vectors. Cosine follows DuckDB 1.5.6's FLOAT
kernel arithmetic and comparison-order clamp: undefined similarity (including a
zero norm) becomes -1, producing cosine distance 2. This also preserves the FLOAT
norm-overflow convention; promoting norms to f64 would change candidate ordering
in those cases. NaN distances from other metrics sort strictly after positive
infinity; signed zeros compare equal. Non-finite coordinates remain accepted.

The zero-vector convention is observed in the official 1.5.6 oracle receipt
`.context/artifacts/vss-fts/vss-oracle-probes-1/zero_cosine.stdout`; the arithmetic
is defined in `extension/core_functions/include/core_functions/array_kernels.hpp`
and already implemented by roastedDuck's SQL array kernels. SQL result scores
still come from those existing SQL functions, not graph navigation scores.


The small tests cover exhaustive ground truth, all metrics, held-out recall and
traversal counts, deterministic failed-batch atomicity, option/dimension boundaries,
removed entry-point routing, duplicates, reinsertion and compaction. They do not
replace the declared 10,000-vector / 200-query recall matrix or SQL tests.
