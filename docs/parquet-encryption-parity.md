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
