# Snowball for roastedDuck's compatible FTS implementation

This crate contains pure Rust generated algorithms and a byte-oriented runtime.
It does not use a C/C++ library, FFI, a Cargo build script, or build-time code generation.
The public entry point preserves DuckDB 1.5.6's 27 canonical stemmers, 86 aliases,
case-sensitive name lookup, `none`, and embedded-NUL behavior. SQL NULL propagation
is handled by the FTS scalar-function caller.

The source definitions and generator are pinned to
[`snowballstem/snowball@896281c2d89c9347f83426d37ad9b36fc4742d5b`](https://github.com/snowballstem/snowball/tree/896281c2d89c9347f83426d37ad9b36fc4742d5b).
The compiler reports 2.0.0, but the 2.0.0 release tag is insufficient: it lacks Serbian.
For every registered algorithm, regenerating C at this commit produces exactly the
same C token sequence as DuckDB 1.5.6's bundled generated C++ source. Only layout and
comments are ignored; literal content and every operator remain part of the check.
`provenance.json` records source, generated-source, compiler and alias-table hashes.

To regenerate, obtain that exact source archive on the external artifact drive and
build its `snowball` executable separately (`make -j1 snowball`). This executable is
one-time development tooling, in the same category as the C++ behavioral oracle;
it is never a product dependency or linked into roastedDuck. Then run:

```sh
python3 scripts/snowball-generate.py --source /path/to/snowball --duckdb /path/to/duckdb-1.5.6
python3 scripts/snowball-generate.py --source /path/to/snowball --duckdb /path/to/duckdb-1.5.6 --check
```

The generated Rust code has deterministic import and byte-carrier substitutions.
The runtime ports the bundled C runtime's byte comparisons, UTF-8 traversal,
grouping, slices and among-table search. It does not normalize, lowercase, or trim
input; those operations belong to FTS tokenization.

The generated code and upstream runtime portions retain Snowball's BSD-3-Clause
license in `LICENSE`. Generation equivalence establishes provenance, not behavioral
acceptance. Run the differential harness against the pinned C++ FTS oracle before
claiming compatibility; preserve its per-language results and raw mismatches.

## Differential evidence

The signed development runner matched C++ DuckDB `v1.5.6` with FTS version
`6814ec9` on **10,819,519** inputs with **zero byte mismatches**. This covers the
complete pinned vocabulary for all 27 canonical algorithms, every registered
alias, `none`, and an embedded-NUL alias, with 22 boundary inputs per case.
The vocabulary source is
`snowballstem/snowball-data@a32eee945f8a71cda47a88f660910a59dacf6955`.

The receipt is `.context/artifacts/vss-fts/snowball/diff-full-1/verification.json`.
It records oracle, extension and runner hashes, per-case counts and input/output
hashes. The earlier 28,936-input diagnostic is retained separately. Successful
scratch outputs were removed after hashing; the vocabulary and receipts remain.
This establishes the measured stemming contract, not FTS indexing or database
interoperability acceptance.
