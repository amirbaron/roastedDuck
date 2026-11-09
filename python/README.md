# roastedDuck Python package

The current source candidate is `roastedduck==1.5.6.post1`, targeting DuckDB 1.5.6.
The compatibility import remains `duckdb`; use a fresh environment and do not
co-install the official `duckdb` distribution.

The first PyPI publication is still being qualified. Until publication is verified,
use the currently published [GitHub releases](https://github.com/amirbaron/roastedDuck/releases).
See [PYPI_README.md](PYPI_README.md) for the candidate's package documentation.

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
