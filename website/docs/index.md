---
template: home.html
hide:
  - toc
  - navigation
---

## Familiar interfaces. An independent implementation.

Run analytical SQL over local data, embed the engine in an application, or use
the selected Python, Go, and Swift clients. roastedDuck ports the engine to Rust
while targeting the SQL and storage behavior of **DuckDB 1.5.6**.

This is a **public preview**. The compatibility target has extensive recorded
differential testing, with known exceptions. It is independently maintained;
it is not an official DuckDB release or an endorsed replacement.

## Start with your workload

| Your starting point | Where to go |
| --- | --- |
| Explore a file or run SQL | [Install the shell](install.md), then [run a query](quickstart.md) |
| Work in a notebook or application | [Python](python.md) |
| Embed in a service | [Go](go.md) or [C API / ADBC](capi.md) |
| Build an Apple application | [Swift](swift.md) |
| Evaluate the tradeoffs | [Benchmarks](benchmarks.md) and [compatibility](compatibility.md) |

## Less memory on the measured workloads

The recorded November 6, 2026 macOS arm64 TPC-H SF10 and ClickBench configurations show
**60–76% lower geometric-mean whole-process peak RSS**, with **7–16% longer
geometric-mean best query times**, at 1 and 14 threads. Individual workloads
vary substantially: ClickBench summed times are 23–25% longer, and its largest
single-thread peak is almost unchanged.

The [benchmark explorer](benchmarks.md) puts the aggregates, every query,
and the exceptions together. No native Linux performance claim is made.

## Built on a substantial foundation

The work builds on DuckDB's algorithms, interfaces, tests, and community.
The Rust engine, shell, selected clients, and public evidence are available
under [MIT and retained third-party licenses](license.md).

The [architecture guide](architecture.md) explains the crate boundaries.
The [contributor guide](contributing.md) explains how to report a mismatch,
run focused tests, and improve the next release.
