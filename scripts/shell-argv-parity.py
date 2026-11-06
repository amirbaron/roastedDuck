"""Compare actual native argv, output bytes and file effects against frozen DuckDB.

Run serially after other engine workloads. Both --work and --out must be new paths;
use the workspace's external artifact directory. No expected result is synthesized.
Database bytes are retained but their content is not a shell-parity assertion.
"""

import argparse
import base64
from collections import Counter
from dataclasses import dataclass
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import time


@dataclass(frozen=True)
class Case:
    name: str
    args: tuple[bytes, ...]
    stdin: bytes = b""
    files: tuple[tuple[bytes, bytes], ...] = ()
    argv0: bytes = b"duckdb"


def quoted(value, quote):
    return quote + value.replace(quote, quote + quote) + quote


def meta_quoted(value):
    return b'"' + value.replace(b"\\", b"\\\\").replace(b'"', b'\\"') + b'"'


def cases(exhaustive):
    samples = [
        ("empty", b""), ("ascii", b"alpha"), ("utf8", "caf\u00e9".encode()),
        ("astral", "duck\U0001f986".encode()), ("combining", "e\u0301".encode()),
        ("latin1", b"caf\xe9"), ("invalid", b"\xff"), ("incomplete", b"\xc3"),
        ("continuation", b"\x80"), ("overlong", b"\xc0\xaf"),
    ]
    if exhaustive:
        samples += [(f"byte-{i:02x}", b"b" + bytes([i])) for i in range(1, 256)]
    for label, value in samples:
        commands = {
            "literal": b"SELECT " + quoted(value, b"'") + b" AS value;",
            "alias": b"SELECT 42 AS " + quoted(value, b'"') + b";",
            "print": b".print " + meta_quoted(value),
        }
        for transport in (b"-c", b"-s", b"-cmd", b"positional"):
            for kind, sql in commands.items():
                args = (b":memory:", sql) if transport == b"positional" else (transport, sql)
                yield Case(f"{label}/{transport.decode()}/{kind}", args,
                           stdin=b"SELECT 43 AS after_stdin;\n")
        for mode in (b"list", b"csv", b"box", b"duckbox", b"table", b"column", b"line",
                     b"html", b"json", b"markdown", b"quote"):
            yield Case(f"{label}/null/{mode.decode()}",
                       (b"-nullvalue", value, b"-c", b".mode " + mode,
                        b"-c", b"SELECT NULL AS value;"))
        yield Case(f"{label}/separator", (b"-list", b"-separator", value,
                   b"-c", b"SELECT 1 AS a, 2 AS b;"))
        yield Case(f"{label}/newline", (b"-list", b"-newline", value,
                   b"-c", b"SELECT * FROM (VALUES (1),(2)) t(a);"))
        yield Case(f"{label}/storage-version", (b"-storage-version", value))
        yield Case(f"{label}/unknown-option", (b"-unknown-" + value,))
        yield Case(f"{label}/program-name", (b"-help",), argv0=b"duckdb-" + value)
        yield Case(f"{label}/database-path", (b"native-" + value + b".db", b"SELECT 42;"))
        filename = b"native-" + value + b".sql"
        fixture = ((filename, b"SELECT 42 AS from_file;\n"),)
        yield Case(f"{label}/init-path", (b"-init", filename, b"-c", b"SELECT 43;"), files=fixture)
        yield Case(f"{label}/input-path", (b"-f", filename), files=fixture)
        missing = b"missing-" + value + b".sql"
        yield Case(f"{label}/missing-init", (b"-init", missing, b"-c", b"SELECT 43;"))
        yield Case(f"{label}/missing-input", (b"-f", missing))


def b64(value):
    return base64.b64encode(value).decode("ascii")


def file_effects(work):
    records = []
    prefix = os.fsencode(work)
    for directory, directories, filenames in os.walk(prefix, followlinks=False):
        for name in sorted(directories + filenames):
            path = os.path.join(directory, name)
            relative = os.path.relpath(path, prefix)
            if os.path.islink(path):
                records.append({"path_hex": relative.hex(), "kind": "symlink",
                                "target_hex": os.readlink(path).hex()})
            elif os.path.isfile(path):
                data = Path(os.fsdecode(path)).read_bytes()
                opaque = relative.endswith((b".db", b".duckdb", b".wal", b".parquet"))
                records.append({"path_hex": relative.hex(), "kind": "file", "opaque": opaque,
                                "sha256": hashlib.sha256(data).hexdigest(), "bytes": len(data),
                                **({} if opaque else {"base64": b64(data)})})
            elif os.path.isdir(path):
                records.append({"path_hex": relative.hex(), "kind": "directory"})
    return sorted(records, key=lambda row: row["path_hex"])


def comparison(result):
    effects = []
    for row in result.get("files", []):
        if row.get("opaque"):
            row = {k: v for k, v in row.items() if k not in ("sha256", "bytes")}
        effects.append(row)
    return result.get("exit"), result.get("stdout_base64"), result.get("stderr_base64"), effects


def run(binary, case, work, archived, timeout):
    work.mkdir()
    (work / "home").mkdir()
    (work / "tmp").mkdir()
    try:
        for filename, content in case.files:
            path = os.path.join(os.fsencode(work), filename)
            os.makedirs(os.path.dirname(path), exist_ok=True)
            with open(path, "wb") as handle:
                handle.write(content)
    except OSError as error:
        shutil.move(work, archived)
        return {"fixture_error": {"type": type(error).__name__, "errno": error.errno,
                                  "message": str(error)}}
    env = {b"PATH": b"/usr/bin:/bin:/usr/sbin:/sbin", b"LANG": b"en_US.UTF-8",
           b"HOME": os.fsencode(work / "home"), b"TMPDIR": os.fsencode(work / "tmp")}
    start = time.monotonic()
    try:
        process = subprocess.run([case.argv0, *case.args], executable=os.fsencode(binary),
                                 input=case.stdin, capture_output=True, cwd=work, env=env,
                                 timeout=timeout, check=False)
        result = {"exit": process.returncode, "stdout_base64": b64(process.stdout),
                  "stderr_base64": b64(process.stderr)}
    except subprocess.TimeoutExpired as error:
        result = {"timeout": True, "stdout_base64": b64(error.stdout or b""),
                  "stderr_base64": b64(error.stderr or b"")}
    result.update(elapsed_ms=round((time.monotonic() - start) * 1000), files=file_effects(work))
    shutil.move(work, archived)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--reference", required=True, type=Path)
    parser.add_argument("--work", required=True, type=Path)
    parser.add_argument("--out", required=True, type=Path)
    parser.add_argument("--timeout", type=float, default=10)
    parser.add_argument("--exhaustive", action="store_true")
    parser.add_argument("-k", default="", help="case-name substring")
    args = parser.parse_args()
    binary, reference, root, output = (p.resolve() for p in
                                       (args.binary, args.reference, args.work, args.out))
    if output.exists():
        parser.error("--out already exists; preserve previous evidence")
    root.mkdir(exist_ok=False, parents=True)
    output.parent.mkdir(exist_ok=True, parents=True)
    selected = [case for case in cases(args.exhaustive) if args.k in case.name]
    if not selected:
        parser.error("no cases selected")
    manifest = {"binary": str(binary), "reference": str(reference), "cases": len(selected),
                "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
                "reference_sha256": hashlib.sha256(reference.read_bytes()).hexdigest(),
                "comparison": "exact bytes/exit/file names; database file content excluded only",
                "normalization": "none", "argv_nul": "not transportable on POSIX; not generated"}
    (root / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    verdicts = Counter()
    with output.open("x") as evidence:
        for index, case in enumerate(selected):
            assert all(b"\0" not in value for value in (case.argv0, *case.args))
            archive = root / f"case-{index:05d}"
            archive.mkdir()
            results = {label: run(image, case, root / "working", archive / label, args.timeout)
                       for label, image in (("reference", reference), ("rust", binary))}
            a, b = results["reference"], results["rust"]
            if any("fixture_error" in item for item in results.values()):
                verdict = "fixture-error"
            elif any(item.get("timeout") for item in results.values()):
                verdict = "timeout"
            elif any(item["exit"] < 0 for item in results.values()):
                verdict = "crash-held-open"
            else:
                verdict = "match" if comparison(a) == comparison(b) else "mismatch"
            verdicts[verdict] += 1
            evidence.write(json.dumps({"case": case.name, "argv0_hex": case.argv0.hex(),
                                       "args_hex": [value.hex() for value in case.args],
                                       "stdin_base64": b64(case.stdin), "verdict": verdict,
                                       **results}) + "\n")
            evidence.flush()
    summary = {"cases": len(selected), "verdicts": dict(verdicts), "manifest": str(root / "manifest.json"),
               "raw_evidence": str(output), "scope": "native argv only; not full shell/storage acceptance"}
    (root / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps(summary))
    raise SystemExit(0 if verdicts.get("match", 0) == len(selected) else 1)


if __name__ == "__main__":
    main()
