# First-party extensions

FTS and VSS stay in the roastedDuck monorepo and ship in the engine. Developers
use `LOAD fts` and `LOAD vss`; they do not clone extension repositories, compile
separate binaries, configure an extension server, or install another package.
See the [user guide](../website/docs/extensions.md) and
[runnable SQL examples](../examples/extensions).

## Repository and release decision

Repository location and distribution are separate choices. Upstream keeps FTS
and VSS in separate source repositories, but also supports statically linked
extensions ([distribution overview](https://duckdb.org/docs/current/extensions/overview)).
A separate source repository would not make roastedDuck's Rust integration a
loadable plugin. FTS integrates with pragma expansion; VSS uses internal catalog,
index lifecycle, MVCC and optimizer traits that must evolve with the engine.
Keep those changes atomic, with one lockfile and one compatibility/release version.
There is no separate FTS/VSS package version, Rust plugin ABI, or C++ adapter.

The independent algorithm crates are already separated from engine integration:

| Layer | FTS | VSS |
| --- | --- | --- |
| Algorithm | `crates/snowball` | `crates/hnsw` |
| SQL registration | `crates/roastedduck/src/fts.rs` | `crates/roastedduck/src/vss/` |
| Storage hooks | Ordinary tables/macros | `crates/storage/src/index/hnsw.rs` |
| Contract | [FTS spec](../docs/fts-extension-spec.md) | [VSS spec](../docs/vss-extension-spec.md) |

The `fts/extension.toml` and `vss/extension.toml` files are maintenance inventories,
not runtime plugin manifests. They point to the sole implementations; do not copy
code here. `scripts/extension-smoke.py` validates their paths and executes their
examples against an explicitly selected engine binary.

Split an algorithm into its own repository only when it has independent users,
a stable standalone API, and its own release/testing ownership. Split engine
integration only after defining and testing a versioned Rust source interface.
Any future binary distribution requires a separate ABI, signing, platform and
upgrade design. The stable C extension bridge remains its own track.

## Contributor workflow

1. Update the pinned source/patch and SQL-surface spec together. Preserve upstream
   licenses. Do not silently target current upstream main.
2. Change algorithm, integration and regression tests in one PR. No default-off
   build features; no new C/C++ product dependencies or USearch reuse.
3. Build normally: `cargo build --release -p roastedduck-cli --bin roastedDuck -j 1`.
   On the local Mac, run `scripts/sign.sh target/release/roastedDuck` before execution.
4. Run `python3 scripts/extension-smoke.py --cli target/release/roastedDuck --out .context/artifacts/extension-dx-check`.
   Use a fresh output directory on the external SSD in local Conductor workspaces.
5. Run the affected Rust tests and the pinned upstream SQL suite against both
   engines. Follow the spec's Snowball/interoperability or recall/MVCC gates when
   those behaviors change. Run `scripts/conformance.sh` after integration.

The smoke check is a loading/developer-journey check, not a replacement for the
full compatibility gates. It uses isolated extension paths and an unreachable
repository to detect accidental download dependence. It records binary/input
hashes, SQL, raw output and expected errors. Package builds inherit these same
implementations; check actual artifact availability before claiming a public
release contains them. Do not publish this branch's feature claims as post2 facts.

The existing native candidate qualification workflow also runs this smoke check
against each built SDK CLI and retains its receipts under
`native-qualification/search-extensions`. This wiring reuses the normal native
build; local receipts do not claim a hosted CI run or new platform qualification.


## Verified developer-experience checkpoint

Installer commit `3def127d` passes strict runtime Clippy, all 22 signed runtime
unit tests, and 14 CLI developer journeys. The strict documentation build passes.
Full `scripts/conformance.sh` after integration passes its unchanged ratchet:
4,801 PASS / 76 SKIP / 20 documented FAIL across 4,897 files, zero regressions,
and no changed statuses or failure classifications versus the previous candidate.
The existing five HNSW persistence deferrals remain; this does not add persistence.

Local receipts under `.context/artifacts/` are `extension-dx-build-2/` (successful
build/test stages), `extension-dx-5/` (final developer journeys),
`extension-docs/build-final.log`, and `extension-dx-conformance-1/`. Earlier smoke
failures are retained: the initial run exposed network-dependent INSTALL; later
harness corrections use the documented separate LOAD startup statement and the
pinned VSS autoload exclusion. No search algorithm or recall threshold changed.
The CI configuration is validated locally; no hosted CI or published-package
qualification is claimed here.
