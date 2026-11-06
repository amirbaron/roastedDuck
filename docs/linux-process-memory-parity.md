# Linux process memory limits

The Linux shell comparison found a real default-memory discrepancy: inside a
7 GiB cgroup, C++ reported 5.5 GiB while Rust reported 6.1 GiB. Rust used VM
physical memory instead of the process/container allocation.

The buffer manager now uses process-available memory for its default 80% budget.
Linux detects cgroup v1/v2 controller membership with namespace root fallback and
caps it at physical memory. SLURM node allocation takes precedence over per-CPU
allocation, with C++ decimal units and fallback behavior. Physical-memory queries
remain separate. Integer budgeting multiplies before dividing, using a wide
intermediate. No table cache or higher memory budget is introduced.

The signed source-45 buffer suite passes all 17 tests, including cgroup paths,
root fallback and scheduler precedence. Formatting and strict Clippy pass on
source 46. Its full-LTO signed optimized build passes the live C++ comparison:

| Allocation | C++ | Rust |
|---|---|---|
| 7 GiB container | 5.5 GiB | 5.5 GiB |
| Node 1500 decimal MB, overriding CPU allocation | 1.1 GiB | 1.1 GiB |
| Node 1.5G | 1.1 GiB | 1.1 GiB |
| 250M per CPU, three allocated CPUs | 572.2 MiB | 572.2 MiB |
| Invalid node allocation blocks per-CPU fallback | 5.5 GiB | 5.5 GiB |

All comparisons are literal setting outputs. The same source-46 library passes
the full original 30-file/155-case C API suite with 947,474 assertions and 547
shipping exports. Its SHA256 is
`5c8c496ff9bb962fef9e7fb8b612e4b3b5051ade47f944a744f610f1a8e9a0fa`.
Native and content signatures authenticate before and after execution.

Evidence under external `.context/artifacts/platform-matrix-20261029/`:

- `linux-amd64-memory-unit-run-45/`: signed buffer tests.
- `linux-amd64-memory-check-46/`: formatting and strict Clippy.
- `linux-amd64-native-build-46/` and `linux-amd64-native-freeze-46.json`: successful build and immutable signing.
- `work/linux-amd64/live-memory-46/verification.json`: all five paired controls.
- `work/linux-amd64/capi-native-rerun-46/verification.json`: full C API refresh.

This is component correctness evidence under Rosetta in the ARM Linux VM.
It does not accept performance or complete the remaining release matrix.
