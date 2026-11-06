# Frozen upstream ADBC tests

Runs all 64 cases of DuckDB 1.5.6 `test/api/adbc/test_adbc.cpp`. No test case or assertion
is removed. `prepare.py` checks both counts and records the source/adapted SHA-256 hashes.
The two database helper bodies and the private autocommit inspection use native APIs;
the private error initializer is dispatched to the selected implementation.

The Rust run enables the optional `rustdb-capi/adbc-test-api` feature. Its three bridge
functions expose borrowed C handles and the private test error initializer. Shipping
builds must leave this feature disabled. Build the test dylib, copy it to a dedicated
SSD directory, change its install name to `@rpath/libduckdb.dylib`, and sign it. Restore
and verify the feature-free normal build before running the harness.

```sh
ADBC_TEST_OUTPUT_DIR="$PWD/.context/artifacts/<task>/rust" \
ADBC_TEST_LIB_DIR="$PWD/.context/artifacts/<task>/test-lib" \
ADBC_TEST_BACKEND=rust \
TMPDIR="$PWD/.context/artifacts/<task>" \
nice -n 10 scripts/adbc-tests/run.sh
```

For reference calibration set `ADBC_TEST_BACKEND=cpp` and point the library directory
at the frozen C++ build. Only this reference run compiles `cpp_reference_bridge.cpp`
and uses the original private C++ connection/error helpers. The Rust run never compiles
that file or links the C++ DuckDB engine. Both use the unmodified frozen driver manager
and nanoarrow schema/metadata/allocator sources, with the existing C API test utilities.

The Arrow comparison still checks logical type parameters (except upstream's top-level
ENUM exemption) and runs SQL EXCEPT ALL for duplicate/NULL/numeric semantics. The adapter
uses an independent native connection, as upstream does. Its borrowed stream proxy
preserves the driver manager's release-identity checks when the public Arrow scan API
changes release on its own copy. Nested Value::ToString expectations use the engine's
VARCHAR cast because the legacy C API result getters do not support nested values.

All build outputs, databases and logs must go on the external SSD. The runner is serial
and signs the executable before it runs. Additional Catch filters may be passed as
arguments; completion requires a run with no filters. Keep failure evidence and verify
the shared-library load paths and absence of the bridge in shipping exports.
