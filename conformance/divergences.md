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
| `test/sql/outofcore/leftover_temp_files_issue_6420.test_slow` | a temporary copy of SF1 `lineitem` spills under `memory_limit='1GB'` | holds it in 996 MB without spilling (C++: 994 MB in memory + 93 MB spilled) |
| `test/sql/tpch/dbgen_error.test` | `CALL dbgen(sf=1)` under `memory_limit='100MB'` spills to an uncreatable `temp_directory` and fails | generates SF1 within 100 MB, never touching the directory |
| `test/issues/internal/test_5457.test` | `EXPLAIN SELECT 42` fails under `memory_limit='300KiB'` | explains it within the limit |
| `test/sql/json/issues/read_json_memory_usage.test` | `read_json_objects` with 8 threads runs out of memory under `memory_limit='50MiB'` | reads the file within the limit |

## C++ extension binaries

`.duckdb_extension` files are C++ shared libraries built against DuckDB's internals; a
Rust engine cannot load them. Tests that `LOAD` the extension files of the C++ build
(`test/extension/load_extension.test`, `loadable_parser_override.test`,
`test_alias_point.test`, `test_custom_type_modifier*.test`, `test_tags.test`,
`load_test_alias.test`, `reset_global_extension_option.test`,
`consistent_semicolon_extension_parse.test`, `install_extension.test_slow`) fail with
"Not implemented: loading the extension file".
