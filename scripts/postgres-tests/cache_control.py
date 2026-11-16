#!/usr/bin/env python3
"""Paired catalog/cache controls, including executed SQL and custom signatures."""
import attach_control

ATTACH_Q = "ATTACH 'dbname=postgresscanner' AS q(TYPE POSTGRES);"
LOG = "SET enable_logging=false; SET logging_storage='memory'; CALL enable_logging('PostgresQueryLog');"
TABLES = "SELECT count(*) FROM duckdb_tables() WHERE database_name='p' AND table_name='external_new';"
CUSTOM = "SELECT pg_class.oid, relname, pg_class.xmin FROM pg_class JOIN pg_namespace ON relnamespace=pg_namespace.oid WHERE relkind IN (''r'',''v'',''m'',''f'',''p'') AND pg_namespace.nspname=${SCHEMA} ORDER BY pg_class.oid"
attach_control.CASES = [
    ('metadata-does-not-lock-relation', ATTACH_Q + "BEGIN; PREPARE metadata_only AS SELECT * FROM p.source; CALL postgres_execute('q','SET lock_timeout=''250ms''; ALTER TABLE source ADD COLUMN external_column INTEGER',use_transaction=false); ROLLBACK", None),
    ('registered-defaults', "SELECT current_setting('pg_staleness_query_enabled'),current_setting('pg_staleness_query'),current_setting('pg_use_information_schema_introspection')", None),
    ('main-schema-precedence', "CREATE SCHEMA p.main; CREATE TABLE p.main.which_one AS SELECT 7 AS i; CREATE TABLE p.public.which_one AS SELECT 9 AS i; USE p; SELECT * FROM main.which_one; SELECT * FROM which_one", None),
    ('composite-outer-alias', "CREATE TYPE p.shape AS STRUCT(a INTEGER,b VARCHAR); CREATE TABLE p.shapes(s p.shape); INSERT INTO p.shapes VALUES ({a:4,b:'four'}); SELECT s.a,s.b,typeof(s) FROM p.shapes", None),
    ('info-real-query-path', "SET pg_use_information_schema_introspection=true;" + LOG + "SELECT id,matrix,typeof(matrix) FROM p.source WHERE id=1; SELECT CASE WHEN count(*)=0 THEN true ELSE error('Nonstandard metadata query') END FROM duckdb_logs_parsed('PostgresQueryLog') WHERE query LIKE '%pg_catalog.%' OR query LIKE '%pg_class%' OR query LIKE '%pg_attribute%'", None),
    ('info-created-and-reloaded', "SET pg_use_information_schema_introspection=true; CREATE TABLE p.is_created(d DECIMAL(9,2),j JSON,a INTEGER[]); INSERT INTO p.is_created VALUES(12.50,'{\"x\":2}',[3,NULL]); SELECT d,j,a,typeof(d),typeof(j),typeof(a) FROM p.is_created; CALL pg_clear_cache(); SELECT d,j,a,typeof(d),typeof(j),typeof(a) FROM p.is_created", None),
    ('info-created-types', "SET pg_use_information_schema_introspection=true; CREATE TYPE p.shade AS ENUM('red','blue'); CREATE TYPE p.shape AS STRUCT(a INTEGER,b VARCHAR); CREATE TABLE p.typed(s p.shape,e p.shade); INSERT INTO p.typed VALUES({a:4,b:'four'},'red'); SELECT s.a,s.b,e FROM p.typed; CALL pg_clear_cache(); SELECT s,e,typeof(s),typeof(e) FROM p.typed", None),
    ('info-names-only-empty-types-indexes', "CREATE TYPE p.hidden_type AS ENUM('a'); CREATE INDEX hidden_index ON p.source(id); SET pg_use_information_schema_introspection=true; SELECT count(*) FROM duckdb_types() WHERE database_name='p' AND type_name='hidden_type'; SELECT count(*) FROM duckdb_indexes() WHERE database_name='p' AND index_name='hidden_index'", None),
    ('signature-fourth-column-ignored', ATTACH_Q + "SET GLOBAL pg_staleness_query_enabled=true; SET GLOBAL pg_staleness_query='SELECT 1,${SCHEMA},''fixed'',random()';" + TABLES + "CREATE TABLE q.external_new AS SELECT 42 i;" + TABLES + "CALL pg_clear_cache();" + TABLES, None),
    ('signature-null-fields', ATTACH_Q + "SET GLOBAL pg_staleness_query_enabled=true; SET GLOBAL pg_staleness_query='SELECT NULL,${SCHEMA},NULL';" + TABLES + "CREATE TABLE q.external_new AS SELECT 42 i;" + TABLES + "CALL pg_clear_cache();" + TABLES, None),
    ('signature-multiple-statements', ATTACH_Q + "SET GLOBAL pg_staleness_query_enabled=true; SET GLOBAL pg_staleness_query='SELECT 1;" + CUSTOM + "';" + TABLES + "CREATE TABLE q.external_new AS SELECT 42 i;" + TABLES, None),
    ('signature-invalid-query', "SET GLOBAL pg_staleness_query_enabled=true; SET GLOBAL pg_staleness_query='SELECT invalid_column,${SCHEMA},1'; SELECT * FROM p.source", 'Failed to execute query'),
    ('cache-clear-inside-rollback', "SET GLOBAL pg_staleness_query_enabled=true; SELECT count(*) FROM p.source; BEGIN; ALTER TABLE p.source ADD COLUMN local_only INTEGER; CALL pg_clear_cache(); SELECT local_only FROM p.source WHERE id=1; ROLLBACK; SELECT column_name FROM duckdb_columns() WHERE database_name='p' AND table_name='source' ORDER BY column_index", None),
]
if __name__ == '__main__':
    attach_control.main()
