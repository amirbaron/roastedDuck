# Secret lifecycle comparison

The signed macOS arm64 development library1135 matches the independently built
C++1.5.6 library934 for the public C API observations below. This is component
evidence, not final release acceptance.

The controls cover temporary and persistent secrets, create/replace/drop, and
readers with and without an established transaction snapshot. Each case uses
private secret storage configured before opening the database.

| Control | Cases per engine | SQL observations per engine | File checks per engine |
| --- | ---: | ---: | ---: |
| Disconnect, interruption, invalidated transaction | 36 | 456 | 72 |
| Actual COMMIT I/O failure | 12 | 188 | 12 |

All644 paired status/row/error observations and84 file-presence observations
match. Comparison substitutes only each engine's private artifact directory;
it does not remove diagnostic prefixes or SQL context. Native manifests were
authenticated before and after each engine's run. Actual process exits are zero.

The first control's historical case label `failed-commit` means an INSERT
conversion error invalidated the transaction: COMMIT succeeds by rolling it back.
It is not evidence that COMMIT itself failed. The second control creates a
directory at an attached database's absent WAL filename, inside its own private
case directory. COMMIT then returns `Failed to commit` on both engines and the
table insert rolls back. Secret catalog visibility, the reader's snapshot,
post-transaction visibility, persistent files and reopening a fresh database are
compared separately. Persistent secret-file side effects must not be assumed to
roll back merely because the attached database transaction failed; the raw
observations preserve C++'s behavior.

Evidence is under `.context/artifacts/platform-matrix-20261029/`:
`secret-lifecycle-paired-1143`, `secret-failed-commit-reference-1144`,
`secret-commit-failure-paired-1145`, and `secret-lifecycle-audit-1146`.
The reusable observers are `scripts/httpfs-tests/secret_lifecycle_control.py`
and `secret_commit_failure_control.py`.

This does not exercise concurrent credential refresh, third-party provider
implementations or custom C++ secret serializers. C++ extension binary loading
is outside the Rust engine's compatibility boundary; those APIs are not evidence
of a missing public selected-client feature. Concurrent built-in refresh remains
an explicit unverified case. Final signed release/platform refresh is still
required, together with the full SQL, shell, C API/ADBC, selected-client,
persistence/WAL and coupled speed/memory gates. The accepted uncached memory
policy and COPY tradeoff are unchanged.
