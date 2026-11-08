# roastedDuck Python package

The public distribution is `roastedduck`, version `0.1.0a1`. The compatibility
import is `duckdb`; use a fresh virtual environment and do not co-install the
official `duckdb` distribution. The package uses the Rust engine directly.

- [Installation and usage](https://amirbaron.github.io/roastedDuck/python/)
- [Preview compatibility](https://amirbaron.github.io/roastedDuck/compatibility/)
- [Release wheels](https://github.com/amirbaron/roastedDuck/releases)
- [Recorded software candidate](../docs/release-candidate-index-20261106.md)

Normal CPython 3.10–3.14 on macOS arm64 and Linux arm64/x86_64 is in scope.
The recorded candidate qualified 15 ABI/platform cells, retaining matching
skips and expected failures. Public wheel metadata/version branding has
separate installation checks; the native extension bytes are preserved.

Build with the pinned Rust toolchain and `maturin==1.9.6`:

```sh
python -m pip install maturin==1.9.6
maturin build --release --out /path/to/wheel-output
```

Choose cache/output storage appropriate for large builds. The upstream Python
helper code retains its MIT license in [LICENSE-UPSTREAM](LICENSE-UPSTREAM).
The historical implementation chronology is retained in
[python-bindings.md](../docs/python-bindings.md); dated open items there are
not the current preview status.
