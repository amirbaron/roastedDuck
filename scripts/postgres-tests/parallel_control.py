#!/usr/bin/env python3
"""Paired scan controls with real analyzed PG tables and executed-query evidence."""
import attach_control

SETUP = """
CALL postgres_execute('p', 'CREATE TABLE scan_data AS SELECT i, CASE WHEN i % 3=0 THEN NULL ELSE ''row-''||i END AS s FROM generate_series(1,100000) i; ANALYZE scan_data');
SET threads=4; SET pg_pages_per_task=1;
SET enable_logging=false; SET logging_storage='memory'; CALL enable_logging('PostgresQueryLog');
"""
AGG = "SELECT count(*),sum(i),min(i),max(i) FROM p.scan_data;"
LOG = " FROM duckdb_logs_parsed('PostgresQueryLog') "
attach_control.CASES = [
    ('snapshot-workers', SETUP + AGG + "SELECT CASE WHEN count(*)>1 THEN true ELSE error('Expected multiple worker transactions') END" + LOG + "WHERE query LIKE 'BEGIN TRANSACTION ISOLATION LEVEL REPEATABLE READ%'; SELECT CASE WHEN count(*)>0 THEN true ELSE error('Expected executed query evidence') END" + LOG + "WHERE query LIKE 'SET TRANSACTION SNAPSHOT%';", None),
    ('disabled-ctid', SETUP + "SET pg_use_ctid_scan=false;" + AGG + "SELECT CASE WHEN count(*)=0 THEN true ELSE error('Unexpected scan query') END" + LOG + "WHERE query LIKE '%ctid BETWEEN%';", None),
    ('text-disables-ctid', SETUP + "SET pg_use_text_protocol=true;" + AGG + "SELECT CASE WHEN count(*)=0 THEN true ELSE error('Unexpected scan query') END" + LOG + "WHERE query LIKE '%ctid BETWEEN%';", None),
    ('timeouts-applied', SETUP + "SET pg_statement_timeout_millis=123456; SET pg_idle_in_transaction_timeout_millis=234567;" + AGG + "SELECT CASE WHEN count(*)>0 THEN true ELSE error('Expected executed query evidence') END" + LOG + "WHERE query='SET statement_timeout=123456'; SELECT CASE WHEN count(*)>0 THEN true ELSE error('Expected executed query evidence') END" + LOG + "WHERE query='SET idle_in_transaction_session_timeout=234567';", None),
    ('tight-pool-two-scans', SETUP + "SET pg_connection_limit=2; SELECT count(*),sum(a.i) FROM p.scan_data a JOIN p.scan_data b ON a.i=b.i; SELECT count(*),sum(i) FROM p.scan_data;", None),
    ('filter-executed', SETUP + "SELECT i,s FROM p.scan_data WHERE i>=99990 AND s IS NOT NULL ORDER BY i; SELECT CASE WHEN count(*)>0 THEN true ELSE error('Expected executed query evidence') END" + LOG + "WHERE query LIKE '%WHERE%\"i\" >= ''99990''%';", None),
    ('string-collation', SETUP + "SELECT count(*) FROM p.scan_data WHERE s='row-99998'; SELECT CASE WHEN count(*)>0 THEN true ELSE error('Expected executed query evidence') END" + LOG + "WHERE query LIKE '%COLLATE \"C\"%';", None),
    ('filter-disabled', SETUP + "SET pg_experimental_filter_pushdown=false; SELECT count(*) FROM p.scan_data WHERE i>=99990; SELECT CASE WHEN count(*)=0 THEN true ELSE error('Unexpected scan query') END" + LOG + "WHERE query LIKE '%\"i\" >= ''99990''%';", None),
    ('rowid-is-null', "SELECT count(rowid),count(*) FROM p.source; BEGIN; INSERT INTO p.source VALUES(30,'thirty',NULL); SELECT count(rowid),count(*) FROM p.source; ROLLBACK", None),
    ('mutation-identifiers', "PRAGMA disable_optimizer; UPDATE p.source SET s='changed' WHERE id=3; DELETE FROM p.source WHERE id=4; SELECT id,s FROM p.source WHERE id BETWEEN 2 AND 5 ORDER BY id", None),
    ('optimizer-disabled-materializes', SETUP + "PRAGMA disable_optimizer; SELECT count(*),sum(a.i) FROM p.scan_data a JOIN p.scan_data b ON a.i=b.i", None),
    ('limit-single-query', SETUP + "SELECT i FROM p.scan_data LIMIT 3 OFFSET 7; SELECT CASE WHEN count(*)=0 THEN true ELSE error('Unexpected scan query') END" + LOG + "WHERE query LIKE '%ctid BETWEEN%';", None),
]
if __name__ == '__main__':
    attach_control.main()
