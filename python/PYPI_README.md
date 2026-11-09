# roastedDuck

**An analytical SQL engine, ported to Rust.**

roastedDuck is an independently maintained, MIT-licensed Rust implementation
targeting DuckDB 1.5.6 compatibility. It provides the familiar `duckdb` Python
API backed by the Rust engine. This is a public preview with explicit
[compatibility boundaries](https://amirbaron.github.io/roastedDuck/compatibility/).

[Documentation](https://amirbaron.github.io/roastedDuck/) ·
[Source](https://github.com/amirbaron/roastedDuck) ·
[Issues](https://github.com/amirbaron/roastedDuck/issues) ·
[Benchmark evidence](https://amirbaron.github.io/roastedDuck/benchmarks/)

## Install

Use a fresh virtual environment:

```sh
python3 -m venv .venv
. .venv/bin/activate
python -m pip install roastedduck
```

The **distribution is `roastedduck`**, while the compatibility **import is
`duckdb`**. The official `duckdb` distribution owns the same import namespace;
install the two engines in separate environments. Keep optional packages such
as NumPy, pandas, and PyArrow in the environment where you use them.

## Run a query

```python
import duckdb

with duckdb.connect() as connection:
    result = connection.execute(
        "SELECT category, sum(amount) AS total "
        "FROM (VALUES ('books', 20), ('coffee', 8), ('books', 15)) "
        "AS sales(category, amount) GROUP BY category ORDER BY total DESC"
    ).fetchall()
    assert result == [("books", 35), ("coffee", 8)]
```

Pass a database filename to `duckdb.connect("example.duckdb")` for persistence.
Use parameterized SQL for application values:

```python
with duckdb.connect("example.duckdb") as connection:
    connection.execute("CREATE TABLE IF NOT EXISTS readings(value INTEGER)")
    connection.execute("INSERT INTO readings VALUES (?)", [42])
    print(connection.execute("SELECT sum(value) FROM readings").fetchall())
```

## Versions and platforms

The compatibility target is **DuckDB 1.5.6**. The distribution version
`1.5.6.post2` identifies a roastedDuck packaging/security revision of that
target. `duckdb.__version__` and `duckdb.__duckdb_version__` retain the compatibility
version; `importlib.metadata.version("roastedduck")` identifies the installed
distribution, and `duckdb.version()` reports both.

Wheels target normal CPython **3.10–3.14**, on **macOS arm64**, **Linux arm64**,
and **Linux x86_64**. Linux wheels use the manylinux 2.28 baseline. Windows,
Intel Macs, PyPy, free-threaded Python, and newer Python versions are outside
this preview's qualified scope.

## Compatibility and safety

The recorded engine qualification includes differential SQL/client tests and
bidirectional persistence/WAL checks against the pinned reference. Known
failures, skips, shell differences, and performance outliers remain explicit
in the [compatibility guide](https://amirbaron.github.io/roastedDuck/compatibility/).
Use a copy of existing data when evaluating a different engine. SQL can access
files and configured network resources; it is not an isolation boundary for
untrusted code.

The security revisions update PyO3 to 0.30.1, rsa to 0.10.0, and
ctutils to 0.4.8, and has its own artifact and test provenance.
The `post2` revision uses roastedDuck names throughout the Rust workspace and
ships freshly qualified images. DuckDB-compatible Python imports and native APIs
remain stable.
Historical native benchmarks describe their recorded binaries
and workloads; they are not a guarantee for every Python query or later build.

## License and credits

MIT licensed, with retained upstream and third-party notices. The work builds
on DuckDB's algorithms, interfaces, tests, and community engineering. Native
cryptography dependencies may contain non-Rust code.

DuckDB is a trademark of the DuckDB Foundation. roastedDuck is not affiliated
with or endorsed by the Foundation.
