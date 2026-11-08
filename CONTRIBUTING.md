# Contributing to roastedDuck

Welcome. Reproducible bug reports, portability work, benchmarks, and focused
improvements are useful contributions. Start with the
[architecture](docs/architecture.md) and [compatibility boundaries](website/docs/compatibility.md).

## Branches

`main` is the integration and release branch. The existing
`amirbaron/rust-duckdb-port` branch continues to carry development work through
pull requests targeting `main`. Contributors can use topic branches in their
forks. Documentation deploys from `main` through GitHub Actions; there is no
separate documentation branch. Releases use annotated `v*` tags.

## Local development

Install the toolchain pinned in `rust-toolchain.toml` using rustup, a C compiler,
and platform development headers. Linux native dependencies may require
`pkg-config`, `perl`, and OpenSSL development headers. Python bindings also need
a native supported Python and the pinned maturin from `python/pyproject.toml`.

```sh
cargo build --locked -p rustdb-shell -j 1
cargo test --locked -p rustdb-types -p rustdb-common -j 1
./target/debug/rustdb -c 'SELECT sum(i) FROM range(10) t(i);'
```

Keep builds and full corpus jobs serialized on resource-constrained machines.
Use `CARGO_TARGET_DIR`, `TMPDIR`, and client cache/output settings to choose
storage appropriate for large outputs. No private signing identity is required
to build the source.

For differential testing, provide the pinned upstream source via `REF` for
`scripts/conformance.sh` or `DUCKDB_SRC` for the performance tools. Historical
paths under `.context/` are local evidence locations, not downloaded dependencies.
See [benchmark reproduction](website/docs/benchmarks.md).

## Changes and review

- Keep changes focused and describe the behavior before and after.
- Add a regression test when fixing an engine correctness issue.
- Preserve exact-output fixtures and keep failures, skips, and reference crashes explicit.
- Report which tests actually ran. A build is not runtime verification.
- Avoid opportunistic formatting of copied upstream files and byte fixtures.
- Never silently fall back to the upstream C++ engine in client tests.

## Documentation

```sh
python3 -m venv .venv-docs
. .venv-docs/bin/activate
pip install -r website/requirements.txt
mkdocs serve -f mkdocs.yml
```

The curated user documentation lives in `website/docs/`. The existing `docs/`
directory retains dated engineering reports. Put current user instructions in
the site and link historical evidence with its date and scope.

By submitting contributions, you agree that your contributions are licensed
under the project's MIT license. Preserve authorship and third-party notices.
