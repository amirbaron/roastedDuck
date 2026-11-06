"""Real progress component frames, cancellation and same-connection reuse.

Preserves full raw streams. Frame counts/timing can differ; this is deliberately
not a raw terminal-stream parity gate.
"""

import argparse
import base64
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil


spec = importlib.util.spec_from_file_location("progress_pty", Path(__file__).with_name("shell-diff.py"))
runner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner)


def quote(value):
    return b'"' + value.replace(b"\\", b"\\\\").replace(b'"', b'\\"') + b'"'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--reference", required=True, type=Path)
    parser.add_argument("--work", required=True, type=Path)
    parser.add_argument("--out", required=True, type=Path)
    args = parser.parse_args()
    root, output = args.work.resolve(), args.out.resolve()
    if root.exists() or output.exists():
        parser.error("use new --work/--out paths to preserve previous evidence")
    root.mkdir(parents=True)
    images = {k: v.resolve() for k, v in (("reference", args.reference), ("rust", args.binary))}
    (root / "manifest.json").write_text(json.dumps({
        "images": {k: {"path": str(v), "sha256": hashlib.sha256(v.read_bytes()).hexdigest()}
                   for k, v in images.items()}, "cases": 6, "normalization": "none",
        "scope": "fixed progress frame bytes and cancellation/reuse; not raw stream equality"}, indent=2) + "\n")
    rows = []
    with output.open("x") as evidence:
        # Both payload widths are covered by frozen native Utf8Proc/terminal controls.
        for label, value, value_width in [("ascii", b"alpha", 5), ("utf8", "\u96ea".encode(), 2)]:
            for family in ["literal", "sql", "hidden-padding"]:
                token = b"NATIVE[" + value + b"]"
                component = token
                if family == "sql":
                    component = b"NATIVE[{sql:SELECT '" + value + b"'}]"
                if family == "hidden-padding":
                    component = b"{min_size:30}{content_align:middle}" + token
                settings = [b".startup_text none", b".mode list", b".highlight off",
                            b"CREATE TABLE progress_input AS SELECT i FROM range(100000)t(i);",
                            b"SET enable_progress_bar=true; SET enable_progress_bar_print=true; SET progress_bar_time=0;",
                            b".progress_bar --clear"]
                if family == "hidden-padding":
                    settings.append(b".progress_bar --add " + quote(b"{hide_if_contains:" + value + b"}HIDDEN[" + value + b"]"))
                settings.append(b".progress_bar --add " + quote(component))
                arguments = ["-dark-mode"]
                for setting in settings:
                    arguments += ["-cmd", os.fsdecode(setting)]
                case = {"name": f"{label}/{family}", "pty": True, "cols": 60, "rows": 20,
                        "args": arguments,
                        "keys": ["SELECT count(*) FROM progress_input a, progress_input b WHERE (a.i + b.i) % 7 = 3;",
                                 "\r", {"wait_for": "NATIVE[", "timeout": 5.0}, "\x03", 0.5,
                                 "SELECT 42 AS answer;", "\r", "\x04"]}
                archive = root / f"case-{len(rows):03d}"
                archive.mkdir()
                results = {}
                for engine, image in images.items():
                    result = runner.run_pty(str(image), str(root / "working"), case)
                    shutil.move(root / "working", archive / engine)
                    frame_bytes = 60 + len(value) - value_width
                    frames = [chunk[:frame_bytes]
                              for chunk in result["stdout"].split(b"\r\x1b[0K")
                              if token in chunk[:frame_bytes]]
                    results[engine] = {
                        "exit": result["exit"], "frames_base64": [base64.b64encode(x).decode() for x in frames],
                        "wait_timeouts": result.get("wait_timeouts", []),
                        "frame_count": len(frames), "frame_has_hidden_text": any(b"HIDDEN[" in x for x in frames),
                        "reuse_query_output": b"\r\n42\r\n" in result["stdout"],
                        "stdout_base64": base64.b64encode(result["stdout"]).decode(),
                        "stderr_base64": base64.b64encode(result["stderr"]).decode(),
                        "files_base64": {k: base64.b64encode(v).decode() for k, v in result["files"].items()},
                    }
                a, b = results["reference"], results["rust"]
                same_frames = bool(a["frame_count"] and b["frame_count"]) and set(a["frames_base64"]) == set(b["frames_base64"])
                verified = same_frames and all(r["exit"] == 1 and r["reuse_query_output"] and not r["frame_has_hidden_text"] and not r["wait_timeouts"] for r in results.values())
                row = {"case": case["name"], "component_hex": component.hex(),
                       "fixed_frames_match": same_frames, "journey_verified": verified,
                       "raw_streams_equal": a["stdout_base64"] == b["stdout_base64"] and a["stderr_base64"] == b["stderr_base64"],
                       **results}
                evidence.write(json.dumps(row) + "\n")
                evidence.flush()
                rows.append(row)
                if a["exit"] != 1 or not a["frame_count"] or not a["reuse_query_output"] or a["wait_timeouts"]:
                    raise SystemExit("Reference progress control failed; inspect raw evidence")
    summary = {"cases": len(rows), "fixed_frame_and_reuse_verified": sum(r["journey_verified"] for r in rows),
               "unverified": [r["case"] for r in rows if not r["journey_verified"]],
               "scope": "fixed frames and reuse only; full streams and counts retained without normalization"}
    (root / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps(summary))
    raise SystemExit(bool(summary["unverified"]))


if __name__ == "__main__":
    main()
