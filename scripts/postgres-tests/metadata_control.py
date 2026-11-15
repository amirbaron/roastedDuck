#!/usr/bin/env python3
"""Compare PostgreSQL metadata through the installed SQL surfaces on both engines."""
import attach_control

attach_control.CASES = [
    ('catalog-type', "SELECT database_name,type,readonly FROM duckdb_databases() WHERE database_name='p'", None),
    ('schema-listing', "SELECT schema_name,internal FROM duckdb_schemas() WHERE database_name='p' AND schema_name IN ('public','extra','pg_catalog','information_schema') ORDER BY schema_name", None),
    ('tables-and-views', "CALL postgres_execute('p','CREATE VIEW public.source_view AS SELECT id FROM public.source'); CALL pg_clear_cache(); SELECT schema_name,table_name,column_count FROM duckdb_tables() WHERE database_name='p' AND schema_name IN ('public','extra') ORDER BY schema_name,table_name", None),
    ('comments', "CALL postgres_execute('p',$$COMMENT ON TABLE source IS 'source comment'; COMMENT ON COLUMN source.s IS 'text comment'$$); CALL pg_clear_cache(); SELECT table_name,comment FROM duckdb_tables() WHERE database_name='p' AND schema_name='public'; SELECT column_name,comment FROM duckdb_columns() WHERE database_name='p' AND schema_name='public' ORDER BY column_index", None),
    ('schema-filter-listing', "ATTACH 'dbname=postgresscanner' AS f(TYPE postgres,SCHEMA 'extra'); SELECT schema_name FROM duckdb_schemas() WHERE database_name='f'; SELECT schema_name,table_name FROM duckdb_tables() WHERE database_name='f'", None),
    ('types-and-case', "CREATE TYPE p.mood AS ENUM ('sad','happy'); USE p; SELECT enum_range(NULL::MOOD); SELECT type_name,labels FROM duckdb_types() WHERE database_name='p' AND schema_name='public' AND type_name='mood'", None),
    ('cleared-listing', "SELECT table_name FROM duckdb_tables() WHERE database_name='p' AND schema_name='extra'; CALL postgres_execute('p','CREATE TABLE extra.added(i INTEGER)'); CALL pg_clear_cache(); SELECT table_name FROM duckdb_tables() WHERE database_name='p' AND schema_name='extra' ORDER BY table_name", None),
]

if __name__ == '__main__':
    attach_control.main()
