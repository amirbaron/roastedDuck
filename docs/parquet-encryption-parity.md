# Parquet encrypted-write parity

The 2026-10-27 completion audit found a real supported-functionality gap masked by
shared `require httpfs` skips. Fresh separate Python processes use pinned C++ and the
current signed source-5 Rust package with synthetic keys only. For 16/24/32-byte keys,
C++ writes `PARE` files, reads all 5,000 integer/nullable Unicode/list rows and performs
COPY FROM successfully. Rust rejects every write with NotImplemented: Parquet encryption.
The default C++ path works; force_mbedtls_unsafe is unnecessary for these fixtures.
Evidence: external `parquet-encryption-20261027/before-1-index.json` and both reports.

Implement the frozen C++ footer-key path: AES-GCM encrypted page headers/data and footer,
plain FileCryptoMetaData, PARE magic, correct byte offsets/sizes and row-group ordinals.
Use shared RustCrypto AES-128/192/256 primitives and operating-system nonces; replace the
reader's private software AES with the same authenticated primitives. Preserve unencrypted
output and the low-memory policy. Add encryption_config to COPY TO/FROM binding and
prepared-plan serialization without exposing key bytes in Debug output.

C++ 1.5.6 also rejects column_keys with the same NotImplemented message; that is a shared
limit, not a requirement to exceed C++. Keep that behavior. Verify all supported key sizes,
multiple pages/row groups, nullable/nested types, wrong/tampered/truncated data, registered
output, and actual C++/Rust cross-reading, including COPY FROM. Then refresh final signed
artifacts and affected SQL/client/persistence/performance gates. No implementation or
completion is claimed by this planning checkpoint.


## First implementation component checkpoint (2026-10-27)

The working writer now emits encrypted page headers/payloads and footer with PARE magic,
plain FileCryptoMetaData, AES_GCM_V1 metadata and encrypted row-group ordinals. Bloom filters
are disabled on encrypted writes, as in frozen C++. COPY TO/FROM accepts the supported
footer-key configuration. Resolved key serialization is implemented for COPY and scans;
column_keys retains the shared C++ NotImplemented behavior.

Shared RustCrypto AES-128/192/256 GCM primitives now handle Parquet modules, with operating-
system nonces, authenticated AAD and detached tags. The reader's private S-box/AES/GHASH
implementation is removed. Ordinary database/WAL AES-256 states are unchanged; plaintext
Parquet continues through its existing branch. FooterKey Debug output omits raw key bytes.

Workspace/all-target checking and Clippy with warnings denied pass. Three immutable signed
release SDK regressions pass: 5,000 nullable Unicode/list/struct/decimal rows across all
three key sizes and four codecs, complete read/COPY FROM equality, wrong/missing keys,
truncation, prepared COPY, encrypted scan-plan serialization and the shared unsupported
column-key setting. Signed unit suites pass 10 crypto and 28 Parquet tests, including
independent GCM known answers and authentication failures. Two existing Snappy comparison/
benchmark tools remain ignored; they are not encryption exclusions.

Evidence: external `parquet-encryption-20261027/{source-1/manifest.json,
sdk-verification-1.json,units-verification-1.json,clippy-1-terminal.json}`. The complete
overlay has 913 entries/912 verified build inputs. Shipping images are rebuilding;
independent C++/Rust cross-read/copy and broader final artifact verification remain
pending. This is component evidence, not a completed encryption or overall port claim.

## Signed shipping cross-engine checkpoint

The native release build exited successfully, then immutable copies were signed and
strictly verified. The shipping library retains all 547 C API exports and zero ADBC
test-hook exports. Fresh separate C++ Python and Rust C ABI processes verify 48 file
cases: each writer's 12 key/codec combinations is read by both engines. Every complete
5,000-row EXCEPT ALL comparison and COPY FROM comparison passes, including nullable
Unicode, lists, structs and decimals over multiple row groups. Missing and incorrect
keys are rejected by both engines. Each process verifies exactly one selected engine
image; all 912 build-input hashes and shipping signatures remain stable.

Evidence: external `parquet-encryption-20261027/{native-images-1.json,cross-1/verification.json}`.
Signed library SHA-256: `64c4931b39d048c954332b4768ccfd5631367401a4e1f864a2963e3cfd674528`.
Corruption controls, registered-backend Python journeys and broader final artifact gates
remain pending. These cross-engine results do not transfer the earlier complete
source-5 corpus/client/performance results to the new images.

## Installed Python and corruption checkpoint

The fresh signed wheel installs successfully in both pinned Arrow 19/21 environments;
its native SHA-256 is `c7f097194a8c5378ae9896d31fd370c499eaa7bed6cd540efd6eeeb2d43f62d3`.
Each environment passes 24 installed-binding file journeys against separate C++ controls:
local and registered-memory output/read/COPY FROM for all three key sizes, prepared COPY
writes, scan-plan serialization and complete nested/nullable 5,000-row equality. Both
engines cross-read each writer's files. Exactly one selected engine image is loaded in
each process. All 912 build inputs and wheel signatures verify stable.

For each file journey, wrong/missing keys, altered encrypted page bytes, altered footer
bytes and truncated files are rejected. The final repeated controls use identical local
and memory paths across processes: all 60 paired error types and full messages per Arrow
environment match C++ literally, with no normalization. Preliminary Arrow 19 reports with
different generated memory paths remain preserved separately as stage 1; accepted stage 2
is the exact-path comparison.

Evidence: external `parquet-encryption-20261027/{wheel-manifest-1.json,
python-encryption-arrow19-2/verification.json,python-encryption-arrow19-2/error-comparison.json,
python-encryption-arrow21-2/verification.json,python-encryption-arrow21-2/error-comparison.json}`.
The full fast Python suites are refreshing next. Full corpus, C API/ADBC, Go/Swift,
shell, persistence/WAL and final speed/RSS verification still remain separate gates.

## Complete fast Python matrices and unskipped SQL checkpoint

Both complete fast suites match the frozen C++ case IDs, verdicts and skip reasons:
Arrow 19 has 3,241 records (3,205 pass, 27 skip, eight xfail, one existing XPASS);
Arrow 21 has 3,492 (3,455 pass, 28 skip, eight xfail, one existing XPASS). All 300
upstream test/data files match the pinned original archive; seven stubs per environment,
source overlay, 912 build inputs and installed native signatures verify stable. Evidence:
external `parquet-encryption-20261027/python-refresh-verification-1.json`. These results
cover the complete fast suite, not every optional Python integration/platform.

Both upstream `parquet_encryption.test` and `parquet_encryption_tpch.test_slow` now pass
on the signed Rust runner and separate C++ oracle. The supplemental copies replace only
`require httpfs` with `SET force_mbedtls_unsafe=true` to enable the frozen reference's
fallback writer; all SQL statements and expected answers remain unchanged. The latter
file exports/imports encrypted SF1 tables, checks all 22 TPC-H answers, then checks all
22 again through encrypted-Parquet views. The first also covers invalid/base64 keys and
the shared unsupported column-key configuration. Original and adapted file hashes, both
engine verdicts and signed runner/oracle/reference identities are retained under external
`parquet-encryption-20261027/sql-encryption-1/verification.json`. Original canonical
corpus requirements/skips remain unchanged.

The supported footer-key component now has independent cross-engine/file/backend/
corruption evidence. C API is refreshing on the shipping library; ADBC, Go/Swift, full
canonical SQL/shell, persistence/WAL and coupled final performance still remain gates.
