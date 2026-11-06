"""Real PTY pager launch, native command/environment bytes and stdout recovery."""

import argparse
import base64
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil


spec = importlib.util.spec_from_file_location("pager_pty", Path(__file__).with_name("shell-diff.py"))
pty_runner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(pty_runner)


def encoded(result):
    return {"exit": result["exit"],
            "stdout_base64": base64.b64encode(result["stdout"]).decode(),
            "stderr_base64": base64.b64encode(result["stderr"]).decode(),
            "files_base64": {k: base64.b64encode(v).decode() for k, v in result["files"].items()}}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--reference", required=True, type=Path)
    parser.add_argument("--work", required=True, type=Path)
    parser.add_argument("--out", required=True, type=Path)
    args = parser.parse_args()
    root, output = args.work.resolve(), args.out.resolve()
    if root.exists() or output.exists():
        parser.error("use new --work and --out paths to preserve prior evidence")
    root.mkdir(parents=True)
    images = {k: v.resolve() for k, v in (("reference", args.reference), ("rust", args.binary))}
    (root / "manifest.json").write_text(json.dumps({
        "images": {k: {"path": str(v), "sha256": hashlib.sha256(v.read_bytes()).hexdigest()}
                   for k, v in images.items()}, "normalization": "none", "cases": 12,
        "scope": "real native pager launch, marker bytes and return to stdout"}, indent=2) + "\n")
    rows = []
    with output.open("x") as evidence:
        for label, value in [("ascii", b"alpha"), ("utf8", "caf\u00e9".encode()),
                             ("invalid", b"\xff"), ("incomplete", b"\xc3")]:
            command = b"printf '%s' '" + value + b"' > pager.marker; cat"
            for source in ("command", "DUCKDB_PAGER", "PAGER"):
                env = {"LESS": "-F", "PAGER": "cat"}
                initial = b".pager on"
                if source == "command":
                    quoted = b'"' + command.replace(b"\\", b"\\\\").replace(b'"', b'\\"') + b'"'
                    initial = b".pager " + quoted
                else:
                    env[source] = os.fsdecode(command)
                case = {"name": f"{label}/{source}", "pty": True,
                        "args": ["-dark-mode", "-cmd", ".startup_text none", "-cmd", ".mode list",
                                 "-cmd", os.fsdecode(initial), "-cmd", ".pager on"],
                        "env": env,
                        "keys": ["SELECT 42 AS answer;", "\r", ".pager off", "\r",
                                 "SELECT 43 AS after;", "\r", "\x04"]}
                archive = root / f"case-{len(rows):03d}"
                archive.mkdir()
                results = {}
                for engine, image in images.items():
                    result = pty_runner.run_pty(str(image), str(root / "working"), case)
                    shutil.move(root / "working", archive / engine)
                    results[engine] = result
                marker_ok = all(r["files"].get("pager.marker") == value for r in results.values())
                success = all(r["exit"] == 0 for r in results.values())
                query_output = all(b"\r\n42\r\n" in r["stdout"] and b"\r\n43\r\n" in r["stdout"]
                                   for r in results.values())
                # Exact terminal output includes the pager query and subsequent stdout query.
                verdict = "match" if success and marker_ok and query_output and results["reference"] == results["rust"] else "mismatch"
                row = {"case": case["name"], "value_hex": value.hex(), "command_hex": command.hex(),
                       "marker_bytes_match_both": marker_ok, "verdict": verdict,
                       "pager_and_stdout_query_output_visible_both": query_output,
                       **{k: encoded(v) for k, v in results.items()}}
                evidence.write(json.dumps(row) + "\n")
                evidence.flush()
                rows.append(row)
                reference = results["reference"]
                if reference["exit"] != 0 or reference["files"].get("pager.marker") != value:
                    (root / "summary.json").write_text(json.dumps({
                        "status": "reference control failed; investigate harness before parity claims",
                        "case": case["name"], "records": len(rows)}, indent=2) + "\n")
                    raise SystemExit("Reference pager control failed; raw evidence preserved")
    summary = {"cases": len(rows), "matches": sum(r["verdict"] == "match" for r in rows),
               "mismatches": [r["case"] for r in rows if r["verdict"] != "match"],
               "normalization": "none"}
    (root / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps(summary))
    raise SystemExit(bool(summary["mismatches"]))


if __name__ == "__main__":
    main()
