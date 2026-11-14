#!/usr/bin/env python3
"""Measure VSS recall with shared float32 fixtures and C++ SQL tie adjudication.

The full run has 10,000 vectors, 200 held-out queries, three distributions,
three dimensions, three metrics, three k values, two search settings, and five
mutation cohorts, on both engines. Limited selections never grant full acceptance.
Only Python's standard library is used. Engines run serially in isolated CLI jobs.
"""

import argparse
import csv
import gzip
import hashlib
import itertools
import json
import math
import os
from pathlib import Path
import platform
import plistlib
import random
import signal
import struct
import subprocess
import sys
import time


PIN = "b833341c8737fd3f3558c7720cc575ae8fc82598"
DISTRIBUTIONS = ("uniform", "clustered", "duplicates")
DIMENSIONS = (3, 32, 128)
METRICS = {
    "l2sq": "array_distance",
    "cosine": "array_cosine_distance",
    "ip": "array_negative_inner_product",
}
KS = (1, 10, 100)
PROFILES = ("default", "ef200")
COHORTS = ("baseline", "after_insert", "after_update", "after_delete", "after_compact")
SEED = 0xD0C156
FULL_CELLS = tuple(itertools.product(DISTRIBUTIONS, DIMENSIONS, METRICS, KS))


def sha256(path):
    value = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            value.update(block)
    return value.hexdigest()


def sql_string(value):
    return "'" + str(value).replace("'", "''") + "'"


def float32(value):
    return struct.unpack("<f", struct.pack("<f", value))[0]


def vector_text(vector):
    # Nine significant digits round-trip every finite IEEE float32 value.
    return "[" + ",".join(format(value, ".9g") for value in vector) + "]"


def vector_literal(vector):
    """Use DOUBLE SQL tokens before FLOAT casts, avoiding DECIMAL arithmetic.

    Nine significant digits still round-trip the shared float32 fixture values.
    Decimal-looking SQL tokens can take DuckDB's DECIMAL -> FLOAT division path,
    which need not match the CSV VARCHAR -> FLOAT parser byte for byte.
    """
    return "[" + ",".join(format(value, ".8e") for value in vector) + "]"


def write_json(path, value):
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n")


def json_results(path):
    """Stream rows from consecutive JSON arrays, independent of CLI line breaks."""
    decoder = json.JSONDecoder()
    pending = ""
    state = "array"
    with path.open(encoding="utf-8", errors="strict") as stream:
        while True:
            block = stream.read(65536)
            pending += block
            while pending.strip():
                pending = pending.lstrip()
                if state == "array":
                    if pending[0] != "[":
                        raise ValueError(f"Expected a CLI JSON row array in {path}")
                    pending, state = pending[1:], "first"
                    continue
                if state == "separator":
                    if pending[0] == ",":
                        pending, state = pending[1:], "value"
                    elif pending[0] == "]":
                        pending, state = pending[1:], "array"
                    else:
                        raise ValueError(f"Invalid CLI JSON array separator in {path}")
                    continue
                if state == "first" and pending[0] == "]":
                    pending, state = pending[1:], "array"
                    continue
                try:
                    value, end = decoder.raw_decode(pending)
                except json.JSONDecodeError:
                    if block:
                        break
                    raise
                if not isinstance(value, dict):
                    raise ValueError(f"Expected a CLI JSON row object in {path}")
                yield value
                pending, state = pending[end:], "separator"
            if not block:
                if pending.strip() or state != "array":
                    raise ValueError(f"Truncated CLI JSON stream in {path}")
                break


def parse_cell(value):
    pieces = value.split(":")
    if len(pieces) != 4:
        raise argparse.ArgumentTypeError("cell must be distribution:dimension:metric:k")
    try:
        cell = pieces[0], int(pieces[1]), pieces[2], int(pieces[3])
    except ValueError as error:
        raise argparse.ArgumentTypeError(str(error)) from error
    if cell not in FULL_CELLS:
        raise argparse.ArgumentTypeError("cell is outside the fixed acceptance matrix")
    return cell


def make_fixture(directory, distribution, dimensions, count, queries):
    directory.mkdir(parents=True)
    seed = SEED + DIMENSIONS.index(dimensions) * 10000 + DISTRIBUTIONS.index(distribution) * 100000
    centers_rng = random.Random(seed)
    centers = [[float32(centers_rng.uniform(-1, 1)) for _ in range(dimensions)] for _ in range(32)]
    duplicate_pool = [[float32(centers_rng.uniform(-1, 1)) for _ in range(dimensions)]
                      for _ in range(max(5, min(500, count // 20)))]

    def sample(rng, *, held_out=False):
        if distribution == "clustered":
            center = centers[rng.randrange(len(centers))]
            return [float32(value + rng.gauss(0, 0.05)) for value in center]
        if distribution == "duplicates" and not held_out:
            return list(duplicate_pool[rng.randrange(len(duplicate_pool))])
        return [float32(rng.uniform(-1, 1)) for _ in range(dimensions)]

    def save(name, rows):
        path = directory / name
        with path.open("w", newline="", encoding="utf-8") as stream:
            writer = csv.writer(stream)
            writer.writerow(("id", "vec"))
            for key, vector in rows:
                writer.writerow((key, vector_text(vector)))
        return path

    vectors_rng = random.Random(seed + 1)
    query_rng = random.Random(seed + 2)
    insert_rng = random.Random(seed + 3)
    update_rng = random.Random(seed + 4)
    query_vectors = [sample(query_rng, held_out=True) for _ in range(queries)]
    # Query vectors are sampled independently. In the duplicate distribution,
    # exact distance ties arise from repeated stored vectors, not leaked queries.
    mutation_count = min(200, max(1, count // 10))
    files = {
        "vectors": save("vectors.csv", ((key, sample(vectors_rng)) for key in range(1, count + 1))),
        "queries": save("queries.csv", enumerate(query_vectors)),
        "inserts": save("inserts.csv", ((count + key, sample(insert_rng)) for key in range(1, mutation_count + 1))),
        "updates": save("updates.csv", ((key, sample(update_rng)) for key in range(1, mutation_count + 1))),
    }
    metadata = {
        "distribution": distribution, "dimensions": dimensions, "seed": seed,
        "vectors": count, "queries": queries, "mutation_count": mutation_count,
        "delete_ids": [mutation_count + 1, mutation_count * 2],
        "float_encoding": "IEEE754 float32 rounded before 9-significant-digit CSV formatting",
        "files": {key: {"path": str(path), "sha256": sha256(path)} for key, path in files.items()},
    }
    write_json(directory / "fixture.json", metadata)
    return metadata, files, query_vectors


def csv_relation(path):
    return f"read_csv({sql_string(path)}, header=true, columns={{'id':'BIGINT','vec':'VARCHAR'}})"


def setup_sql(engine, extension, files, dimensions, metrics, query_vectors, *, indexed):
    yield "SET threads=1;"
    yield "SET autoinstall_known_extensions=false;"
    yield f"LOAD {sql_string(extension)};" if engine == "oracle" else "LOAD vss;"
    yield ("SELECT 'identity' AS tag, version() AS engine_version, "
           "(SELECT loaded FROM duckdb_extensions() WHERE extension_name='vss') AS loaded, "
           "(SELECT extension_version FROM duckdb_extensions() WHERE extension_name='vss') AS extension_version;")
    yield f"CREATE TABLE vectors AS SELECT id, vec::FLOAT[{dimensions}] AS vec FROM {csv_relation(files['vectors'])};"
    yield f"CREATE TABLE queries AS SELECT id AS qid, vec::FLOAT[{dimensions}] AS vec FROM {csv_relation(files['queries'])};"
    yield "SELECT 'input' AS tag, count(*) AS row_count, sum(id)::VARCHAR AS id_sum FROM vectors;"
    for qid, vector in enumerate(query_vectors):
        literal = f"{vector_literal(vector)}::FLOAT[{dimensions}]"
        # Execute before index construction and every measurement. Equality is
        # adjudicated by the engine on the actual typed operands, with no tolerance.
        yield (f"SELECT 'query-vector:{qid}' AS tag, CASE WHEN "
               f"(SELECT count(*) FROM queries WHERE qid={qid} AND vec={literal})=1 "
               f"THEN true ELSE error('Query vector FLOAT equality failed for qid {qid}') END AS matches;")
    if indexed:
        for metric in metrics:
            yield f"CREATE INDEX idx_{metric} ON vectors USING HNSW(vec) WITH(metric={sql_string(metric)});"
    else:
        yield "SET disabled_optimizers='extension';"


def mutation_sql(cohort, files, dimensions, mutation_count, metrics, *, indexed):
    if cohort == "after_insert":
        yield f"INSERT INTO vectors SELECT id, vec::FLOAT[{dimensions}] FROM {csv_relation(files['inserts'])};"
    elif cohort == "after_update":
        yield f"UPDATE vectors SET vec=u.vec::FLOAT[{dimensions}] FROM {csv_relation(files['updates'])} u WHERE vectors.id=u.id;"
    elif cohort == "after_delete":
        yield f"DELETE FROM vectors WHERE id BETWEEN {mutation_count + 1} AND {mutation_count * 2};"
    elif cohort == "after_compact" and indexed:
        for metric in metrics:
            yield f"PRAGMA hnsw_compact_index('idx_{metric}');"


def ann_select(request, vector, dimensions):
    distance = f"{METRICS[request['metric']]}(vec,{vector_literal(vector)}::FLOAT[{dimensions}])"
    return (f"SELECT 'ann:{request['record_id']}' AS tag, list(id) AS ids FROM "
            f"(SELECT id FROM vectors ORDER BY {distance} LIMIT {request['k']}) nearest")


class Harness:
    def __init__(self, args):
        self.args = args
        self.out = args.out
        self.failures = []
        self.failure_count = 0
        self.failure_categories = {}
        self.jobs = []
        self.fixtures = []
        self.cells = []
        self.counters = {"ann_statements": 0, "ann_measurements": 0, "ann_profile_statements": 0,
                         "ann_profiles_recorded": 0, "exact_truth_statements": 0,
                         "exact_profile_statements": 0, "exact_profiles_recorded": 0,
                         "candidate_scoring_statements": 0, "candidate_ids": 0,
                         "adjudicated_queries": 0, "query_vector_guard_statements": 0,
                         "query_vectors_verified": 0}
        self.full = args.limit == 200 and args.vectors == 10000 and set(args.cells) == set(FULL_CELLS) and args.cohorts == COHORTS
        self.empty_init = self.out / "empty.duckdbrc"
        self.empty_init.write_text("")
        self.environment = os.environ.copy()
        (self.out / "tmp").mkdir()
        self.environment["TMPDIR"] = str(self.out / "tmp")
        artifact = args.vss_extension.read_bytes()
        footer = artifact[-512:]
        fields = [footer[i * 32:(i + 1) * 32].rstrip(b"\0").decode("ascii") for i in range(8)]
        if fields[3] != "CPP" or fields[4] != PIN[:7] or fields[5] != "v1.5.6":
            raise ValueError(f"Oracle extension is not the pinned VSS artifact: {fields}")
        self.identity = {
            "oracle_cli": {"path": str(args.oracle_cli), "sha256": sha256(args.oracle_cli)},
            "rust_cli": {"path": str(args.rust_cli), "sha256": sha256(args.rust_cli)},
            "vss_extension": {"path": str(args.vss_extension), "sha256": sha256(args.vss_extension), "footer": fields},
            "upstream_vss_pin": PIN, "harness_sha256": sha256(Path(__file__).resolve()),
            "python": sys.version, "platform": platform.platform(), "seed": SEED,
            "query_sql_literal_encoding": "nine-significant-digit exponential DOUBLE tokens cast to FLOAT[N]",
            "query_operand_guard": "every query array must equal its CSV-loaded FLOAT array in each engine before measurement",
        }
        if self.identity["oracle_cli"]["sha256"] == self.identity["rust_cli"]["sha256"]:
            raise ValueError("Oracle and Rust executables must be different artifacts")

    def fail(self, record):
        self.failure_count += 1
        stage = record.get("stage", "unknown")
        self.failure_categories[stage] = self.failure_categories.get(stage, 0) + 1
        if len(self.failures) < 100:
            self.failures += [record]

    def save(self, *, finished=False):
        expected = len(self.args.cells) * len(self.args.cohorts) * len(PROFILES) * 2 * self.args.limit
        summary = {
            "scope": "FULL_MATRIX" if self.full else "NOT_FULL_DIAGNOSTIC",
            "finished": finished, "passed": finished and not self.failures,
            "full_acceptance": finished and self.full and not self.failures,
            "identity": self.identity, "vectors": self.args.vectors, "queries": self.args.limit,
            "selected_cells": self.args.cells, "cohorts": self.args.cohorts,
            "search_profiles": {"default": "RESET hnsw_ef_search", "ef200": "SET hnsw_ef_search=200"},
            "construction": "default M/M0/ef_construction; metric alone specified; one build thread",
            "threshold": "Rust ef200 mean tie-aware recall >= 19/20 in every cell; default and oracle are reported separately",
            "expected_ann_measurements": expected, "counters": self.counters,
            "plan_evidence_scope": "one executed EXPLAIN ANALYZE at query 0 per engine/cohort/metric/k/search-profile cell",
            "fixtures": self.fixtures, "jobs": self.jobs, "cells": self.cells,
            "failure_count": self.failure_count, "failure_categories": self.failure_categories,
            "failures": self.failures, "failure_examples_limit": 100,
            "limitations": "recall and mutation cohorts only; does not replace upstream SQL tests, MVCC concurrency tests, or deferred persistence gates",
        }
        write_json(self.out / "verification.json", summary)

    def run_job(self, directory, engine, lines, stage):
        directory.mkdir(parents=True, exist_ok=True)
        sql_path = directory / "statements.sql"
        with sql_path.open("w", encoding="utf-8") as stream:
            for line in lines:
                stream.write(line + "\n")
        stdout_path, stderr_path = directory / "stdout.json-stream", directory / "stderr.log"
        cli = self.args.oracle_cli if engine == "oracle" else self.args.rust_cli
        expected_cli_hash = self.identity["oracle_cli" if engine == "oracle" else "rust_cli"]["sha256"]
        if sha256(cli) != expected_cli_hash:
            raise RuntimeError(f"Selected {engine} executable changed before {stage}")
        command = [str(cli), "-json", "-batch", "-bail", "-init", str(self.empty_init), ":memory:"]
        record = {"stage": stage, "engine": engine, "command": command,
                  "sql": str(sql_path), "sql_sha256": sha256(sql_path),
                  "stdout": str(stdout_path), "stderr": str(stderr_path)}
        self.jobs.append(record)
        self.save()
        print(json.dumps({"stage": stage, "status": "started"}), flush=True)
        started = time.monotonic()
        reason = None
        with sql_path.open("rb") as stdin, stdout_path.open("wb") as stdout, stderr_path.open("wb") as stderr:
            process = subprocess.Popen(command, stdin=stdin, stdout=stdout, stderr=stderr,
                                       cwd=directory, env=self.environment, start_new_session=True)
            heartbeat = started
            try:
                while process.poll() is None:
                    elapsed = time.monotonic() - started
                    sizes = stdout_path.stat().st_size + stderr_path.stat().st_size
                    sizes += sum(path.stat().st_size for path in (directory / "profiles").glob("*.txt"))
                    if elapsed > self.args.timeout or sizes > self.args.max_output_mb * 1024 * 1024:
                        reason = "timeout" if elapsed > self.args.timeout else "output limit"
                        os.killpg(process.pid, signal.SIGTERM)
                        try:
                            process.wait(timeout=10)
                        except subprocess.TimeoutExpired:
                            os.killpg(process.pid, signal.SIGKILL)
                            process.wait()
                        break
                    try:
                        process.wait(timeout=1)
                    except subprocess.TimeoutExpired:
                        pass
                    if time.monotonic() - heartbeat >= 30:
                        print(json.dumps({"stage": stage, "status": "running", "elapsed_seconds": int(elapsed), "output_bytes": sizes}), flush=True)
                        heartbeat = time.monotonic()
            finally:
                if process.poll() is None:
                    os.killpg(process.pid, signal.SIGTERM)
                    try:
                        process.wait(timeout=10)
                    except subprocess.TimeoutExpired:
                        os.killpg(process.pid, signal.SIGKILL)
                        process.wait()
        record.update(exit=process.returncode, stop_reason=reason,
                      elapsed_seconds=time.monotonic() - started,
                      stdout_sha256=sha256(stdout_path), stderr_sha256=sha256(stderr_path),
                      cli_sha256_after=sha256(cli))
        self.save()
        if record["cli_sha256_after"] != expected_cli_hash:
            raise RuntimeError(f"Selected {engine} executable changed during {stage}")
        if reason or process.returncode != 0:
            raise RuntimeError(f"{stage} failed ({reason or process.returncode}); see {stderr_path}")
        print(json.dumps({"stage": stage, "status": "completed"}), flush=True)
        return stdout_path

    def check_identity(self, row, engine):
        if row.get("engine_version") != "v1.5.6" or row.get("loaded") is not True:
            raise ValueError(f"Wrong {engine} version or unloaded VSS: {row}")
        if engine == "oracle" and row.get("extension_version") != PIN[:7]:
            raise ValueError(f"Wrong oracle VSS revision: {row}")

    def check_vector_guard(self, row, seen):
        qid = int(row["tag"].removeprefix("query-vector:"))
        if qid not in range(self.args.limit) or qid in seen or row.get("matches") is not True:
            raise ValueError(f"Query FLOAT equality guard failed or duplicated: {row}")
        seen.add(qid)
        self.counters["query_vectors_verified"] += 1

    def check_profile(self, path, engine, *, exact=False):
        if not path.is_file():
            raise ValueError(f"Missing executed plan profile: {path}")
        if path.stat().st_size > 1024 * 1024:
            raise ValueError(f"Unexpectedly large plan profile: {path}")
        text = path.read_text(encoding="utf-8", errors="strict")
        self.counters["exact_profiles_recorded" if exact else "ann_profiles_recorded"] += 1
        if exact:
            good = "HNSW_INDEX_SCAN" not in text and "HNSW Index" not in text and ("TABLE_SCAN" in text or "SEQ_SCAN" in text)
        else:
            good = "HNSW Index" in text and "TABLE_SCAN" in text and "exact scan" not in text.lower()
            if engine == "rust":
                good = good and "HNSW graph traversal" in text
        if not good:
            self.fail({"stage": "plan-evidence", "engine": engine, "exact": exact,
                                  "path": str(path), "reason": "required executed plan/mode evidence missing"})
        return {"path": str(path), "sha256": sha256(path), "passed": good}

    def dataset(self, distribution, dimensions, pairs):
        root = self.out / f"{distribution}-{dimensions}"
        metadata, files, query_vectors = make_fixture(root / "fixtures", distribution, dimensions,
                                                      self.args.vectors, self.args.limit)
        self.fixtures.append(metadata)
        requests = []
        metrics = sorted({metric for metric, _ in pairs})
        last_cohort = max(COHORTS.index(cohort) for cohort in self.args.cohorts)
        for engine in ("oracle", "rust"):
            directory = root / engine
            (directory / "profiles").mkdir(parents=True)
            expected = {}
            expected_settings = {}

            def statements():
                self.counters["query_vector_guard_statements"] += len(query_vectors)
                yield from setup_sql(engine, self.args.vss_extension, files, dimensions, metrics, query_vectors, indexed=True)
                for cohort in COHORTS[:last_cohort + 1]:
                    yield from mutation_sql(cohort, files, dimensions, metadata["mutation_count"], metrics, indexed=True)
                    if cohort not in self.args.cohorts:
                        continue
                    for metric, k in pairs:
                        for profile in PROFILES:
                            yield "RESET hnsw_ef_search;" if profile == "default" else "SET hnsw_ef_search=200;"
                            setting_tag = f"setting:{cohort}:{metric}:{k}:{profile}"
                            expected_settings[setting_tag] = None if profile == "default" else "200"
                            yield f"SELECT {sql_string(setting_tag)} AS tag,current_setting('hnsw_ef_search')::VARCHAR AS value;"
                            profile_name = f"{cohort}-{metric}-k{k}-{profile}.txt"
                            profile_path = directory / "profiles" / profile_name
                            for qid, vector in enumerate(query_vectors):
                                request = {"record_id": len(requests) + 1, "engine": engine,
                                           "distribution": distribution, "dimensions": dimensions,
                                           "cohort": cohort, "metric": metric, "k": k,
                                           "profile": profile, "qid": qid, "plan_profile": str(profile_path)}
                                requests.append(request)
                                expected[request["record_id"]] = request
                                query = ann_select(request, vector, dimensions)
                                if qid == 0:
                                    yield f".once profiles/{profile_name}"
                                    yield "EXPLAIN ANALYZE " + query + ";"
                                    self.counters["ann_profile_statements"] += 1
                                yield query + ";"
                                self.counters["ann_statements"] += 1

            stdout = self.run_job(directory, engine, statements(), f"{distribution}-{dimensions}-{engine}-ann")
            seen = set()
            identity_count, input_count = 0, 0
            seen_settings = set()
            seen_vectors = set()
            for row in json_results(stdout):
                tag = row.get("tag", "")
                if tag == "identity":
                    self.check_identity(row, engine)
                    identity_count += 1
                elif tag == "input":
                    input_count += 1
                    if row.get("row_count") != self.args.vectors or int(row["id_sum"]) != self.args.vectors * (self.args.vectors + 1) // 2:
                        raise ValueError(f"Input fixture load mismatch: {row}")
                elif tag.startswith("query-vector:"):
                    self.check_vector_guard(row, seen_vectors)
                elif tag.startswith("setting:"):
                    if tag not in expected_settings or tag in seen_settings or "value" not in row or row["value"] != expected_settings[tag]:
                        raise ValueError(f"Search-setting confirmation mismatch: {row}")
                    seen_settings.add(tag)
                elif tag.startswith("ann:"):
                    record_id = int(tag[4:])
                    if record_id not in expected or record_id in seen:
                        raise ValueError(f"Unexpected or duplicate ANN tag: {tag}")
                    seen.add(record_id)
                    self.counters["ann_measurements"] += 1
                    request = expected[record_id]
                    ids = row.get("ids") or []
                    if not isinstance(ids, list) or any(type(value) is not int for value in ids):
                        raise ValueError(f"ANN returned malformed candidate IDs: {tag}")
                    request["ids"] = ids
                    request["validation_errors"] = []
                    self.counters["candidate_ids"] += len(ids)
                    if len(ids) != request["k"] or len(set(ids)) != len(ids):
                        request["validation_errors"].append("candidate length/uniqueness mismatch")
                        self.fail({"stage": "candidate-shape", "record_id": record_id,
                                              "engine": engine, "fixture": str(root), "expected_k": request["k"],
                                              "length": len(ids), "unique": len(set(ids))})
                else:
                    raise ValueError(f"Unexpected CLI result: {row}")
            if seen != set(expected):
                raise ValueError(f"Missing {len(set(expected) - seen)} ANN results for {engine}")
            if (identity_count != 1 or input_count != 1 or seen_settings != set(expected_settings)
                    or seen_vectors != set(range(self.args.limit))):
                raise ValueError("Missing or duplicate identity/input/settings confirmation")
            for request in expected.values():
                if request["qid"] == 0:
                    request["plan_evidence"] = self.check_profile(Path(request["plan_profile"]), engine)
            self.save()
        self.adjudicate(root, metadata, files, query_vectors, requests, pairs, metrics, last_cohort)

    def adjudicate(self, root, metadata, files, query_vectors, requests, pairs, metrics, last_cohort):
        request_path, candidates_path = root / "requests.csv", root / "candidates.csv"
        with request_path.open("w", newline="") as stream, candidates_path.open("w", newline="") as candidates_stream:
            writer, candidates = csv.writer(stream), csv.writer(candidates_stream)
            writer.writerow(("record_id", "engine", "cohort", "metric", "qid", "k", "profile"))
            candidates.writerow(("record_id", "ordinal", "id"))
            for request in requests:
                writer.writerow([request[name] for name in ("record_id", "engine", "cohort", "metric", "qid", "k", "profile")])
                for ordinal, candidate in enumerate(request["ids"]):
                    candidates.writerow((request["record_id"], ordinal, candidate))
        metadata["candidate_files"] = {str(path): sha256(path) for path in (request_path, candidates_path)}
        directory = root / "oracle-scoring"
        (directory / "profiles").mkdir(parents=True)

        def statements():
            self.counters["query_vector_guard_statements"] += len(query_vectors)
            yield from setup_sql("oracle", self.args.vss_extension, files, metadata["dimensions"], metrics, query_vectors, indexed=False)
            yield f"CREATE TABLE requests AS SELECT * FROM read_csv({sql_string(request_path)}, header=true, columns={{'record_id':'BIGINT','engine':'VARCHAR','cohort':'VARCHAR','metric':'VARCHAR','qid':'INTEGER','k':'INTEGER','profile':'VARCHAR'}});"
            yield f"CREATE TABLE candidates AS SELECT * FROM read_csv({sql_string(candidates_path)}, header=true, columns={{'record_id':'BIGINT','ordinal':'INTEGER','id':'BIGINT'}});"
            yield "CREATE TABLE truth(cohort VARCHAR, metric VARCHAR, qid INTEGER, k INTEGER, cutoff DOUBLE, strict_total BIGINT);"
            for cohort in COHORTS[:last_cohort + 1]:
                yield from mutation_sql(cohort, files, metadata["dimensions"], metadata["mutation_count"], metrics, indexed=False)
                if cohort not in self.args.cohorts:
                    continue
                for metric in metrics:
                    ks = sorted(k for selected_metric, k in pairs if selected_metric == metric)
                    values = ",".join(f"({k})" for k in ks)
                    for qid, vector in enumerate(query_vectors):
                        distance = f"{METRICS[metric]}(vec,{vector_literal(vector)}::FLOAT[{metadata['dimensions']}])"
                        top = f"SELECT id,{distance} AS distance FROM vectors ORDER BY distance,id LIMIT {max(ks)}"
                        if qid == 0:
                            yield f".once profiles/{cohort}-{metric}-exact.txt"
                            yield f"EXPLAIN ANALYZE {top};"
                            self.counters["exact_profile_statements"] += 1
                        yield (f"INSERT INTO truth WITH top AS MATERIALIZED ({top}), "
                               "ranked AS (SELECT distance,row_number() OVER(ORDER BY distance,id) AS rank FROM top), "
                               f"cutoffs AS (SELECT ks.k,ranked.distance AS cutoff FROM ranked JOIN (VALUES {values}) ks(k) ON rank=ks.k) "
                               f"SELECT {sql_string(cohort)},{sql_string(metric)},{qid},c.k,c.cutoff, "
                               "count(*) FILTER(WHERE t.distance<c.cutoff) FROM cutoffs c CROSS JOIN top t GROUP BY c.k,c.cutoff;")
                        self.counters["exact_truth_statements"] += 1
                yield ("CREATE OR REPLACE TEMP TABLE scored AS SELECT r.record_id,c.ordinal,c.id, v.id IS NOT NULL AS valid, "
                       "CASE r.metric WHEN 'l2sq' THEN array_distance(v.vec,q.vec) "
                       "WHEN 'cosine' THEN array_cosine_distance(v.vec,q.vec) "
                       "WHEN 'ip' THEN array_negative_inner_product(v.vec,q.vec) END AS score "
                       "FROM requests r JOIN candidates c USING(record_id) JOIN queries q USING(qid) "
                       f"LEFT JOIN vectors v ON c.id=v.id WHERE r.cohort={sql_string(cohort)};")
                yield ("SELECT r.record_id,t.cutoff::VARCHAR AS cutoff,t.strict_total, "
                       "count(s.id) AS returned,count(s.id) FILTER(WHERE NOT s.valid) AS invalid, "
                       "count(s.id) FILTER(WHERE s.score<t.cutoff) AS better, "
                       "count(s.id) FILTER(WHERE s.score=t.cutoff) AS tied, "
                       "coalesce(list(struct_pack(id:=s.id,score:=s.score::VARCHAR) ORDER BY s.ordinal) FILTER(WHERE s.id IS NOT NULL),[]) AS scores "
                       "FROM requests r JOIN truth t ON (r.cohort=t.cohort AND r.metric=t.metric AND r.qid=t.qid AND r.k=t.k) "
                       f"LEFT JOIN scored s USING(record_id) WHERE r.cohort={sql_string(cohort)} "
                       "GROUP BY r.record_id,t.cutoff,t.strict_total ORDER BY r.record_id;")
                self.counters["candidate_scoring_statements"] += 2

        stdout = self.run_job(directory, "oracle", statements(), f"{root.name}-oracle-exact-adjudication")
        metadata["exact_profiles"] = []
        for cohort in self.args.cohorts:
            for metric in metrics:
                metadata["exact_profiles"].append(
                    self.check_profile(directory / "profiles" / f"{cohort}-{metric}-exact.txt", "oracle", exact=True))
        by_id = {request["record_id"]: request for request in requests}
        groups, seen = {}, set()
        detail_path = root / "per-query.jsonl.gz"
        identity_count, input_count = 0, 0
        seen_vectors = set()
        with gzip.open(detail_path, "wt", encoding="utf-8") as details:
            for row in json_results(stdout):
                if row.get("tag") == "identity":
                    self.check_identity(row, "oracle")
                    identity_count += 1
                    continue
                if row.get("tag") == "input":
                    input_count += 1
                    if row.get("row_count") != self.args.vectors or int(row["id_sum"]) != self.args.vectors * (self.args.vectors + 1) // 2:
                        raise ValueError(f"Oracle adjudication fixture load mismatch: {row}")
                    continue
                if row.get("tag", "").startswith("query-vector:"):
                    self.check_vector_guard(row, seen_vectors)
                    continue
                record_id = row["record_id"]
                if record_id not in by_id or record_id in seen:
                    raise ValueError(f"Unexpected scored record: {record_id}")
                seen.add(record_id)
                request = by_id[record_id]
                k = request["k"]
                strict, better, tied = (int(row[name]) for name in ("strict_total", "better", "tied"))
                credit = better + min(tied, k - strict)
                if int(row["invalid"]) or int(row["returned"]) != k or not (0 <= better <= strict < k) or not (0 <= credit <= k):
                    request["validation_errors"].append("oracle candidate/credit validation mismatch")
                    self.fail({"stage": "oracle-candidate-validation", "fixture": str(root), "record_id": record_id, "row": row})
                key = tuple(request[name] for name in ("engine", "cohort", "metric", "k", "profile"))
                groups.setdefault(key, []).append(credit)
                detail = {name: value for name, value in request.items() if name != "ids"}
                detail.update(oracle=row, tie_credit=credit, recall=credit / k)
                details.write(json.dumps(detail, ensure_ascii=False, separators=(",", ":")) + "\n")
                self.counters["adjudicated_queries"] += 1
        if seen != set(by_id):
            raise ValueError(f"Missing {len(set(by_id) - seen)} oracle score records")
        if identity_count != 1 or input_count != 1 or seen_vectors != set(range(self.args.limit)):
            raise ValueError("Missing or duplicate oracle scoring identity/input confirmation")
        for (engine, cohort, metric, k, profile), credits in sorted(groups.items()):
            recalls = sorted(credit / k for credit in credits)
            def quantile(fraction):
                return recalls[math.ceil(fraction * len(recalls)) - 1]
            passed = sum(credits) * 20 >= 19 * len(credits) * k
            cell = {"distribution": metadata["distribution"], "dimensions": metadata["dimensions"],
                    "engine": engine, "cohort": cohort, "metric": metric, "k": k, "profile": profile,
                    "queries": len(credits), "credit_sum": sum(credits), "possible_credit": len(credits) * k,
                    "mean_recall": sum(credits) / (len(credits) * k), "minimum": recalls[0],
                    "p05": quantile(0.05), "p50": quantile(0.5), "p95": quantile(0.95),
                    "meets_095": passed, "details": str(detail_path)}
            self.cells.append(cell)
            if len(credits) != self.args.limit or (engine == "rust" and profile == "ef200" and not passed):
                self.fail({"stage": "recall-cell", "cell": cell})
        metadata["per_query_sha256"] = sha256(detail_path)
        self.save()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--oracle-cli", required=True, type=Path)
    parser.add_argument("--rust-cli", required=True, type=Path)
    parser.add_argument("--vss-extension", required=True, type=Path)
    parser.add_argument("--out", required=True, type=Path)
    parser.add_argument("--limit", type=int, default=200, help="queries per cell; less than 200 is diagnostic only")
    parser.add_argument("--vectors", type=int, default=10000, help="values other than 10000 are diagnostic only")
    parser.add_argument("--cells", action="append", type=parse_cell, help="repeat distribution:dimension:metric:k selectors; partial matrix is diagnostic")
    parser.add_argument("--cohorts", default=",".join(COHORTS), help="comma-separated cohort subset; partial coverage is diagnostic")
    parser.add_argument("--timeout", type=int, default=7200, help="maximum seconds per isolated CLI job")
    parser.add_argument("--max-output-mb", type=int, default=512, help="per-job stdout+stderr limit; exceeding it fails the job")
    args = parser.parse_args()
    if not 1 <= args.limit <= 200 or args.vectors < 100 or args.timeout < 1 or args.max_output_mb < 1:
        parser.error("require 1 <= limit <= 200, vectors >= 100, and positive resource limits")
    selected_cohorts = set(args.cohorts.split(","))
    if not selected_cohorts or not selected_cohorts.issubset(COHORTS):
        parser.error("unknown or empty cohort selection")
    args.cohorts = tuple(cohort for cohort in COHORTS if cohort in selected_cohorts)
    args.cells = tuple(sorted(set(args.cells or FULL_CELLS)))
    for name in ("oracle_cli", "rust_cli", "vss_extension", "out"):
        setattr(args, name, getattr(args, name).resolve())
    if os.environ.get("CONDUCTOR_IS_LOCAL") == "1":
        mount = Path("/Volumes/amirs ssd")
        info = plistlib.loads(subprocess.check_output(["diskutil", "info", "-plist", str(mount)]))
        if info.get("MountPoint") != str(mount) or not args.out.is_relative_to(mount):
            parser.error("local Conductor recall artifacts require the mounted external SSD")
    args.out.mkdir(parents=True, exist_ok=False)
    try:
        harness = Harness(args)
    except Exception as error:
        write_json(args.out / "verification.json", {"finished": False, "passed": False,
                   "full_acceptance": False, "failure": repr(error)})
        print(str(error), file=sys.stderr)
        return 1
    finished = False
    try:
        harness.save()
        for distribution, dimensions in sorted({(cell[0], cell[1]) for cell in args.cells}):
            pairs = [(metric, k) for dist, dim, metric, k in args.cells if dist == distribution and dim == dimensions]
            harness.dataset(distribution, dimensions, pairs)
        expected = len(args.cells) * len(args.cohorts) * len(PROFILES) * 2 * args.limit
        if harness.counters["ann_measurements"] != expected or harness.counters["adjudicated_queries"] != expected:
            raise ValueError("matrix completion counters disagree")
        if (harness.counters["ann_statements"] != expected
                or harness.counters["ann_profiles_recorded"] != harness.counters["ann_profile_statements"]
                or harness.counters["exact_profiles_recorded"] != harness.counters["exact_profile_statements"]
                or harness.counters["query_vectors_verified"] != harness.counters["query_vector_guard_statements"]):
            raise ValueError("emitted and observed query/profile counters disagree")
        finished = True
    except (Exception, KeyboardInterrupt) as error:
        harness.fail({"stage": "harness", "reason": repr(error)})
        print(str(error), file=sys.stderr, flush=True)
    harness.save(finished=finished)
    print(json.dumps({"finished": finished, "full_matrix": harness.full,
                      "passed": finished and not harness.failures,
                      "report": str(args.out / "verification.json")}), flush=True)
    return 0 if finished and not harness.failures else 1


if __name__ == "__main__":
    sys.exit(main())
