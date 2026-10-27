# ZSTD strategy parity

The live completion audit confirms `crates/zstd/src/cctx.rs::effective_params`
replaces Greedy/Lazy/Lazy2/binary-tree/optimal strategies with DFast and caps hash/
chain tables. `block.rs` dispatches only Fast or DFast. This is the documented
compression-level gap; valid cross-readable frames alone do not implement the
requested compression strategy. Preserve the owner's accepted speed/memory policy;
do not introduce table caching or change the default level to disguise the gap.

## Signed before-change calibration (2026-10-27)

External evidence is under `zstd-strategy-parity-20261027/`. Signed standalone Rust
codec probes use the current codec source directly, with no C/C++ dependency. The
signed C++ oracle links DuckDB1.5.6's own vendored ZSTD from its preserved native
library, rather than a system codec. Source/native identities are checked before
and after. The codec package sources are unchanged during calibration.

`before-2/verification.json` retains40 paired frames over five deterministic inputs
(3KB/100KB/300KB text, 200KB random data, mixed binary), each at levels3,4,5,6,8,13,19,22.
All80 opposite-engine decodes reproduce the exact input. Only15/40 frames match
byte-for-byte: all five level3 controls, four level4 controls, and the incompressible
random frame at each stronger level. For the 3KB text, C++ level4 is760bytes versus
Rust783; C++ level13 is715 versus Rust781. These are actual encoder differences,
not corruption or a decoder incompatibility.

Every frame's compression timing and whole-process peak RSS are retained together.
These are single-sample codec diagnostics, not final TPC-H/ClickBench performance
acceptance. The first calibration stopped on a loader-tracing harness assertion:
macOS stripped DYLD_PRINT_LIBRARIES when launching its timing utility. That raw
`before-1` run remains preserved. `before-2` sets the tracing variable at the final
child invocation and verifies the actual C++ library path (and absence of that
library in the Rust codec process).

## Implementation and verification contract

Port the frozen encoder's actual match-finder selection and lifecycle: hash-chain
Greedy/Lazy/Lazy2, row-hash mode selected by platform/window, binary-tree lazy and
optimal strategies, and their state across blocks/extDict wraps/context resets.
Check associated block splitting/optimal-parser behavior at high levels. Do not
claim a stronger strategy merely by relabeling the DFast matcher or reordering
serialized output. Pure Rust shipping artifacts remain required.

Build the missing algorithms in stages while retaining explicit unsupported-path
tracking; do not call the whole codec or goal complete from a small passing cohort.
Use signed independent C++ golden frames, adversarial/boundary/overlap/wrap cases,
all requested levels, both streaming/one-shot paths and opposite-engine decoders.
Retain default-level bytes and coupled speed/memory controls. Then refresh actual
Parquet/spill/storage integration and the final release gates on final signed
artifacts. No implementation or whole-goal completion is claimed by this baseline.

## Small-window hash-chain implementation checkpoint

`lazy.rs` now ports the no-dictionary hash-chain search and Greedy/Lazy/Lazy2 parser:
chain updates/lazy skipping, bounded searches, gain comparisons, backwards match
extension and repeat-offset disable/restore/swap. Selection is enabled only for
these strategies with windowLog<=14, where the frozen reference also selects the
hash-chain path. Row-hash/binary-tree/optimal fallbacks remain explicit. This is a
staged component, not a reduced completion scope. Default-level selection is unchanged.

The retained `after-1` matrix has19/40 identical frames versus15 before; the four
changed frames are exactly the 3KB text at levels4/5/6/8, now matching C++. Every
other before/after frame, including all five level3 controls, remains byte-identical.
All80 opposite-engine decodes still match original inputs. `boundaries-1` records320
additional byte-identical frozen C++ frames: four payload kinds (zero/text/random/
overlap),16 sizes from0 through16384bytes, and levels3/4/5/6/8. The C++-derived frame
size/FNV goldens are checked into `tests/data/lazy_hash_chain_1.5.6.tsv`; raw frames,
input/source/native hashes remain external. Small tooling compile/link corrections
and the earlier loader-trace failure remain separate from valid measurements.

Workspace/all-target Clippy with warnings denied passes. All14 codec tests pass in
both immutable signed debug and release executables, including640 golden comparisons
per build while reusing contexts, original default/stream/string reference cases and
decoder tests. All918 snapshot build inputs remain unchanged. Evidence:
source-1/manifest.json, clippy-1-terminal.json, tests-{debug,release}-1/verification.json,
boundaries-1/verification.json, speed-memory-diagnostic-1.json.

Single-sample 3KB diagnostics retain compressed size/time/RSS together: level4 changes
783->760bytes and27.4->38.9microseconds, with2.21MB whole-process RSS on both old/new
Rust; C++ is760bytes/58.9microseconds/4.39MB. Level8 changes783->759bytes and27.3->56.9
microseconds, again2.21MB on both Rust images; C++ is759bytes/58.2microseconds/4.41MB.
These short samples are noisy diagnostics, not performance acceptance. Default codec
frames are unchanged, and no caching or memory increase is introduced to chase speed.
Actual Parquet/spill/storage shipping integration and final SQL/client/performance
gates remain pending after the remaining strategies are implemented.
