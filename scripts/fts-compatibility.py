#!/usr/bin/env python3
"""Real FTS database interoperability between DuckDB 1.5.6 and roastedDuck.

All processes run serially. Raw SQL/stdout/stderr and structured comparisons are
retained beneath a new output directory. This supplements, not replaces, the
unchanged upstream SQL tests and the full Snowball differential corpus.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys


PIN = "6814ec9a7d5fd63500176507262b0dbf7cea0095"
SOURCE_HASHES = {
    "extension/fts/fts_extension.cpp": "6ac62b9c5e8818a383735d0a16ee56f408641325ae6ee1f47c1acff3dd2adf5f",
    "extension/fts/fts_indexing.cpp": "80cf42878121f59a28b9d7e8f96b48a83b5da09e6889abd97c29cb0e2925977a",
}


def digest(path):
    hasher = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            hasher.update(block)
    return hasher.hexdigest()


def literal(value):
    return "'" + str(value).replace("'", "''") + "'"


def decode_results(raw):
    """Parse the CLI's consecutive JSON arrays without changing string bytes.

    Numbers remain textual tokens so floating-point rounding in Python cannot
    conceal a difference. SQL strings, including embedded NUL, remain exact.
    """
    decoder = json.JSONDecoder(parse_float=lambda value: {"$number": value},
                               parse_int=lambda value: {"$integer": value})
    remaining = raw.decode("utf-8", errors="strict").strip()
    result = []
    while remaining:
        value, end = decoder.raw_decode(remaining)
        if not isinstance(value, list):
            raise ValueError("CLI result is not a JSON row array")
        result.append(value)
        remaining = remaining[end:].strip()
    return result


CREATE_SQL = """
CREATE TABLE documents(id VARCHAR PRIMARY KEY, title VARCHAR, body VARCHAR);
INSERT INTO documents VALUES
 ('doc1', 'Running ducks', 'The duck runs beside a café. Running, running!'),
 ('doc2', 'Quiet cats', 'A cat sleeps; ducks swim and cafés open.'),
 ('doc3', 'Connections', 'Connected connections connect searchable documents.'),
 ('doc4', '', ''), ('doc5', NULL, NULL),
 ('doc6', 'QUÁCKING', 'QUÁCKING+BÁRKING+999 and naïve façade'),
 ('doc7', 'Repeated repeated', 'ducks ducks ducks swimming');
PRAGMA create_fts_index('documents', 'id', 'title', 'body');
CREATE SCHEMA archive;
CREATE TABLE archive.documents(id VARCHAR, title VARCHAR, body VARCHAR);
INSERT INTO archive.documents SELECT * FROM main.documents;
CREATE TABLE archive.stop_words(sw VARCHAR);
INSERT INTO archive.stop_words VALUES ('duck'), ('The'), ('and');
PRAGMA create_fts_index('archive.documents', 'id', 'title', 'body',
 stemmer='none', stopwords='archive.stop_words', lower=false, strip_accents=false);
CHECKPOINT;
"""


def snapshots():
    result = [
        ("columns", """SELECT schema_name, table_name, column_name, column_index,
            data_type, is_nullable FROM duckdb_columns()
            WHERE schema_name IN ('fts_main_documents','fts_archive_documents')
            ORDER BY schema_name, table_name, column_index"""),
        ("macros", """SELECT schema_name, function_name, parameters, parameter_types,
            macro_definition FROM duckdb_functions()
            WHERE schema_name IN ('fts_main_documents','fts_archive_documents')
            ORDER BY schema_name, function_name"""),
    ]
    for schema, source in [("fts_main_documents", "main.documents"),
                           ("fts_archive_documents", "archive.documents")]:
        for table in ["stopwords", "docs", "fields", "terms", "dict", "stats"]:
            result.append((schema + "." + table,
                           f"SELECT to_json(t)::VARCHAR AS payload FROM {schema}.{table} t ORDER BY payload"))
        result.append((schema + ".tokenize",
                       f"SELECT {schema}.tokenize(' QUÁCKING+running café 999 ') AS tokens"))
        for query, options, label in [
            ("ducks running", "", "rank"),
            ("ducks ducks", "", "repeated"),
            ("quacking", "", "accent"),
            ("zzzznomatch", "", "missing"),
            ("", "", "empty"),
            ("ducks running", ", conjunctive := true", "conjunctive"),
            ("ducks running", ", fields := 'title', k := 1.8, b := 0.4", "fields"),
        ]:
            result.append((schema + "." + label, f"""SELECT id, score::VARCHAR AS score
                FROM (SELECT id, {schema}.match_bm25(id, {literal(query)}{options}) AS score
                      FROM {source}) q ORDER BY score DESC NULLS LAST, id"""))
        result.append((schema + ".null",
                       f"SELECT {schema}.match_bm25('doc1', NULL)::VARCHAR AS score"))
    return result


class Harness:
    def __init__(self, args):
        self.args = args
        self.out = args.out
        self.records = []
        self.failures = []
        self.counter = 0
        self.empty_init = self.out / "empty.duckdbrc"
        self.empty_init.write_text("")
        self.environment = os.environ.copy()
        self.environment["TMPDIR"] = str(self.out / "tmp")
        (self.out / "tmp").mkdir()
        self.extension_dir = self.out / "extensions"
        data = args.fts_extension.read_bytes()
        footer = data[-512:]
        fields = [footer[i * 32:(i + 1) * 32].rstrip(b"\0").decode("ascii") for i in range(8)]
        if fields[4] != PIN[:7] or fields[5] != "v1.5.6" or fields[3] != "CPP":
            raise ValueError(f"Wrong pinned oracle extension footer: {fields}")
        extension = self.extension_dir / fields[5] / fields[6] / "fts.duckdb_extension"
        extension.parent.mkdir(parents=True)
        shutil.copyfile(args.fts_extension, extension)
        self.identity = {
            "mode": "ORACLE_FIXTURE_SELF_TEST" if args.oracle_only else "CROSS_ENGINE",
            "upstream_pin": PIN,
            "oracle": {"path": str(args.oracle_cli), "sha256": digest(args.oracle_cli)},
            "rust": None if args.oracle_only else {"path": str(args.rust_cli), "sha256": digest(args.rust_cli)},
            "fts_extension": {"path": str(args.fts_extension), "sha256": digest(args.fts_extension), "footer": fields},
            "source_files": {},
        }
        if not args.oracle_only and self.identity["oracle"]["sha256"] == self.identity["rust"]["sha256"]:
            raise ValueError("Oracle and Rust executables are identical; use --oracle-only for fixture calibration")
        for name, expected in SOURCE_HASHES.items():
            actual = digest(args.upstream_source / name)
            if actual != expected:
                raise ValueError(f"Source hash does not match pinned FTS revision: {name}")
            self.identity["source_files"][name] = actual
        self.identity["upstream_tests"] = [
            {"path": str(path.relative_to(args.upstream_source)), "sha256": digest(path)}
            for path in sorted((args.upstream_source / "test/sql/fts").glob("*.test*"))
        ]
        if len(self.identity["upstream_tests"]) != 10:
            raise ValueError("Expected all ten pinned upstream FTS SQL tests")

    def save(self):
        (self.out / "verification.json").write_text(json.dumps({
            "identity": self.identity, "records": self.records, "failures": self.failures,
            "passed": not self.failures,
            "acceptance_scope": "oracle fixture validation only; no Rust acceptance" if self.args.oracle_only
                                else "bidirectional FTS persisted-index interoperability; not complete extension acceptance",
        }, ensure_ascii=False, indent=2) + "\n")

    def run(self, engine, label, database, sql, *, autoload=False, readonly=False):
        self.counter += 1
        prefix = self.out / f"{self.counter:03d}-{engine}-{label}"
        oracle = engine == "oracle" or self.args.oracle_only
        cli = self.args.oracle_cli if oracle else self.args.rust_cli
        setup = (f"SET extension_directory={literal(self.extension_dir)};\n"
                 "SET autoinstall_known_extensions=false;\nSET autoload_known_extensions=true;\nSET threads=1;\n")
        if not autoload:
            setup += f"LOAD {literal(self.args.fts_extension)};\n" if oracle else "LOAD fts;\n"
        query = (setup + sql + "\n").encode("utf-8")
        prefix.with_suffix(".sql").write_bytes(query)
        command = [str(cli), "-json", "-batch", "-bail", "-init", str(self.empty_init)]
        if readonly:
            command.append("-readonly")
        command.append(str(database))
        try:
            completed = subprocess.run(command, input=query, capture_output=True,
                                       cwd=self.out, env=self.environment, timeout=self.args.timeout)
            status, stdout, stderr = completed.returncode, completed.stdout, completed.stderr
        except subprocess.TimeoutExpired as error:
            status, stdout, stderr = "timeout", error.stdout or b"", error.stderr or b""
        prefix.with_suffix(".stdout").write_bytes(stdout)
        prefix.with_suffix(".stderr").write_bytes(stderr)
        record = {"engine": engine, "label": label, "database": str(database),
                  "status": status, "autoload": autoload, "readonly": readonly,
                  "raw_prefix": prefix.name}
        self.records.append(record)
        if status != 0:
            self.failures.append({"stage": label, "engine": engine, "reason": "CLI execution failed"})
            self.save()
            raise RuntimeError(f"{engine} {label} failed; see {prefix}.stderr")
        try:
            rows = decode_results(stdout)
        except (ValueError, UnicodeError) as error:
            self.failures.append({"stage": label, "engine": engine, "reason": str(error)})
            self.save()
            raise
        record["results"] = rows
        self.save()
        return rows

    def compare(self, label, expected, actual):
        if expected != actual:
            self.failures.append({"stage": label, "reason": "exact structured result mismatch",
                                  "expected": expected, "actual": actual})
        self.save()

    def snapshot(self, engine, label, database, *, autoload=False, attached=False):
        queries = snapshots()
        # One tagged JSON row per query makes empty result sets and CLI formatting
        # independent of one another, without normalizing any returned SQL values.
        sql = "\n".join(f"SELECT {literal(name)} AS section, (SELECT to_json(list(t))::VARCHAR "
                         f"FROM ({query}) t) AS payload;" for name, query in queries)
        if attached:
            sql = f"ATTACH {literal(database)} AS fixture (READ_ONLY); USE fixture;\n" + sql
        return self.run(engine, label, ":memory:" if attached else database, sql,
                        autoload=autoload, readonly=not attached)

    def direction(self, creator, reader):
        database = self.out / f"created-by-{creator}.duckdb"
        self.run(creator, "create", database, CREATE_SQL)
        before = digest(database)
        baseline = self.snapshot(creator, "creator-reopen", database)
        for autoload, attached, label in [(False, False, "reader-reopen"),
                                          (True, False, "reader-autoload"),
                                          (False, True, "reader-attach")]:
            actual = self.snapshot(reader, label, database, autoload=autoload, attached=attached)
            self.compare(f"{creator}-to-{reader}-{label}", baseline, actual)
        if digest(database) != before:
            self.failures.append({"stage": f"{creator}-read-only", "reason": "read-only probes changed database bytes"})
        edited = self.out / f"created-by-{creator}-rebuilt-by-{reader}.duckdb"
        shutil.copyfile(database, edited)
        self.run(reader, "rebuild", edited, """
            INSERT INTO documents VALUES ('doc8', 'Fresh ducks', 'Running duck additions');
            PRAGMA create_fts_index('documents', 'id', 'title', 'body', overwrite=true);
            CHECKPOINT;
        """)
        rebuilt = self.snapshot(reader, "rebuilt-reader", edited)
        reopened = self.snapshot(creator, "rebuilt-creator", edited)
        self.compare(f"{creator}-to-{reader}-rebuild-return", rebuilt, reopened)
        self.run(reader, "drop", edited,
                 "PRAGMA drop_fts_index('documents'); PRAGMA drop_fts_index('archive.documents'); CHECKPOINT;")
        self.run(creator, "dropped-reopen", edited,
                 "SELECT schema_name FROM duckdb_schemas() WHERE schema_name LIKE 'fts_%' ORDER BY schema_name;",
                 readonly=True)
        # Some CLI versions emit no bytes for an empty row set; count explicitly.
        count = self.run(creator, "dropped-count", edited,
                         "SELECT count(*)::VARCHAR AS remaining FROM duckdb_schemas() WHERE schema_name LIKE 'fts_%';",
                         readonly=True)
        self.compare(f"{creator}-to-{reader}-drop", [[{"remaining": "0"}]], count)
        self.save()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--oracle-cli", required=True, type=Path)
    parser.add_argument("--rust-cli", type=Path)
    parser.add_argument("--fts-extension", required=True, type=Path)
    parser.add_argument("--upstream-source", required=True, type=Path)
    parser.add_argument("--out", required=True, type=Path)
    parser.add_argument("--timeout", type=int, default=120)
    parser.add_argument("--oracle-only", action="store_true", help="Validate fixtures using C++ in both roles; never grants Rust acceptance")
    args = parser.parse_args()
    if not args.oracle_only and args.rust_cli is None:
        parser.error("--rust-cli is required unless --oracle-only is specified")
    for name in ["oracle_cli", "rust_cli", "fts_extension", "upstream_source", "out"]:
        value = getattr(args, name)
        if value is not None:
            setattr(args, name, value.resolve())
    args.out.mkdir(parents=True, exist_ok=False)
    harness = Harness(args)
    try:
        identity_sql = "SELECT version() AS engine_version; SELECT extension_version FROM duckdb_extensions() WHERE extension_name='fts' AND loaded;"
        oracle = harness.run("oracle", "identity", ":memory:", identity_sql)
        harness.compare("oracle-version", [[{"engine_version": "v1.5.6"}], [{"extension_version": PIN[:7]}]], oracle)
        if harness.failures:
            raise RuntimeError("Oracle identity did not match the pinned version")
        harness.run("rust", "identity", ":memory:", identity_sql)
        harness.direction("oracle", "rust")
        harness.direction("rust", "oracle")
    except Exception as error:
        if not harness.failures:
            harness.failures.append({"stage": "harness", "reason": repr(error)})
        print(str(error), file=sys.stderr)
    harness.save()
    print(json.dumps({"passed": not harness.failures, "mode": harness.identity["mode"],
                      "records": len(harness.records), "report": str(args.out / "verification.json")}))
    return 1 if harness.failures else 0


if __name__ == "__main__":
    sys.exit(main())
