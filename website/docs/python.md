# Python

The `roastedduck` wheel provides the familiar `duckdb` import using the Rust
engine. [Install from PyPI](install.md#python) in a fresh virtual environment
with `python -m pip install roastedduck`.

```python
import duckdb

with duckdb.connect("example.duckdb") as connection:
    connection.execute("CREATE TABLE IF NOT EXISTS readings(value INTEGER)")
    connection.execute("INSERT INTO readings VALUES (?)", [42])
    print(connection.execute("SELECT sum(value) FROM readings").fetchall())
```

## Package and engine versions

```python
from importlib.metadata import version
import duckdb

print(version("roastedduck"))  # 1.5.6.post2: public Python distribution
print(duckdb.__duckdb_version__)  # separately reported compatibility engine version
```

`duckdb.__version__` and `duckdb.__duckdb_version__` retain `1.5.6` for
version-gated client compatibility. Use `version("roastedduck")` for the public
distribution version; `duckdb.version()` reports both. The package
metadata distinguishes security/packaging revisions from the compatibility
target. Version 1.5.6.post2 is rebuilt from the fixed source and dependency graph.
Release manifests bind the tested wheel bytes and their installed native images.

## Optional integrations

Install pandas, NumPy, or PyArrow separately when your application needs them.
The preview's recorded upstream testing covers selected pinned dependency
environments; it does not establish compatibility with every future dependency
release. The installed ADBC facade is also included.

## Compatibility boundaries

Normal CPython 3.10–3.14 on macOS arm64 and Linux arm64/x86_64 is in scope.
All 15 release wheels run the pinned upstream fast suite against the matching
C++ control, preserving matching skips and expected failures. A separate hosted
matrix verifies ordinary PyPI installation, native identity, SQL, and encryption. See [compatibility](compatibility.md).

## Build a wheel

```sh
python -m pip install maturin==1.16.0
# Run from the repository root.
maturin build --locked --release --out /path/to/wheel-output
```

Choose an external output/cache directory when disk space is limited. The build
requires the pinned Rust toolchain and the native interpreter for the desired ABI.
