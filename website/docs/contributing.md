# Contributing

Small, reproducible reports are a good starting point. Include the release or
commit, operating system, client version, a minimal query, expected result,
and actual result. For comparisons, state which upstream version you tested.

- [Report a bug](https://github.com/amirbaron/roastedDuck/issues/new/choose)
- [Contribution guide](https://github.com/amirbaron/roastedDuck/blob/main/CONTRIBUTING.md)
- [Security policy](https://github.com/amirbaron/roastedDuck/blob/main/SECURITY.md)

## Useful areas for the next preview

Reproduce the install flow on a clean system, test real workloads, investigate
the documented performance outliers, or contribute physical iOS evidence.
Please keep results and limitations explicit; a successful build does not
establish runtime compatibility.

## Development workflow

`main` remains the integration/release branch. The existing
`amirbaron/rust-duckdb-port` development branch uses pull requests targeting
`main`. External contributors can open pull requests from topic branches in
their forks. Documentation deploys from `main`, without an extra site branch.

CI covers focused regression, source builds, documentation, and packaging
checks. The full historical SQL/client/performance qualification is larger
than the per-change CI suite. Required release checks must run on the artifact
being released, with provenance and exceptions retained.
