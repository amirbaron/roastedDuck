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

Status: existing implementation; fresh acceptance pending.
