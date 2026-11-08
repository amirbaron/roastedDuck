# roastedDuck: an analytical SQL engine, ported to Rust

Today I'm opening roastedDuck, an independent Rust implementation targeting
DuckDB 1.5.6 compatibility. It is MIT licensed and available as a public preview.

It includes an in-process SQL engine, a command-line shell, Python, Go, Swift,
C API, and ADBC integration. The work builds on DuckDB's algorithms, interfaces,
tests, and years of community engineering. It is independently maintained and
is not an official DuckDB release.

The question behind the project is practical: how does a Rust implementation
behave when it aims to preserve familiar SQL and storage contracts while making
different implementation choices?

On the recorded macOS arm64 TPC-H SF10 and ClickBench configurations, the
candidate used 60–76% less geometric-mean whole-process peak RSS, with 7–16%
longer geometric-mean best query times. Those figures are not a universal
promise: ClickBench summed best times were 23–25% longer, its largest
single-thread peak was almost unchanged, and several stress cases regress.
The documentation publishes every query and the unfavorable cases alongside
the aggregate.

The candidate has extensive differential testing against the pinned reference,
including selected client suites and bidirectional persistence/WAL journeys.
Known exceptions remain explicit. Physical iOS execution is unverified, and
Windows and Intel Macs are outside this preview.

I would particularly value clean-system installation reports, representative
real-world workloads, and reproducible compatibility mismatches.

- Documentation: https://amirbaron.github.io/roastedDuck/
- Code and downloads: https://github.com/amirbaron/roastedDuck
- Evidence: https://amirbaron.github.io/roastedDuck/benchmarks/

DuckDB is a trademark of the DuckDB Foundation. roastedDuck is not affiliated
with or endorsed by the Foundation.

---

Maintainer note: this is prepared announcement copy, not evidence that an
announcement was sent. Review naming/promotional use against the Foundation's
published trademark guidelines before an external campaign. No third-party
outreach or endorsement is implied.
