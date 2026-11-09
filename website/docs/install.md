# Installation

Download from [GitHub Releases](https://github.com/amirbaron/roastedDuck/releases).
The current preview is `v1.5.6.post1`, targeting DuckDB 1.5.6. Each release has a `SHA256SUMS` manifest,
native SDKs, Python wheels, Swift packages, and a provenance report.

## Native shell and SDK

| Machine | Archive |
| --- | --- |
| Apple Silicon Mac | `roastedduck-1.5.6.post1-macos-arm64.tar.gz` |
| Linux ARM64 | `roastedduck-1.5.6.post1-linux-arm64.tar.gz` |
| Linux x86_64 | `roastedduck-1.5.6.post1-linux-amd64.tar.gz` |

Download the appropriate archive and `SHA256SUMS` into the same directory.
Check the archive's digest against its line in the manifest:

```sh
# macOS; replace the filename on Linux and use sha256sum instead.
shasum -a 256 roastedduck-1.5.6.post1-macos-arm64.tar.gz
grep 'roastedduck-1.5.6.post1-macos-arm64.tar.gz' SHA256SUMS
mkdir roastedduck
tar -xzf roastedduck-1.5.6.post1-macos-arm64.tar.gz -C roastedduck
cd roastedduck
./bin/roastedDuck -c 'SELECT 42 AS answer;'
```

The two digests must match before extraction or execution. Checksums establish
that the files match the release manifest; obtain both from the official project
release. The preview's historical signatures are development signatures, not
Apple Developer ID notarization. If macOS blocks a downloaded binary, use the
normal **System Settings → Privacy & Security → Open Anyway** flow after
verifying its source. Do not disable Gatekeeper globally.

The archive contains `bin/rustdb` and its `bin/roastedDuck` alias, dynamic
and static libraries, C/ADBC headers, the pinned Go source, and license notices. Engine and ABI identifiers intentionally
remain compatible; branding does not change SQL result bytes.

## Python

Install from [PyPI](https://pypi.org/project/roastedduck/) using normal CPython
3.10–3.14 on Apple Silicon or Linux arm64/x86_64. pip selects the matching wheel:

```sh
python3.12 -m venv .venv
. .venv/bin/activate
python -m pip install roastedduck
python -c "import duckdb; print(duckdb.sql('SELECT 42').fetchall())"
```

Linux wheels use `manylinux_2_28_aarch64` or `manylinux_2_28_x86_64` tags;
they require a compatible glibc environment. Free-threaded Python, PyPy, Windows,
and Intel Mac wheels are outside this preview.

The distribution name is `roastedduck`; the import name is `duckdb`. Install
in a fresh virtual environment and do not co-install the official `duckdb`
package. To pin this release, use `python -m pip install roastedduck==1.5.6.post1`.
The same wheels and the tested source archive are also available in GitHub Releases.

## Build from source

Install rustup, the pinned Rust 1.99.0 toolchain, and platform development tools.
On Debian-family Linux, install a C compiler, `pkg-config`, `perl`, and
`libssl-dev` first. Native cryptography dependencies may invoke a C compiler.

```sh
git clone https://github.com/amirbaron/roastedDuck.git
cd roastedDuck
cargo build --locked --release -p rustdb-shell -j 1
./target/release/rustdb -c 'SELECT 42;'
```

Client-specific instructions: [Python](python.md), [Go](go.md), [Swift](swift.md),
[C API and ADBC](capi.md). See [compatibility](compatibility.md) before using
the preview with important databases.
