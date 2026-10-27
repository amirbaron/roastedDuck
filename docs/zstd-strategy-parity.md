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

## Row-hash and streaming dictionary implementation checkpoint

The source now ports salted row matching for Greedy/Lazy/Lazy2: automatic
platform/window selection, 16/32/64-entry tag rows, eight-position hash cache,
long-match update skipping, bounded circular candidate search and entropy-driven
salt advancement across context resets. Row matching allocates its actual tag/hash
working tables and omits the hash-chain table. The shared lazy parser now handles
external-dictionary repeats, segment boundaries, backward extension and the
reference's distinct incompressible-skip threshold. Binary-tree/optimal strategies
still use the explicit fallback and remain required work.

The first known-size row comparison improves the broad40-case cohort to28 exact
frames, with all80 opposite decodes passing; unresolved binary-tree/optimal cases
remain visible. `stream-1/verification.json` then proves84/84 byte-identical one-shot/
stream frames and168 opposite decodes, including2.5MB input-buffer wraps and levels
exercising all three row sizes. `stream-edges-1` adds108/108 exact frames and216
opposite decodes for0..17byte final blocks, both tiny streams and the2,228,224byte
buffer-wrap boundary. Default-level controls remain literal C++ matches.

`reuse-2/verification.json` separately verifies32 actual chronological C++/Rust
same-context frames: repeated inputs, changed levels, hash-chain/row/default path
switches, and one-shot/stream transitions over two rounds. The loader log identifies
the frozen native C++ library. These goldens come from that retained C++ sequence;
they do not assume reused salt state writes fresh-context bytes.

The224 new C++ goldens are retained in `lazy_rows_1.5.6.tsv` with raw frames/provenance
external. Signed debug and release builds each pass all16 codec tests, including
864 explicit small-window/row/context goldens plus original default/stream/string
reference and decoder checks. Workspace/all-target Clippy passes after one retained
documentation-style correction. Source3 preserves925 overlay entries/924 verified
build inputs. Evidence: tests-{debug,release}-2/verification.json, clippy-3-terminal.json,
row-golden-provenance-1.json, source-3/manifest.json.

Per-case coupled diagnostics remain explicit in row-speed-memory-diagnostic-1.json.
For300KB text, C++/Rust level5 is1.391/1.735ms with7.70/5.06MiB whole-process RSS;
level8 is3.182/4.491ms with10.14/7.55MiB; level12 is6.237/8.480ms with10.12/7.55MiB.
Each produces identical compressed bytes. These are single codec samples, not
TPC-H/ClickBench acceptance; their individual slowdowns are not hidden in averages.
Higher requested strategies now use their real working tables instead of a capped
DFast substitute. Default Fast/DFast table allocation remains unchanged; no table
caching is introduced to close a speed gap. Shipping integration/final performance
and the remaining binary-tree/optimal paths remain open.

## Delayed binary-tree lazy strategy checkpoint

`bt_lazy.rs` now ports the actual delayed unsorted binary tree used by BtLazy2:
unsorted preceding-position insertion, reverse-chain candidate stacking, delayed
sorting, smaller/larger links, prefix/extDict suffix comparison, bounded tree
search, offset-price tie decisions and repetitive-pattern update skipping. The
shared lazy parser selects it at depth2. The strategy now retains its real level
parameters rather than DFast caps. Index reduction preserves its reserved unsorted
marker; an explicit reduction unit covers the frozen reference rule. Optimal
parsing/tree search and high-level splitting still remain required work.

`bt-1` proves51/51 byte-identical one-shot/stream frames and102 opposite decodes.
`bt-edges-2` proves206/206 additional exact frames and412 opposite decodes: all four
small-window payload types, requested levels across source-size ranges,4.5MB text/
random/zero/overlap inputs, real4MB-window streaming wraps and0..17byte final blocks.
The oracle records actual ZSTD_getCParams strategy6 for active matcher cases.
The earlier bt-edges-1 harness stopped before compression because standalone
ZSTD_getCParams(size=0) means unknown-size, not a known-empty frame. That failure
is preserved; empty frames need no match-finder selection and remain controls.

`bt-reuse-1` proves32/32 chronological reused-context frames, mixing tree, row-hash,
default, one-shot and wrapped streaming paths over two rounds. The actual frozen
C++ library loader path is verified. All289 new C++ size/hash goldens are retained
in `bt_lazy_1.5.6.tsv`, with full raw frames, input hashes and provenance external.

Workspace/all-target Clippy passes. Six immutable signed codec executables in each
of debug/release pass all19 tests,0ignored, including1153 explicit small-window/
row/tree goldens per build and original reference/decoder/overflow controls.
All927 source inputs remain verified; source4 preserves928 overlay entries.
Evidence: tests-{debug,release}-3/verification.json, clippy-4-terminal.json,
bt-golden-provenance-1.json, source-4/manifest.json. These are codec component checks;
shipping Parquet/spill/storage integration has not been refreshed for this source.

Per-case speed/RSS is retained in bt-speed-memory-diagnostic-1.json without averages.
For300KB text, C++/Rust level13 is14.756/16.871ms with13.11/10.59MiB whole-process RSS;
level14 is18.057/19.578ms with13.11/10.58MiB; level15 is18.951/22.283ms with13.11/10.59MiB.
Frames are identical. These single native samples include process/DLL overhead in
RSS and are not final TPC-H/ClickBench acceptance or isolated working-table savings.
Default Fast/DFast allocation remains unchanged; there is no new table caching.

## Working optimal parser and splitter; SSD access interruption

The working source now adds adaptive optimal pricing, the optimal all-match tree
including three-byte lookups, repeat-history shortest-path reconstruction and
Ultra2's first-block statistics pass. The original strategy caps have been removed.
A signed standalone parser build passes seven default C++ small-window smoke frames
and all768 four-payload/sixteen-size/levels11..22 default C++ controls, with1536
opposite decodes. These small windows do not enable the reference block splitter.

The preserved default large-input cohort is still incomplete: opt-known-1 matches
only9/29 default C++ frames, although all58 opposite decodes pass. The inspected
mismatches have additional C++ intra-block partitions. Two explicit diagnostic
oracles disable C++ splitting only to isolate parsing: opt-nosplit-1 matches29/29
frames, and opt-stream-nosplit-1 matches12/12 nine-MB streaming cases (including
external-dictionary wraps), with82 opposite decodes combined. Those diagnostic
flags are not a product setting or a waiver of default C++ parity.

The working split.rs source now implements entropy-cost estimates, recursive
partition derivation and raw/RLE/compressed partition emission with separate
coding/decoding repcode histories. Its build has NOT started: macOS denied creation
of the next external artifact directory with EPERM, then denied reading existing
SSD evidence as well. The volume remains mounted at its required path, reports
writable media and about252GiB free. Process ancestry is Conductor/runtime/Codex/
Python. The access cause is not conclusively diagnosed; restoration of access was
requested. No permissions, mounts, links or large-output destinations were changed.

While access is pending, a small source fixture was generated in RAM from the
existing internal signed C++ reference library, whose SHA256 and actual loaded
path match the preserved reference. Three original input/frame pins calibrate the
independent Python generator. optimal_small_1.5.6.tsv contains768 default fresh
frames and32 actual chronological default C++ context frames; the new Rust tests
have not yet run. Small generator/provenance notes are in .context. Cross-check
these768 source goldens against the already-preserved SSD raw controls and archive
the context controls after access returns.

Source formatting checks pass. There is no new full type/Clippy/test or shipping
verification claim for the splitter source, no post-optimal full-source snapshot
yet, and no final performance acceptance. The previous source4 signed19-test stage
remains a before-optimal control. The goal remains active; this is the first goal
turn with the SSD access condition, not a completed or paused goal.
