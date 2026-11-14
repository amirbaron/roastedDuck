# roastedDuck Python package

The current public preview is `roastedduck==1.5.6.post3`, targeting DuckDB 1.5.6.
The compatibility import remains `duckdb`; use a fresh environment and do not
co-install the official `duckdb` distribution.

Install from [PyPI](https://pypi.org/project/roastedduck/) with
`python -m pip install roastedduck`. The identical release files and checksums
are on [GitHub Releases](https://github.com/amirbaron/roastedDuck/releases).
See [PYPI_README.md](PYPI_README.md) for usage and scope.

Build from the repository root with the pinned Rust toolchain:

```sh
python -m pip install maturin==1.16.0
maturin build --locked --release --out /path/to/wheel-output
```

The canonical `pyproject.toml` is at the repository root. This lets source archives
include the exact patched dependencies under `third_party/`; source archives are
rebuilt outside the checkout before publication. Use cache/output storage
appropriate for large native builds.

Normal CPython 3.10–3.14 on macOS arm64 and Linux arm64/x86_64 is in scope.
New source-built security revisions need their own qualification. Historical
source2551 results do not automatically qualify newly compiled images.
