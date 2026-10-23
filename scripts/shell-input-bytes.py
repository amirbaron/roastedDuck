#!/usr/bin/env python3
"""Compare native shell argument bytes with C++, preserving stdout/stderr as bytes."""

import argparse
import json
import os
from pathlib import Path
import subprocess


def cases():
    for value in range(256):
        byte = bytes([value])
        yield f"raw-quoted-{value:02x}", b".print 'a" + byte + b"b' tail\n"
        yield f"octal-{value:02x}", f'.print "a\\{value:03o}b" tail\n'.encode()
        yield f"command-{value:02x}", b".notacommand" + byte + b"\n"
    for index, value in enumerate([
        b"caf\xe9", "café 🦆".encode(), b"\xff\xfe\x80", b"\xc0\x80",
        b"\xed\xa0\x80", b"\xf4\x90\x80\x80", b"ab\0cd",
    ]):
        yield f"raw-sequence-{index}", b".print '" + value + b"' tail\n"
    yield "octal-overflow", b'.print "\\777" "\\400ignored" "\\000ignored" end\n'
    yield "single-quote-escapes-literal", b".print '\\351' '\\000tail' end\n"
    yield "unknown-escape", b'.print "\\xe9" "\\q" "\\\\" end\n'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--reference", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    home = args.output / "home"
    home.mkdir(exist_ok=True)
    env = {**os.environ, "HOME": str(home.resolve()), "TMPDIR": str(args.output.resolve())}
    records, mismatches = [], []
    for name, data in cases():
        record = {"name": name, "input_hex": data.hex()}
        for label, binary in (("cpp", args.reference), ("rust", args.binary)):
            run = subprocess.run(
                [b"duckdb", b"-init", b"/dev/null", b"-batch", b"-csv", b"-noheader"],
                executable=os.fsencode(binary.resolve()), input=data,
                capture_output=True, cwd=args.output, env=env, timeout=30,
            )
            record[label] = {"exit": run.returncode, "stdout_hex": run.stdout.hex(),
                             "stderr_hex": run.stderr.hex()}
        records.append(record)
        if record["cpp"] != record["rust"]:
            mismatches.append(record)
    assert len(records) == 778
    (args.output / "cases.json").write_text(json.dumps(records, indent=2) + "\n")
    report = {"total": len(records), "mismatch_count": len(mismatches), "mismatches": mismatches}
    (args.output / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    print(f"{len(records)} native byte cases, {len(mismatches)} mismatches")
    return bool(mismatches)


if __name__ == "__main__":
    raise SystemExit(main())
