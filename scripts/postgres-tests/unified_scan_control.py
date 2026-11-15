#!/usr/bin/env python3
"""Paired standalone/query scan results and evidence of actual PostgreSQL requests."""
import attach_control

SCAN="postgres_scan('dbname=postgresscanner','public','scan_unified')"
PUSH="postgres_scan_pushdown('dbname=postgresscanner','public','scan_unified')"
QUERY="postgres_query('p','SELECT i,s FROM scan_unified')"
SETUP="CALL postgres_execute('p','CREATE TABLE scan_unified AS SELECT i,''row-''||i AS s FROM generate_series(1,100000) i; ANALYZE scan_unified'); SET threads=4; SET pg_pages_per_task=1;"
LOG="SET enable_logging=false; SET logging_storage='memory'; CALL enable_logging('PostgresQueryLog');"
CHECK=" FROM duckdb_logs_parsed('PostgresQueryLog') "
def evidence(condition,predicate):
    return "SELECT CASE WHEN "+condition+" THEN true ELSE error('Missing expected PostgreSQL scan behavior') END"+CHECK+"WHERE "+predicate+";"
attach_control.CASES=[
    ('view-update-rowid',"CALL postgres_execute('p','CREATE VIEW scan_view AS SELECT * FROM source'); UPDATE p.scan_view SET s='updated' WHERE id=1",'column "ctid" does not exist'),
    ('view-delete-rowid',"CALL postgres_execute('p','CREATE VIEW scan_view AS SELECT * FROM source'); DELETE FROM p.scan_view WHERE id=1",'column "ctid" does not exist'),
    ('legacy-parallel-snapshot',SETUP+LOG+f'SELECT count(*),sum(i) FROM {SCAN};'+evidence('count(*)>1',"query='BEGIN TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY'")+evidence('count(*)>0',"query LIKE 'SET TRANSACTION SNAPSHOT%'"),None),
    ('legacy-filter-pushdown',SETUP+LOG+f'SELECT i,s FROM {PUSH} WHERE i>=99997 ORDER BY i;'+evidence('count(*)>0',"query LIKE '%\"i\" >= ''99997''%'"),None),
    ('legacy-no-filter-pushdown',SETUP+LOG+f'SELECT count(*) FROM {SCAN} WHERE i>=99997;'+evidence('count(*)=0',"query LIKE '%\"i\" >= ''99997''%'"),None),
    ('legacy-filter-independent-setting',SETUP+'SET pg_experimental_filter_pushdown=false;'+LOG+f'SELECT count(*) FROM {PUSH} WHERE i>=99997;'+evidence('count(*)>0',"query LIKE '%\"i\" >= ''99997''%'"),None),
    ('legacy-limit-disables-tasks',SETUP+LOG+f'SELECT i FROM {PUSH} LIMIT 3 OFFSET 7;'+evidence('count(*)>0',"query LIKE 'COPY (%LIMIT 3 OFFSET 7%'" )+evidence('count(*)=0',"query LIKE '%ctid BETWEEN%'"),None),
    ('legacy-ctid-disabled',SETUP+'SET pg_use_ctid_scan=false;'+LOG+f'SELECT count(*),sum(i) FROM {SCAN};'+evidence('count(*)=0',"query LIKE '%ctid BETWEEN%'"),None),
    ('legacy-text-disables-tasks',SETUP+'SET pg_use_text_protocol=true;'+LOG+f'SELECT count(*),sum(i) FROM {SCAN};'+evidence('count(*)=0',"query LIKE 'COPY (%' OR query LIKE '%ctid BETWEEN%'"),None),
    ('query-copy-and-limit',SETUP+LOG+f'SELECT i FROM {QUERY} LIMIT 3 OFFSET 7;'+evidence('count(*)>0',"query LIKE 'COPY (%LIMIT 3 OFFSET 7%'"),None),
    ('query-filter-not-pushed',SETUP+LOG+f'SELECT i FROM {QUERY} WHERE i>=99997 ORDER BY i;'+evidence('count(*)=0',"query LIKE '%\"i\" >= ''99997''%'"),None),
    ('mixed-query-table-join',SETUP+'SET pg_connection_limit=2;'+f'SELECT count(*),sum(t.i) FROM p.scan_unified t JOIN {QUERY} q ON t.i=q.i',None),
    ('mixed-query-table-no-optimizer',SETUP+'PRAGMA disable_optimizer;'+f'SELECT count(*),sum(t.i) FROM p.scan_unified t JOIN {QUERY} q ON t.i=q.i',None),
    ('query-two-scans',SETUP+f'SELECT count(*),sum(a.i) FROM {QUERY} a JOIN {QUERY} b ON a.i=b.i',None),
    ('query-text-parameters',"SET pg_use_text_protocol=true; SELECT * FROM postgres_query('p','SELECT $1::integer + 1 AS i',params:={n:41::INTEGER});",None),
    ('query-without-transaction',LOG+"SELECT * FROM postgres_query('p','SELECT current_setting(''transaction_isolation'') AS iso',use_transaction=false);"+evidence('count(*)=0',"query LIKE 'BEGIN%'"),None),
    ('legacy-ignores-default-secret',"CREATE SECRET(TYPE postgres,DBNAME 'not_a_database'); SELECT count(*) FROM postgres_scan('dbname=postgresscanner','public','source')",None),
    ('legacy-prepared-reexecution',SETUP+f'PREPARE x AS SELECT count(*),sum(i) FROM {PUSH} WHERE i>99990; EXECUTE x; EXECUTE x;',None),
    ('query-prepared-reexecution',SETUP+f'PREPARE x AS SELECT i FROM {QUERY} LIMIT 3; EXECUTE x; EXECUTE x;',None),
]
for protocol in ['true','false']:
    attach_control.CASES.append(('legacy-timeout-'+protocol,"CALL postgres_execute('p','CREATE VIEW scan_delay AS SELECT 1 i FROM pg_sleep(0.1)'); SET pg_statement_timeout_millis=1; SET pg_use_text_protocol="+protocol+"; SELECT * FROM postgres_scan('dbname=postgresscanner','public','scan_delay')",'statement timeout'))
if __name__=='__main__':attach_control.main()
