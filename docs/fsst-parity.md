# FSST controlled encoder and storage parity audit

The live queue's historical5878-versus5877 segment-count observation is not yet
calibrated to a retained exact input. Frozen C++ storage analysis uses a randomly
seeded RandomEngine to choose vectors for training; Rust deliberately uses a fixed
seed for reproducibility. Do not assume a different stored symbol table proves a
codec bug without controlling that sampling. Final storage journeys remain required.

## Confirmed same-training-input counterexample (2026-10-29)

Independent signed tools compare DuckDB1.5.6's vendored FSST source with the current
Rust primitive using exactly the same byte strings. This bypasses outer storage
sampling. Eight of nine initial corpora have identical exported tables and compressed
records: empty batches, empty strings, short ASCII, BEEPBOOP, Unicode, long strings,
all byte values and random bytes. The embedded-NUL length corpus differs in both
table and compressed bytes, although all18 opposite-engine decodes succeed.
The C++ table has7symbols and Rust8; both serialized tables occupy40bytes.

A trace-only oracle retains the original compressed outputs exactly. The first
queue difference has equal gain28 and symbol value0x61: C++ extracts length7 before
length4, while Rust's extra length tie-break does the opposite. Frozen QSymbol
identity includes value/length, but queue priority compares only gain/value.
The unordered-set erase/reinsert order and actual heap tie handling therefore
matter. Preserve full controls; do not remove the NUL corpus or merely relabel
compatible output as exact parity.

External evidence: `fsst-parity-20261029/{inner-1/verification.json,ref-queue-1.log,
rust-queue-1.log}` plus raw input/table/compressed records. The first C++ tooling
link needed the frozen library's exception implementation, and the first Rust tool
called a test-only convenience decoder; both compile failures remain separate.
Valid probes use the regular decoder method and are signed before execution.

Next reproduce the frozen candidate-container and heap lifecycle, add independent
regressions for collision/erase/reinsert/growth/ties, then recompare all corpora and
public persistent storage/WAL in both directions. Keep the accepted speed–memory
policy and individual outliers; full release acceptance is not claimed here.
