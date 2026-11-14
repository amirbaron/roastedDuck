# roastedDuck 1.5.6.post3 publication

Released source: `afe4e2bb2baf79addf027b35b1ede98c2e05ad4f`, compatibility target
DuckDB 1.5.6, public-preview status. [Source PR #13](https://github.com/amirbaron/roastedDuck/pull/13)
merged the compatible FTS/VSS implementations and offline bundled installation.
The maintenance branch remains `v1.5-variegata`.

## Public packages

```sh
brew install amirbaron/roastedduck/roastedduck
python -m pip install roastedduck
```

Use a fresh Python environment; the distribution is `roastedduck` while its import
remains `duckdb`. Do not co-install the upstream `duckdb` distribution there.
Homebrew installs a separate `roastedduck` CLI and notices, without replacing
upstream `duckdb`. Both implementations load normally with `LOAD fts;` / `LOAD vss;`.

- [GitHub release: 43 checksum-verified assets](https://github.com/amirbaron/roastedDuck/releases/tag/v1.5.6.post3).
- [PyPI 1.5.6.post3](https://pypi.org/project/roastedduck/1.5.6.post3/): 15 wheels plus one source distribution.
- [Homebrew tap](https://github.com/amirbaron/homebrew-roastedduck) and [bottles](https://github.com/amirbaron/homebrew-roastedduck/releases/tag/v1.5.6.post3).

## Verification

| Gate | Evidence |
| --- | --- |
| Fresh source builds, 15 Python ABI cells, C API/ADBC, Go, Swift, encryption and dependency audit | [Producer 43584310679](https://github.com/amirbaron/roastedDuck/actions/runs/43584310679) |
| Trusted PyPI publication and 15 ordinary unpinned index installs | [Publisher 43604870953](https://github.com/amirbaron/roastedDuck/actions/runs/43604870953) |
| Native Homebrew installation, tests, bottle creation and re-pour tests | [Tap build 43604947328](https://github.com/amirbaron/homebrew-roastedduck/actions/runs/43604947328) |
| Public bottle downloads, verified `poured_from_bottle`, and FTS/VSS/index-plan tests on all three platforms | [Public install 43606039657](https://github.com/amirbaron/homebrew-roastedduck/actions/runs/43606039657) |

The security job initially exposed inconsistent GitHub terminal metadata despite
successful steps. Its check and aggregate acceptance were rerun successfully.
Older same-name artifacts are retained; assembly selects the latest record per
name and still verifies the producer/source identity, digest, size and complete
qualification records. No native or Python distribution bytes were changed for
that retry. Both attempts remain visible.

The strict assembler verified all source/wheel/native/Swift/Apple payloads and
produced `launch-acceptance.json` and `SHA256SUMS`. All 43 GitHub asset digests and
sizes were verified before making the draft release public. PyPI's API serves the
same 16 qualified distribution hashes. A fresh local Python 3.14 index install
also passes provider-hash, FTS/VSS, persistence and encryption checks.

Homebrew bottles are genuine `brew bottle` outputs. Each was tested before and
after pouring, and its machine-code section matches the qualified SDK CLI.
Their metadata declares `any_skip_relocation`; the formula pins the published
checksums. A local source-style Homebrew diagnostic hit this Mac's outdated CLT
check; no system developer tools were modified. Binary bottles avoid that source
build path. An ordinary public-tap install in an isolated external Homebrew prefix
successfully pours the published bottle, and all 14 developer journeys pass on
its installed CLI. The local `brew test` harness itself is blocked before SQL by
the host's outdated CLT; the same package tests pass on all three clean CI
platforms. Global Homebrew and system developer tools were not modified. The
earlier diagnostic payload was not published.

Local Apple qualification rebuilt and authenticated all three ARM64 slices,
passed 133 Mac and 133 simulator cases, all four simulator encryption profiles,
Mac encryption/relocation and four cross-engine storage journeys. The final
archive was extracted and consumed again. All generated runtime/build/cache data
was stored on the dedicated external SSD; no new internal-storage exception was
needed. The task-owned simulator was shut down afterward.

The required post-merge SQL run passes its unchanged ratchet: 4,897 files,
4,801 PASS / 76 SKIP / 20 documented FAIL, zero changed statuses. Earlier complete
Snowball and VSS recall receipts retain their tested-artifact scope; no search
algorithm or acceptance threshold changed during publication.

## Limits and receipt locations

HNSW remains in-memory only; five persistent HNSW originals remain deferred.
C++ extension binaries are not loaded. Physical iOS execution, Apple notarization,
Windows/Intel-Mac wheels, PyPy/free-threaded Python and arbitrary-input performance
claims remain outside the recorded scope. Historical benchmarks retain their
original candidate/date scope.

Detailed local receipts are under `.context/artifacts/release-post3/`:
`conformance/`, `mac-suite-2/`, `ios-suite/`, `apple-release/`,
`public-release-preparation/`, `publication/`, `pypi-provider-verification.json`,
`pypi-local-install/`, `homebrew-bottles/`, `bottles-public/`,
`public-brew-local/runtime-verification.json`, and the public bottle provider verification. Transient DNS failures and interrupted status requests
were retained; publication stayed gated on verified uploaded bytes.
