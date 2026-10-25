# Frozen Python Decimal conversion parity

Three upstream assertions remain failing in the preserved Python package: positive
Decimal exponents and negative Infinity differ from the frozen C++1.5.6 client.
The goal is C++client parity; do not silently waive those assertions or substitute a
different numeric/type contract. Keep all ordinary finite controls and raw outcomes.

Source audit finds more boundaries than the three failures: small fractional values
use scale+1width in C++, positive exponents retain scale after multiplying coefficient,
nonfinite values use FLOAT and ignore Infinity sign, and large exponent handling uses
separate type inference and value conversion paths. C++PyDecimal::TryGetType and
ToDuckValue are in the pinned Python client's native/python_objects.cpp/header.

Before implementation, run bounded real installed-package comparisons for parameter
binding, explicit Value wrappers, insertion/casting, single/mixed Pandas object columns,
including signs/zeros/fractions, precision38, positive/negative exponents, nonfinite
values, and exponent/caster boundaries. Isolate each value in a fresh process, preserve
partial results on exceptions/crashes/timeouts, and record interpreter/package/engine
identities. Float bit patterns and Decimal tuples/types must remain visible; no broad
hexadecimal/address/value normalization. Extreme caster-only inputs do not imply
Pandas extreme-exponent coverage.

Preserved Rust wheel4 is a conversion diagnostic baseline, not current-core acceptance;
core progress has changed since it was built. Build a new signed wheel after fixes and
verify focused conversions, unchanged rendering, complete Arrow19/21 fast matrices and
current query-progress behavior. Existing other discrepancies/client/storage/performance
gates remain open. Store environments, logs, wheels and snapshots on the SSD, serialize
all work, and preserve the accepted lower-memory policy without retaining table blocks.

## Measured installed-package baseline

44values/256real outcomes yield106exact/150mismatches, with no child crashes or
timeouts on either engine. The immutable Rust wheel4 conversion baseline and frozen
CPPresults preserve description types, Decimal tuples, float bits and raw errors.
Families are parameters, explicit Value wrappers, DOUBLE insertion, DECIMAL casting,
and single/mixed Pandas columns; four large exponent-caster cases are binder-only.
Evidence: `shell-argv-20261025/decimal-{cpp,rust-baseline}-1.jsonl` and
`decimal-comparison-baseline-1.json`. A provisional36valuecomparison is retained
separately as partial observation, not terminal acceptance.

Port the frozen contract explicitly: fractional width uses scale+1when leading zeros
require it; positive exponents retain decimal scale; the cached19power multiplier
uses its frozen integer-width/wrap behavior; value conversion narrows scale tou8,
whereas Pandas type inference uses the full exponent. Nonfinite values use FLOAT,
Infinity sign is discarded, signaling NaNs are NotImplemented errors, and out-of-i32
exponents use the frozen caster error. These are compatibility semantics, including
the frozen client's numerical quirks; existing failing tests must pass without edits.

Keep Pandas inference separate from row conversion, including nested values and
error origin, so ordinary unsupported-object fallback is unchanged while Decimal
constructor errors propagate like CPP. Avoid the current zero-extension-before-width
check: coefficient conversion must remain bounded by the supported decimal width,
not allocate a string whose length is an arbitrary positive Python exponent.
