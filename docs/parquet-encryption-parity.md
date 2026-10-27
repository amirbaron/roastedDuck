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
