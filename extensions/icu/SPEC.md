# ICU: DuckDB 1.5.6 contract

Upstream engine commit: `069cc9f9b5be802405797faecc284961b07c70ef`.
Source: extension/icu, including its pinned ICU 66 data.

Existing roastedDuck's compatible implementation: crates/icu.
Surface: collations and sort keys, timezone-aware timestamp functions/casts,
TimeZone/Calendar settings, calendar/timezone enumeration and date/time ranges.
Preserve preloading and the existing pure Rust calendar/collation implementation.

Acceptance: unchanged upstream ICU SQL tests plus existing normalization, sort-key,
DST and non-Gregorian differential controls against C++ 1.5.6. Data upgrades need
separate qualification. No new runtime dependency on ICU C/C++.

Status: fresh source acceptance passes63/63 original files on both engines,
72,473 exact sort keys across137 locale/tag forms,82 matching public C API
records and6 native tests. Original files and prerequisites are unchanged; the
local-repository variable is scoped to its requiring test. Data SHA256 remains
9b5516e7609c98b7e25b6ed27ae60ae2ae49ebe1386fa8ef3bccb37b9df3d5ac.
Receipt: `.context/artifacts/popular-extensions/icu-verification.json`.
Final package/platform/source-freeze checks remain separate.
