# Accepted divergences

Test files rustdb fails on purpose, because passing them would take behaving worse than
the C++ engine or loading C++ code. Every entry names what the test asserts and why
rustdb differs. Anything not listed here is a bug to fix.

## Leaner than C++

These tests assert that DuckDB runs out of memory or spills to disk at a given limit.
rustdb fits the same work into less memory, so the expected error or spill never
happens (the porting goal is peak memory at most C++'s, never spilling more).

| test | asserts | rustdb |
|---|---|---|
| `test/sql/tpch/dbgen_error.test` | `CALL dbgen(sf=1)` under `memory_limit='100MB'` spills to an uncreatable `temp_directory` and fails | generates SF1 within 100 MB, never touching the directory |
| `test/issues/internal/test_5457.test` | `EXPLAIN SELECT 42` fails under `memory_limit='300KiB'` | explains it within the limit |
| `test/sql/settings/enable_caching_operators.test_slow` | a hash-join probe over 8 MB strings with caching operators enabled runs out of memory under `memory_limit='300mb'` | streams the strings through within the limit |
| `test/sql/json/issues/read_json_memory_usage.test` | `read_json_objects` with 8 threads runs out of memory under `memory_limit='50MiB'` | reads the file within the limit |

## C++ extension binaries

`.duckdb_extension` files are C++ shared libraries built against DuckDB's internals; a
Rust engine cannot load them. Tests that `LOAD` the extension files of the C++ build
(`test/extension/load_extension.test`, `loadable_parser_override.test`,
`test_alias_point.test`, `test_custom_type_modifier*.test`, `test_tags.test`,
`load_test_alias.test`, `reset_global_extension_option.test`,
`consistent_semicolon_extension_parse.test`, `install_extension.test_slow`,
`concurrent_load_extension.test`) fail with
"Not implemented: loading the extension file".

## Uninitialized reference VARIANT padding

C++ 1.5.6 can include uninitialized bytes in a VARIANT BLOB when a NULL
primitive follows a non-NULL value during conversion. Its primitive converter
retains the previous payload size and advances the NULL row's offset without
writing those bytes. Public raw `hash(v)` consequently depends on allocator
contents for these inputs. Two identical native C++ processes differ in 423
of 5,000 hashes; the physical probe verifies that all differences from Rust
are confined to eight unused BLOB bytes after the final VARIANT_NULL entry.
Logical payloads/metadata match, and every normalized hash matches across both
C++ repeats and Rust. Rust initializes unused bytes and does not reproduce
process memory contents.

This exception is limited to raw hashes of those undefined padding bytes.
The 55 defined-value hash controls, a separate 5,000-row fully initialized
VARIANT workload's raw hashes, and logical grouping/join results match C++
literally. The original failed raw comparison remains preserved rather than
being normalized away. Evidence: external
`nested-hash-parity-20261027/{physical-2/padding-verification.json,
public-hashes-1/differences.json,public-hashes-2/verification.json}`.
