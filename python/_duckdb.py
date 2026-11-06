"""Expose the native Rust module under DuckDB's conventional top-level import."""
import sys

from duckdb import _duckdb as _native

sys.modules[__name__] = _native
