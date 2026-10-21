#!/usr/bin/env python3
"""Differential test of a shell binary against DuckDB's C++ CLI.

Runs both binaries on the same cases and compares stdout, stderr, the exit code and the
files each case writes, byte for byte. A case runs in a scratch directory at the same path
for both binaries, with argv[0] `duckdb` and an empty `HOME`, so paths and the program
name print the same; anything else that legitimately varies between two runs (timings) is
masked by the case's own `normalize` rules.

Usage: scripts/shell-diff.py --binary RUSTDB --reference DUCKDB [-k SUBSTRING] [--show]
                             [--jobs N] [--work DIR] [CASES.toml ...]

Cases live in TOML files (default: scripts/shell-diff/*.toml), one `[[case]]` table each:

  name       unique name (required)
  args       command-line arguments (after argv[0])
  stdin      text fed to standard input (default: none, stdin is /dev/null)
  encoding   how stdin and files are encoded (default utf-8; latin-1 writes the bytes
             \u0000-ÿ stand for, to feed invalid UTF-8)
  merged     standard error goes to the same pipe as standard output, to compare how
             the two interleave (the buffering of stdout)
  interrupt_after  seconds after which the shell gets SIGINT (Ctrl-C)
  known      why the case differs (an engine difference recorded in docs/work-queue.md):
             it is listed but does not count as a failure (--show prints it in full)
  files      {name = content} written to the scratch directory first
  home       {name = content} written to the empty HOME first (e.g. `.duckdbrc`)
  env        extra environment variables
  normalize  [[regex, replacement], ...] applied to both outputs before comparing
  vary       {key = [values]}: one case per value, `{key}` in name/args/stdin replaced
  pty        run under a pseudo-terminal: `keys` is a list of strings sent one at a time
             (numbers wait that many seconds); `cols`/`rows` set the window size;
             `resize` = [[after_key_index, cols, rows], ...] resizes mid-session
"""
import argparse
import concurrent.futures
import difflib
import fcntl
import glob
import itertools
import os
import pty
import re
import select
import shutil
import signal
import struct
import subprocess
import sys
import termios
import time
import tomllib

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DEFAULT_WORK = os.path.join(
    os.environ.get("TMP_ROOT", "/Volumes/amirs ssd/ai context/conductor/duckdb-rust/tmp"), "shell-diff")
REF = os.environ.get("REF", os.path.join(ROOT, ".context/duckdb-download/duckdb-1.5.6"))
# Files whose bytes depend on the library (database files hold its version): compared by
# existence only.
OPAQUE_SUFFIXES = (".db", ".duckdb", ".wal", ".parquet")


def expand(cases):
    """Expands `vary` into one case per combination of values."""
    for case in cases:
        vary = case.pop("vary", None)
        if not vary:
            yield case
            continue
        keys = list(vary)
        for values in itertools.product(*(vary[k] for k in keys)):
            def sub(text):
                for key, value in zip(keys, values):
                    text = text.replace("{" + key + "}", str(value))
                return text
            new = dict(case)
            new["name"] = sub(case["name"])
            if "args" in case:
                new["args"] = [sub(a) for a in case["args"]]
            if "stdin" in case:
                new["stdin"] = sub(case["stdin"])
            if "keys" in case:
                new["keys"] = [sub(k) if isinstance(k, str) else k for k in case["keys"]]
            yield new


def prepare(work, case):
    if os.path.exists(work):
        shutil.rmtree(work)
    os.makedirs(work)
    home = os.path.join(work, "home")
    os.mkdir(home)
    os.symlink(os.path.join(REF, "data"), os.path.join(work, "data"))
    for name, content in case.get("files", {}).items():
        path = os.path.join(work, name)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "wb") as f:
            f.write(content.encode(case.get("encoding", "utf-8")))
    for name, content in case.get("home", {}).items():
        with open(os.path.join(home, name), "w") as f:
            f.write(content)
    env = {"PATH": "/usr/bin:/bin:/usr/sbin:/sbin", "HOME": home, "LANG": "en_US.UTF-8"}
    env.update(case.get("env", {}))
    return home, env


def written_files(work, case):
    """The files a run left in its scratch directory (besides the case's own inputs)."""
    given = set(case.get("files", {}))
    result = {}
    for dirpath, dirnames, filenames in os.walk(work):
        dirnames[:] = [d for d in dirnames if not os.path.islink(os.path.join(dirpath, d))]
        for name in filenames:
            path = os.path.join(dirpath, name)
            rel = os.path.relpath(path, work)
            if rel in given or os.path.islink(path):
                continue
            if rel.startswith("home/") and rel[5:] in case.get("home", {}):
                continue
            with open(path, "rb") as f:
                data = f.read()
            if rel.endswith(OPAQUE_SUFFIXES) or ".wal" in rel or rel.endswith(".tmp") or data[8:12] == b"DUCK":
                result[rel] = b"<opaque>"
            else:
                result[rel] = data
    return result


def run_pipe(binary, work, case):
    _, env = prepare(work, case)
    stdin = case.get("stdin")
    encoding = case.get("encoding", "utf-8")
    process = subprocess.Popen(["duckdb"] + case.get("args", []), executable=binary,
                               stdin=subprocess.PIPE if stdin is not None else subprocess.DEVNULL,
                               stdout=subprocess.PIPE,
                               stderr=subprocess.STDOUT if case.get("merged") else subprocess.PIPE,
                               cwd=work, env=env)
    data = stdin.encode(encoding) if stdin is not None else None
    if "interrupt_after" in case:
        # Ctrl-C while the input is being processed: write it, wait, send SIGINT.
        if data is not None:
            process.stdin.write(data)
            process.stdin.flush()
        time.sleep(case["interrupt_after"])
        process.send_signal(signal.SIGINT)
        data = None
    try:
        stdout, stderr = process.communicate(data, timeout=case.get("timeout", 60))
    except subprocess.TimeoutExpired:
        process.kill()
        process.communicate()
        raise
    return {"stdout": stdout, "stderr": stderr or b"", "exit": process.returncode,
            "files": written_files(work, case)}


def run_pty(binary, work, case):
    _, env = prepare(work, case)
    env.setdefault("TERM", "xterm-256color")
    cols, rows = case.get("cols", 100), case.get("rows", 30)
    pid, fd = pty.fork()
    if pid == 0:
        os.chdir(work)
        os.execve(binary, ["duckdb"] + case.get("args", []), env)
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
    out = bytearray()

    def drain(timeout):
        end = time.time() + timeout
        while True:
            remaining = end - time.time()
            if remaining <= 0:
                return True
            ready, _, _ = select.select([fd], [], [], remaining)
            if not ready:
                return True
            try:
                data = os.read(fd, 65536)
            except OSError:
                return False
            if not data:
                return False
            out.extend(data)
            # answer cursor position queries like a terminal with the cursor at 1;1
            if b"\x1b[6n" in data:
                os.write(fd, b"\x1b[1;1R")

    resizes = {r[0]: (r[1], r[2]) for r in case.get("resize", [])}
    alive = drain(case.get("startup", 1.0))
    for index, key in enumerate(case["keys"]):
        if index in resizes:
            cols, rows = resizes[index]
            fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
            os.kill(pid, signal.SIGWINCH)
            alive = drain(0.3)
        if not alive:
            break
        if isinstance(key, (int, float)):
            alive = drain(key)
            continue
        os.write(fd, key.encode())
        alive = drain(case.get("delay", 0.3))
    if alive:
        drain(case.get("tail", 1.0))
    status = None
    for _ in range(50):
        done, status = os.waitpid(pid, os.WNOHANG)
        if done:
            break
        time.sleep(0.05)
    else:
        os.kill(pid, signal.SIGKILL)
        _, status = os.waitpid(pid, 0)
        status = -1
    os.close(fd)
    code = os.waitstatus_to_exitcode(status) if status not in (None, -1) else "killed"
    return {"stdout": bytes(out), "stderr": b"", "exit": code, "files": written_files(work, case)}


def normalize(result, case):
    # The reference CLI is built from a source tarball, without the release's version.
    rules = [(re.compile(rb"v0\.0\.1 \(Unknown Version\)|v1\.5\.6 \(Variegata\)"), b"VERSION")]
    rules += [(re.compile(p.encode()), r.encode()) for p, r in case.get("normalize", [])]
    out = dict(result)
    for key in ("stdout", "stderr"):
        data = result[key]
        for pattern, repl in rules:
            data = pattern.sub(repl, data)
        out[key] = data
    files = {}
    for name, data in result["files"].items():
        for pattern, repl in rules:
            data = pattern.sub(repl, data)
        files[name] = data
    out["files"] = files
    return out


def run_case(case, binary, reference, work_root, index):
    work = os.path.join(work_root, f"case-{index}")
    runner = run_pty if case.get("pty") else run_pipe
    try:
        ref = normalize(runner(reference, work, case), case)
        got = normalize(runner(binary, work, case), case)
    except subprocess.TimeoutExpired as ex:
        return case, None, None, f"timeout: {ex}"
    finally:
        if os.path.exists(work):
            shutil.rmtree(work)
    return case, ref, got, None


def show_bytes(label, data):
    print(f"--- {label} ({len(data)} bytes)")
    for line in data.decode("utf-8", "backslashreplace").split("\n"):
        print("  " + repr(line)[1:-1])


def report(case, ref, got):
    print(f"=== {case['name']}")
    for key in ("stdout", "stderr"):
        if ref[key] != got[key]:
            print(f"--- {key}")
            a = [repr(l)[1:-1] for l in ref[key].decode("utf-8", "backslashreplace").split("\n")]
            b = [repr(l)[1:-1] for l in got[key].decode("utf-8", "backslashreplace").split("\n")]
            for line in itertools.islice(difflib.unified_diff(a, b, "duckdb", "binary", n=1, lineterm=""), 2, 80):
                print(line)
    if ref["exit"] != got["exit"]:
        print(f"--- exit code: duckdb {ref['exit']}, binary {got['exit']}")
    if ref["files"] != got["files"]:
        for name in sorted(set(ref["files"]) | set(got["files"])):
            a, b = ref["files"].get(name), got["files"].get(name)
            if a != b:
                print(f"--- file {name}: duckdb {a!r:.300}\n    binary {b!r:.300}")


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--binary", required=True)
    parser.add_argument("--reference", required=True)
    parser.add_argument("-k", dest="filter", action="append", default=[],
                        help="run only cases whose name contains this (repeatable)")
    parser.add_argument("--show", action="store_true", help="print both outputs of every case run")
    parser.add_argument("--jobs", type=int, default=4)
    parser.add_argument("--work", default=DEFAULT_WORK)
    parser.add_argument("files", nargs="*")
    args = parser.parse_args()

    files = args.files or sorted(glob.glob(os.path.join(ROOT, "scripts/shell-diff/*.toml")))
    cases = []
    for path in files:
        with open(path, "rb") as f:
            cases.extend(expand(tomllib.load(f).get("case", [])))
    names = [c["name"] for c in cases]
    duplicates = {n for n in names if names.count(n) > 1}
    if duplicates:
        sys.exit(f"duplicate case names: {sorted(duplicates)}")
    if args.filter:
        cases = [c for c in cases if any(f in c["name"] for f in args.filter)]

    binary, reference = os.path.abspath(args.binary), os.path.abspath(args.reference)
    os.makedirs(args.work, exist_ok=True)
    differ = known = 0
    # Pseudo-terminal sessions depend on timing and fork: they run one at a time, after the
    # others.
    piped = [(i, c) for i, c in enumerate(cases) if not c.get("pty")]
    sessions = [(i, c) for i, c in enumerate(cases) if c.get("pty")]
    with concurrent.futures.ThreadPoolExecutor(max_workers=args.jobs) as pool:
        futures = [pool.submit(run_case, case, binary, reference, args.work, i) for i, case in piped]
        results = (f.result() for f in futures)
        results = itertools.chain(results, (run_case(case, binary, reference, args.work, i)
                                            for i, case in sessions))
        for case, ref, got, error in results:
            if error:
                differ += 1
                print(f"=== {case['name']}\n--- {error}")
                continue
            if args.show:
                print(f"=== {case['name']} (exit duckdb {ref['exit']}, binary {got['exit']})")
                show_bytes("duckdb stdout", ref["stdout"])
                show_bytes("duckdb stderr", ref["stderr"])
                if ref != got:
                    show_bytes("binary stdout", got["stdout"])
                    show_bytes("binary stderr", got["stderr"])
            if ref != got and "known" in case and not args.show:
                known += 1
                print(f"=== {case['name']}: known difference: {case['known']}")
            elif ref != got:
                differ += 1
                report(case, ref, got)
            sys.stdout.flush()
    print(f"{differ} of {len(cases)} cases differ" + (f" ({known} more as known)" if known else ""))
    sys.exit(1 if differ else 0)


if __name__ == "__main__":
    main()
