# roastedDuck 1.5.6.post4 publication

Released source: `a8ff63a80a1fd10028a0e54b773099f10f345921`, compatibility target
DuckDB 1.5.6, public-preview status. [PR #15](https://github.com/amirbaron/roastedDuck/pull/15)
merged the bundled httpfs, aws, azure, icu and postgres_scanner implementations;
[#16](https://github.com/amirbaron/roastedDuck/pull/16) and
[#17](https://github.com/amirbaron/roastedDuck/pull/17) carry the two fixes that the
qualification attempts below required. The maintenance branch `v1.5-variegata` was
fast-forwarded to main's merge of #17, whose tree is identical to the released commit.

## Public packages

```sh
brew install amirbaron/roastedduck/roastedduck
python -m pip install roastedduck
```

- [GitHub release: 43 checksum-verified assets](https://github.com/amirbaron/roastedDuck/releases/tag/v1.5.6.post4).
- [PyPI 1.5.6.post4](https://pypi.org/project/roastedduck/1.5.6.post4/): 15 wheels plus one source distribution.
- [Homebrew tap](https://github.com/amirbaron/homebrew-roastedduck) and [bottles](https://github.com/amirbaron/homebrew-roastedduck/releases/tag/v1.5.6.post4).

## Verification

| Gate | Evidence |
| --- | --- |
| Fresh source builds, 15 Python ABI cells, C API/ADBC, Go, Swift, encryption and dependency audit | [Producer 44016875211](https://github.com/amirbaron/roastedDuck/actions/runs/44016875211) |
| Trusted PyPI publication and 15 ordinary unpinned index installs, each running the bundled-extension and encryption smoke checks | [Publisher 44048510639](https://github.com/amirbaron/roastedDuck/actions/runs/44048510639) |
| Homebrew installation, package tests, bottle creation and re-pour tests | [Tap build 44048570643](https://github.com/amirbaron/homebrew-roastedduck/actions/runs/44048570643) |
| Public bottle downloads with verified `poured_from_bottle` and package tests on all three platforms | [Public install 44050087459](https://github.com/amirbaron/homebrew-roastedduck/actions/runs/44050087459) |

The strict assembler verified every source, wheel, native, Swift and Apple payload
against the producer's artifacts and wrote `launch-acceptance.json` and `SHA256SUMS`.
All 43 GitHub asset digests and sizes were checked before the draft became public.
PyPI's API serves exactly the 16 accepted distribution hashes, and a fresh local
Python 3.13 index install passes the provider-hash, bundled-extension and encryption
checks. The Homebrew bottles are genuine `brew bottle` outputs whose machine-code
sections match the qualified SDK CLI. An upgrade from post3 in an isolated local
Homebrew prefix poured the public post4 bottle; all 14 developer journeys and every
assertion of the formula test pass on it. The local `brew test` harness itself stops
before SQL on this Mac's outdated Command Line Tools, as for post3; the same package
tests pass on the three clean CI platforms.

Local Apple qualification rebuilt and authenticated all three ARM64 slices (547 C API
exports each), passed 133 Mac and 133 simulator Swift cases, all four simulator
encryption profiles, Mac encryption and relocation, four cross-engine storage journeys,
and a consumer of the final archive. Physical-device execution and notarization remain
unverified. The local release-toolchain SQL corpus on the released commit: 4,845
passes, 33 skips, 19 documented failures, zero status changes and zero ratchet
regressions.

## Earlier qualification attempts

Two producer runs on earlier commits did not lead to a release and remain in the
workflow history:

- [43966533558](https://github.com/amirbaron/roastedDuck/actions/runs/43966533558)
  (`25f727ae`): the Linux x86_64 build failed after building every wheel, when the
  notice collector's unauthenticated GitHub API call hit a rate limit. #16 fetches
  that file from a pinned commit and checks the same blob hash; the notices are
  byte-identical.
- [43982129204](https://github.com/amirbaron/roastedDuck/actions/runs/43982129204)
  (`9b82fa94`): the Linux arm64 Python 3.10 smoke hit `INTERNAL Error: VSS execution
  without an active transaction context`. A pending query dropped its planning context
  before installing the running query's own, and a worker thread could initialize an
  HNSW scan in between; the race predates post3. #17 keeps the context alive for the
  execution. A regression test failed 2 of 8,000 queries before the change and none
  of 80,000 after it, and the released Mac wheel ran 6,000 such queries without error.

## Scope

The bundled extensions were tested against local MinIO, Azure, PostgreSQL 17, STS/SSO,
metadata and proxy fixtures, not live AWS, Azure, Google Cloud, Redshift or Aurora
accounts. The remaining differences are listed in the
[compatibility guide](../website/docs/compatibility.md#bundled-extensions).
