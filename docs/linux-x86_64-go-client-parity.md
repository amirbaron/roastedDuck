# Linux x86_64 Go client verification

The pinned DuckDB Go v2.10506.0 root suite, including Arrow, passes all 652 cases
on both frozen C++ 1.5.6 and signed Rust source 46. Every verdict matches; no
selected, unfinished or unstarted tests are hidden. Both test executables are
ELF x86_64, authenticate before/after execution, and load exactly their selected
engine. These are Rosetta functional results in the dedicated ARM Linux VM,
not native x86_64 performance measurements.

The Rust library SHA256 is
`5c8c496ff9bb962fef9e7fb8b612e4b3b5051ade47f944a744f610f1a8e9a0fa`;
the C++ comparator is
`7a0d906abac7f6e6a4d72bff86777292e5ba48bb24895f90cb0212ca45e70746`.
All 79 upstream Go source files match the existing pin. Official Go 1.27.3 AMD64
toolchain input and its complete extracted closure are hash checked and signed.
Builds use one package worker, low priority and isolated SSD caches. Test runtime
containers use 7 GiB RAM without swap.

A separate Go module then builds against each library with the pinned client,
without modifying its three existing consumer source files. It verifies prepared
statements, appender columns, Unicode, NULLs, BLOBs, DECIMAL, TIMESTAMP and nested
LIST/STRUCT logical values, plus primary-key errors and rollback.

All four typed persistence journeys pass: C++ to Rust and Rust to C++, each with
a checkpoint and an actual nonempty WAL. Snapshots and hashes are preserved before
receiver replay. The receiver mutates/checkpoints the database, and the original
engine reads the expected updated rows. Each phase reports the actual library
image; all consumer/engine signatures reauthenticate afterward.

Evidence under external `.context/artifacts/platform-matrix-20261029/`:

- `go-amd64-toolchain-inputs-50.json`: official input checksum and signed toolchain.
- `linux-amd64-go-pair-51/verification.json`: complete 652/652 paired verdicts.
- `work/linux-amd64/go-{cpp,rust}-build-51/`: signed root-suite executables.
- `work/linux-amd64/go-{cpp,rust}-runtime-51/`: individual events, loaders and original logs.
- `go-amd64-consumer-inputs-55.json`: unchanged separate consumer inputs.
- `linux-amd64-go-consumer-55/verification.json`: build/sign/runtime summary.
- `work/linux-amd64/go-consumer-journeys-55/verification.json`: all four journeys and before-replay snapshots.

This closes the stated Linux x86_64 Go component gates for source 46. Remaining
Python/Swift/ADBC coverage, other target refreshes, final common-source verification
and coupled speed/memory acceptance remain open.
