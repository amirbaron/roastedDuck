#!/usr/bin/env python3
"""Differential regular SELECT verification, including observable re-execution."""
import argparse
import hashlib
import json
from pathlib import Path

from s3_version_control import API


CASES = {
    'switch-parameters': [
        'CREATE TABLE t(x INTEGER)', 'INSERT INTO t VALUES (0),(1),(4),(8)',
        "PREPARE p AS SELECT switch(x, MAP {$1:$2, $3:$4}, $5) AS label FROM t ORDER BY x",
        "EXECUTE p(1,'yes',0,'no','maybe')", 'DEALLOCATE p',
        'PRAGMA enable_verification',
        "SELECT switch(x, MAP {1:'yes',0:'no'}, 'maybe') AS label FROM t ORDER BY x",
        'PRAGMA disable_verification',
    ],
    'summarize': [
        'PRAGMA enable_verification',
        'SUMMARIZE SELECT 9223372036854775296',
        'SUMMARIZE VALUES (1.0),(6754950520)',
        'SUMMARIZE SELECT 42 AS named',
        'PRAGMA disable_verification',
    ],
    # Where the prepared verifier replaces constants: every clause, but neither the
    # query inside a subquery expression nor a SHOW/DESCRIBE reference.
    'constants': [
        'CREATE TABLE t AS SELECT i, i % 3 AS k FROM range(12) r(i)',
        'PRAGMA enable_verification',
        'SELECT (SELECT max(i) FROM t WHERE k = 1) AS top, 7 AS seven',
        'SELECT i FROM t WHERE i IN (SELECT i FROM t WHERE k = 2 ORDER BY 1 DESC LIMIT 2) ORDER BY i',
        'SELECT i, EXISTS (SELECT 1 FROM t u WHERE u.i = t.i + 10) AS later FROM t ORDER BY i LIMIT 3',
        'WITH c AS (SELECT k, count(*) AS n FROM t WHERE i > 2 GROUP BY k) SELECT * FROM c ORDER BY k',
        'SELECT * REPLACE (i + 100 AS i) FROM t ORDER BY i LIMIT 2 OFFSET 1',
        'SELECT k, sum(i) FILTER (WHERE i > 3) OVER (PARTITION BY k ORDER BY i ROWS BETWEEN 1 PRECEDING AND 1 FOLLOWING) AS s FROM t ORDER BY k, i',
        'SELECT list_transform([1, 2, 3], x -> x + 10) AS shifted',
        'SELECT * FROM t PIVOT (sum(i) FOR k IN (0, 1, 2)) ORDER BY ALL',
        "SELECT * FROM (VALUES (1, 'a'), (2, 'b')) v(n, s) ORDER BY n DESC",
        'SELECT DISTINCT ON (k) k, i FROM t ORDER BY k, i DESC',
        "SELECT x BETWEEN 2 AND 4 AS mid, CASE WHEN x > 3 THEN 'hi' ELSE 'lo' END AS band FROM range(6) r(x)",
        'SELECT a.i, b.k FROM t a JOIN t b ON a.i = b.i + 1 AND b.k <> 1 ORDER BY a.i LIMIT 4',
        "DESCRIBE SELECT 42 AS answer, 'x' AS letter",
        'PRAGMA disable_verification',
    ],
    'stable': [
        'PRAGMA enable_verification',
        'SELECT 42 AS a, 42 AS b, NULL AS n',
        "SELECT {'x':[1,NULL,3], 'y':'é'} AS value",
        "SELECT 'NaN'::DOUBLE AS n, '-Infinity'::DOUBLE AS i, -0.0::DOUBLE AS z",
        'WITH t AS (SELECT i%3 k, i v FROM range(20) t(i)) SELECT k,sum(v) FROM t GROUP BY k ORDER BY k',
        'SELECT a.i, b.i FROM range(4) a(i) JOIN range(6) b(i) ON a.i=b.i ORDER BY a.i',
        'SELECT i, sum(i) OVER (ORDER BY i ROWS BETWEEN 1 PRECEDING AND CURRENT ROW) FROM range(5) t(i)',
        'WITH RECURSIVE t(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM t WHERE i<5) SELECT * FROM t',
        # Ordered outside the set operation: its own row order is unspecified.
        'SELECT * FROM (SELECT 1 UNION SELECT 2 EXCEPT SELECT 3) ORDER BY ALL',
        'SELECT * FROM range(10) LIMIT 3 OFFSET 1',
        "SELECT CAST('2025-01-01' AS DATE), CAST(1.25 AS DECIMAL(10,2)), [1,2,3]",
        'PRAGMA disable_verification',
    ],
    'prepared': [
        'PREPARE p AS SELECT $1::INTEGER + 7 AS result',
        'PRAGMA enable_verification',
        'EXECUTE p(35)',
        'DEALLOCATE p',
        'PRAGMA disable_verification',
    ],
    'sequence': [
        'CREATE SEQUENCE s', 'PRAGMA enable_verification',
        "SELECT nextval('s') AS x", 'PRAGMA disable_verification',
        "SELECT currval('s') AS last_value",
    ],
    'query-text': [
        'PRAGMA enable_verification',
        'SELECT current_query() AS q',
        "select current_query() = '' OR starts_with(current_query(), 'select') AS original_or_empty",
        'PRAGMA disable_verification',
    ],
    'error': [
        'PRAGMA enable_verification', "SELECT error('owned verification error')",
        'SELECT 42 AS recovered', 'PRAGMA disable_verification',
    ],
    'transaction': [
        'BEGIN', 'CREATE TABLE t(i INTEGER)', 'INSERT INTO t VALUES (1),(2)',
        'PRAGMA enable_verification', 'SELECT sum(i) FROM t',
        'PRAGMA disable_verification', 'ROLLBACK',
        "SELECT count(*) FROM duckdb_tables() WHERE table_name='t'",
    ],
    'settings': [
        'PRAGMA enable_verification', 'SET debug_force_external=true',
        "SELECT current_setting('debug_force_external') AS external",
        "SELECT current_setting('enable_caching_operators') AS caching",
        'PRAGMA disable_verification',
        "SELECT current_setting('enable_caching_operators') AS caching",
    ],
    'profiling': [
        "SET enable_profiling='no_output'", 'PRAGMA enable_verification',
        'SELECT 42 AS x', 'PRAGMA disable_verification',
        "SELECT current_setting('enable_profiling') AS profiling",
    ],
}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--library', type=Path, required=True)
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=False)
    records = []
    for case, statements in CASES.items():
        directory = out / case
        directory.mkdir()
        api = API(args.library, directory / 'secrets')
        try:
            for sql in statements:
                record = dict(case=case, **api.query(sql))
                records.append(record)
                (out / 'progress.json').write_text(json.dumps(records, indent=2) + '\n')
        finally:
            api.close()
    (out / 'verification.json').write_text(json.dumps(dict(records=records,
        library_sha256=hashlib.sha256(args.library.read_bytes()).hexdigest()), indent=2) + '\n')
    print(f'{len(records)} verifier records captured', flush=True)


if __name__ == '__main__':
    main()
