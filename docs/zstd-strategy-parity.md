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
