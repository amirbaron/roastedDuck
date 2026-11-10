<p align="center"><img src="website/docs/assets/roastedduck.svg" width="96" height="96" alt="roastedDuck"></p>

# roastedDuck

**An analytical SQL engine, ported to Rust.**

roastedDuck is an independent Rust implementation targeting the DuckDB 1.5.6
SQL, storage, and selected client contracts. It is an in-process engine, with
a command-line shell, Python, Go, Swift, C API, and ADBC support. The engine is
Rust; some third-party cryptography dependencies contain native code.

**Public preview.** Compatibility has extensive differential test coverage,
with [explicit exceptions](https://amirbaron.github.io/roastedDuck/compatibility/).
This project is independently maintained and is not an official DuckDB release.

[Documentation](https://amirbaron.github.io/roastedDuck/) ·
[Downloads](https://github.com/amirbaron/roastedDuck/releases) ·
[Benchmarks](https://amirbaron.github.io/roastedDuck/benchmarks/) ·
[Contributing](CONTRIBUTING.md)

## Try it

Download the native archive for your platform from
[Releases](https://github.com/amirbaron/roastedDuck/releases), verify its SHA-256
against `SHA256SUMS`, extract it, and run:

```sh
./bin/roastedDuck -c "SELECT 'hello from roastedDuck' AS greeting, sum(i) FROM range(10) t(i);"
./bin/roastedDuck example.duckdb
```

Python wheels are distributed as **`roastedduck`**, while the compatibility
import remains **`duckdb`**. Use a separate virtual environment; the official
`duckdb` distribution and roastedDuck own the same import namespace.

```sh
python3 -m venv .venv
. .venv/bin/activate
python -m pip install roastedduck
python -c "import duckdb; print(duckdb.sql('SELECT 42').fetchall())"
```

See the [installation guide](https://amirbaron.github.io/roastedDuck/install/)
for exact platform names, verification, Go linkage, and Swift packages. Packages
are available through GitHub Releases and [PyPI](https://pypi.org/project/roastedduck/).
No crates.io publication is claimed.

## What is verified

The 1.5.6.post2 release has fresh native client, security, encryption, and
15-cell Python qualification. Its source archive is rebuilt outside the checkout,
and ordinary PyPI installs are checked on every advertised cell.

The separate November 6, 2026 software candidate records:

- All 4,870 SQL corpus files exercised on macOS arm64 and Linux arm64/x86_64,
  retaining 14 classified raw failures on each platform.
- 1,089 workspace test passes, plus C API and ADBC consumer coverage.
- 15 CPython 3.10–3.14 platform/ABI cells, and full selected Go/Swift suites.
- Bidirectional persistence/WAL checks and final relocated-artifact checks.

These are recorded candidate results, not a claim that every commit reruns every
suite. Historical Linux x86_64 full-corpus qualification used translation; current
package/client CI runs natively. iOS simulator testing and
device builds are recorded; **physical iOS execution is unverified**. Windows,
Intel Macs, and other clients are outside the preview's supported scope.

## The memory tradeoff

For the November 6, 2026 candidate, on macOS arm64 TPC-H SF10 and ClickBench workloads, at 1 and 14
threads, geometric-mean whole-process peak RSS was **60–76% lower**, with
geometric-mean best execution times **7–16% longer**, versus the pinned C++
reference. These are per-query geometric means, not maximum-memory guarantees.

ClickBench summed best times were **23–25% longer**. Its largest single-thread
peak was nearly unchanged, and a separate VARIANT stress case took **5.391×**
as long. Read the [methodology, every query, and exceptions](https://amirbaron.github.io/roastedDuck/benchmarks/)
before applying the averages to your workload.

## Build and contribute

```sh
git clone https://github.com/amirbaron/roastedDuck.git
cd roastedDuck
cargo build --locked --release --bin roastedDuck -j 1
./target/release/roastedDuck -c 'SELECT 42;'
```

The repository pins Rust 1.99.0. The CLI package is `roastedduck-cli` and its
executable is `roastedDuck`. Rust crates and modules use `roastedduck-*` and
`roastedduck_*` names; DuckDB-compatible native APIs and Python imports remain stable.
Public release versions follow the targeted DuckDB version. Preview status
is recorded separately from version compatibility. See [CONTRIBUTING.md](CONTRIBUTING.md)
for prerequisites, tests, and the existing branch workflow.

## License and acknowledgements

[MIT](LICENSE), with upstream and third-party notices retained in
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md). This work builds on the algorithms,
interfaces, tests, and extensive engineering of the DuckDB community. DuckDB is
a trademark of the DuckDB Foundation. This project is not affiliated with or
endorsed by the Foundation.
