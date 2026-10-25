# Go client parity

Pin duckdb-go v2.10506.0 (commit addeaeb4ad0a9040c4f8dd70757e17e3852d5881),
the matching DuckDB1.5.6 client. Source archive SHA256:
dffa8d880adf55cf7c8cff5ed901e2be6dff50ff048d93532ed5695851dcea54.
The public API includes database/sql, Appender, Arrow, scalar/table UDFs, replacement
scans, logging, profiling, types, cancellation and errors. Inventory the complete pinned
source/tests before claiming coverage. Preserve its license and unchanged controls.

Use its documented duckdb_use_lib build tag to link only the explicit signed Rust C API
image; the default prebuilt static C++ engine is not an acceptable fallback. Build test
executables without running, verify native load commands, pin the selected image where
necessary, sign/strictly verify, and then run serialized tests. CPP controls use the same
client/dependency pins, explicit frozen C API image and test assertions. Keep all raw
per-test output, failures/skips, source/library/toolchain hashes and runtime image proof.

The installed /usr/local/bin/go is Go1.10.3amd64, while the selected module requires
Go1.24.0. Prepare an isolated official arm64Go toolchain on the dedicated SSD, verifying
its published archive checksum; do not alter the system Go installation. Go module/build/
temporary/telemetry caches and test fixture files must also stay on dedicated storage.
Explicit fixture-path adaptations must retain assertions and match both controls.

Authoritative contracts:
https://pkg.go.dev/github.com/duckdb/duckdb-go/v2@v2.10506.0
https://github.com/duckdb/duckdb-go/tree/addeaeb4ad0a9040c4f8dd70757e17e3852d5881
https://go.dev/dl/?mode=json

A successful build is preparation only. Completion also requires public API/tests,
installation/loading, ownership/concurrency/cancellation, types/UDF/Arrow, cross-engine
file/WAL journeys and documented performance/platform acceptance. Broader clients and
the whole-port speed–memory/storage gates remain open.

## First complete control and Rust discovery

The signed CPP dynamic client with Arrow passes all652reported cases (including
subtests/examples), with255listed top-level Test functions all started/finished and
no skips/failures. Direct launch with child nice increment preserves DYLD loader proof.
The first launch through protected /usr/bin/nice passed tests but lost loader tracing;
its evidence remains separate. Runtime/binary/source hashes are preserved in
`.context/artifacts/go-client-20261025/cpp-test-2/summary.json`.

The first Rust run terminates early at TestErrConnect/local_config_option: schema=main
incorrectly succeeds as a startup option, then the upstream helper dereferences its
expected error. Prior outcomes include197pass and5fail records (parents/subtests counted),
with many tests still unstarted. Other observed failures: explicit access_mode=read_write
on an in-memory connection reads back automatic, and query-appender VARIANT shredding
exposes no typed value segments after checkpoint. These are open product gaps, not
accepted client deviations. Keep the failed Rust run and unstarted list intact.

Match frozen DBConfig::SetOption: the15DUCKDB_LOCAL custom options (and their aliases)
have no global setter and reject startup configuration. Generic settings with a LOCAL
default scope may still be set at startup; do not reject them based only on displayed
scope. Match AccessModeSetting::GetSetting using actual instance configuration, not a
hardcoded settings-table default. Verify direct C API controls and signed SDK/Go tests
before regenerating broader core/shell/Python acceptance on a changed engine.

The first output-parser invocation also accidentally generated54,548,250bytes in the
system Go cache. Only717new files born during its exact observation window were moved
to dedicated SSD quarantine after checking no active handles, preserving hashes and
original paths; pre-existing cache files were untouched. Receipt:
`parser-cache-quarantine-1/manifest.json`. The runner now parses saved Go test markers
in Python, so it cannot compile the parser or create another default Go cache.

## Startup fixes and first full Rust suite

Signed SDK image2passes all4client-settings regressions. New immutable Rust C API image
`config-library-1/libduckdb.dylib` SHA7161c8893b34827377a0dfc96b359062f4a616fe109bbb26608d5ef1b0cd777a
retains547exports and matches all27direct configuration observations, closing all19
prior mismatches. Original automatic mode remains automatic; explicit modes read back
correctly in both current_setting and duckdb_settings. Inputs/raw outputs and a source
snapshot accompany config-comparison-after-1.json.

The complete root Go suite with Arrow now finishes:647pass/5fail records across the
same652case IDs as CPP, with no unstarted/unfinished tests and the selected Rust library
verified by the loader. Five records represent four open categories: VARIANT typed
segment visibility, SET external_threads=-1 not rejected, custom CHECKPOINT_LATENCY
remaining zero, and a complex TIMESTAMP_NS/timestamptz bind error being masked by a
LATERAL DEFAULT-clause error (parent/subtest both fail). Preserve individual errors;
none are waived. Evidence: rust-test-2 and full-suite-comparison-2.json.

Next verify external_threads input conversion/callback constraints against the frozen
public API, preserving exact error types and messages and valid control settings.
Then close remaining categories and rerun the entire suite. Core/shell/Python/Swift
acceptance on earlier images predates these core changes and needs a final refresh.
