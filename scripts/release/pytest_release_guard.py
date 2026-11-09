"""Verify the selected installed engine inside the actual pytest process."""
import hashlib
import json
import os
from pathlib import Path


def pytest_runtest_logreport(report):
    path = os.environ.get("ROASTEDDUCK_PYTEST_EVENTS")
    if path:
        with Path(path).open("a") as stream:
            stream.write(json.dumps({
                "nodeid": report.nodeid, "when": report.when, "outcome": report.outcome,
                "wasxfail": getattr(report, "wasxfail", None),
            }) + "\n")


def pytest_sessionstart(session):
    import duckdb
    import _duckdb

    expected = os.environ["ROASTEDDUCK_TEST_ENGINE"]
    assert bool(getattr(_duckdb, "__rust_engine__", False)) == (expected == "rust")
    native = Path(_duckdb.__file__).resolve()
    digest = hashlib.sha256(native.read_bytes()).hexdigest()
    assert digest == os.environ["ROASTEDDUCK_EXPECTED_NATIVE_SHA256"]
    Path(os.environ["ROASTEDDUCK_PYTEST_IDENTITY"]).write_text(json.dumps({
        "engine": expected, "package": duckdb.__file__, "native": str(native), "native_sha256": digest,
    }, indent=2) + "\n")
