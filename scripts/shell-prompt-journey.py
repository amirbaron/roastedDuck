"""Raw terminal prompt, prompt SQL, continuation and byte-truncation journeys."""

import argparse
import base64
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil


spec = importlib.util.spec_from_file_location("prompt_pty", Path(__file__).with_name("shell-diff.py"))
runner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner)


def quoted(value):
    return b'"' + value.replace(b"\\", b"\\\\").replace(b'"', b'\\"') + b'"'


def cases():
    for name, value in [("empty", b""), ("ascii", b"alpha"), ("utf8", "\u96ea".encode()),
                        ("latin1", b"caf\xe9"), ("invalid", b"\xff"), ("incomplete", b"\xc3")]:
        for family in ["literal", "sql", "continuation", "continuation-truncate"]:
            command = b".prompt "
            if family == "literal":
                command += quoted(value + b"> ")
            elif family == "sql":
                sql = b"SELECT '" + value.replace(b"'", b"''") + b"'"
                command += quoted(b"{sql:" + sql + b"}> ")
            else:
                if family == "continuation-truncate":
                    value_for_prompt = b"123456789012345678" + value
                else:
                    value_for_prompt = value
                command += quoted(b"MAIN> ") + b" " + b" ".join([quoted(value_for_prompt)] * 4)
            keys = ["SELECT 42 AS answer;", "\r", "\x04"]
            if family.startswith("continuation"):
                keys = ["SELECT", "\r", "1 +", "\r", "2 +", "\r", "3 +", "\r", "4",
                        "\x1b[A", "\x1b[A", "\x1b[B", "\x1b[B", ";", "\r",
                        "SELECT 42 AS answer;", "\r", "\x04"]
            yield f"{name}/{family}", value, command, keys
    # The command argument transports ASCII escape syntax; its parser creates the NUL.
    for family, command in [
        ("literal", b'.prompt "a\\000b> "'),
        ("continuation", b'.prompt "MAIN> " "a\\000b" "a\\000b" "a\\000b" "a\\000b"'),
    ]:
        yield f"escaped-nul/{family}", b"a\0b", command, ["SELECT", "\r", "42;", "\r", "\x04"]
    for name, value, command in [
        ("sql-nul", b"\0tail", b'.prompt "{sql:SELECT chr(0)||\'tail\'}> "'),
        ("sql-nul-fallback", b"\0tail", b'.prompt "{sql:SELECT chr(0)||\'tail\'}> "'),
        ("max-width-utf8", "\u96ea".encode(), b".prompt " + quoted("{max_length:1}\u96ea> ".encode())),
        ("max-width-incomplete", b"\xc3", b".prompt " + quoted(b"{max_length:1}\xc3> ")),
    ]:
        yield f"supplement/{name}", value, command, ["SELECT 42 AS answer;", "\r", "\x04"]


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
    parser.add_argument("-k", default="")
    args = parser.parse_args()
    root, output = args.work.resolve(), args.out.resolve()
    if root.exists() or output.exists():
        parser.error("use new --work/--out paths to preserve previous evidence")
    selected = [c for c in cases() if args.k in c[0]]
    if not selected:
        parser.error("no cases selected")
    root.mkdir(parents=True)
    images = {k: v.resolve() for k, v in (("reference", args.reference), ("rust", args.binary))}
    (root / "manifest.json").write_text(json.dumps({
        "images": {k: {"path": str(v), "sha256": hashlib.sha256(v.read_bytes()).hexdigest()}
                   for k, v in images.items()}, "normalization": "none", "cases": len(selected),
        "scope": "actual terminal prompts and query reuse; not whole shell completion"}, indent=2) + "\n")
    rows = []
    with output.open("x") as evidence:
        for name, value, command, keys in selected:
            assert b"\0" not in command
            case = {"name": name, "pty": True, "rows": 4, "cols": 60,
                    "args": ["-dark-mode", "-cmd", ".startup_text none", "-cmd", ".mode list",
                             "-cmd", os.fsdecode(command)], "keys": keys}
            if name.endswith("fallback"):
                case["args"] += ["-cmd", ".read_line_version fallback"]
            archive = root / f"case-{len(rows):03d}"
            archive.mkdir()
            results = {}
            for engine, image in images.items():
                result = runner.run_pty(str(image), str(root / "working"), case)
                shutil.move(root / "working", archive / engine)
                results[engine] = result
            completed = all(isinstance(r["exit"], int) and r["exit"] >= 0 for r in results.values())
            verdict = "match" if completed and results["reference"] == results["rust"] else "mismatch"
            row = {"case": name, "payload_hex": value.hex(), "command_hex": command.hex(),
                   "verdict": verdict,
                   "query_output_visible": {k: b"\r\n42\r\n" in r["stdout"] for k, r in results.items()},
                   **{k: encoded(r) for k, r in results.items()}}
            evidence.write(json.dumps(row) + "\n")
            evidence.flush()
            rows.append(row)
            if name.startswith(("ascii/", "utf8/")) and results["reference"]["exit"] != 0:
                raise SystemExit("Valid reference terminal control failed; inspect raw evidence")
    summary = {"cases": len(rows), "matches": sum(r["verdict"] == "match" for r in rows),
               "mismatches": [r["case"] for r in rows if r["verdict"] != "match"],
               "normalization": "none"}
    (root / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps(summary))
    raise SystemExit(bool(summary["mismatches"]))


if __name__ == "__main__":
    main()
