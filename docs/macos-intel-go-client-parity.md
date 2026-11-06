# macOS Intel Go client checkpoint

On 2026-10-30 both frozen engines pass all 652 pinned Go root-suite cases, including
Arrow, with identical case IDs/verdicts and no unfinished/unstarted cases. Four
independent typed checkpoint/WAL journeys also pass. Execution is x86_64 under
Rosetta on an arm64 Mac, not native Intel performance evidence.

The client is duckdb-go v2.10506.0, commit
`addeaeb4ad0a9040c4f8dd70757e17e3852d5881`; all 79 source files and the bindings
header match the frozen pin. Go 1.27.3 builds with GOOS=darwin/GOARCH=amd64 and
explicit Intel compiler wrappers. Paths containing SSD spaces must be quoted for
Cgo's compiler argument parser. A preserved compiler probe fails unquoted and
passes quoted; the initial `go env` probe was insufficient and is retained.

The signed suites load exactly these frozen dylibs, verified through actual
dyld loading and before/after content manifests:

| Engine | Native SHA256 |
|---|---|
| C++1.5.6 | `47b40c6ae52d38c6643a6c2b7abf5271975144856255ce6c8573f5b69333a5fe` |
| Rust | `4f826321a230eee4312a175150c990051209ed30c72781e0268ec72cac130104` |

Separate signed Go modules check prepared inserts, Appender, NULLs, DECIMAL,
TIMESTAMP, BLOB, list/struct projections, duplicate-key rejection and rollback.
Both writer directions cover checkpoint and nonempty WAL, with before-replay
snapshots, receiving-engine mutation/checkpoint and original-engine readback.
C++ and Rust WAL snapshots are 440 and 429 bytes respectively. Every result verifies
the selected image with `dladdr`; consumer sources and binaries remain unchanged.

Evidence under `.context/artifacts/platform-matrix-20261029/`:

- `work/apple/go-intel-full-2/verification.json`: complete suites and four journeys.
- `work/apple/go-intel-{cpp,rust}-build-1/`: signed suite binaries and pinned inputs.
- `work/apple/go-intel-full-2/{cpp,rust}-consumer/`: independent signed modules.
- `go-intel-compiler-preflight-{1,2}/` and `go-intel-compiler-path-fix-1.json`:
  original compiler-path diagnostics and correction.

The first queued Go attempt stopped because its preceding Swift gate had not
produced a successful receipt. It executed no Go build/tests; its error remains
preserved. The successful fresh driver2 runs only after Swift passes.

Other architectures, final common-source release artifacts and coupled
speed/memory acceptance remain separate gates. No engine table cache or memory
budget was increased for these checks.
