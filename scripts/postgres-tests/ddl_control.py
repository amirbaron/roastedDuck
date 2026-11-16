#!/usr/bin/env python3
"""Compare PostgreSQL-owned indexes, views and CHECKPOINT with pinned DuckDB."""
import attach_control

def export_existing_types(engine, out):
    directory = attach_control.literal(str(out / ('export-existing-' + engine)))
    return ("DETACH p; ATTACH 'dbname=postgresscanner' AS p(TYPE postgres,SCHEMA 'public'); "
            "CALL postgres_execute('p','ALTER TABLE source DROP CONSTRAINT source_pkey'); "
            "CREATE INDEX source_text_idx ON p.source(s); "
            "CREATE VIEW p.source_view AS SELECT id FROM public.source WHERE id<=3; "
            f"USE p;\nEXPORT DATABASE {directory};\nUSE memory; DETACH p;\nIMPORT DATABASE {directory};\n"
            "SELECT count(*),sum(id) FROM public.source; SELECT * FROM public.source_view ORDER BY id; "
            "SELECT schema_name,type_name FROM duckdb_types() WHERE NOT internal AND type_name IN ('source','source_view') ORDER BY schema_name,type_name")


def export_roundtrip(engine, out):
    directory = attach_control.literal(str(out / ('export-created-' + engine)))
    return ("CALL postgres_execute('p','CREATE SCHEMA export_case'); DETACH p; "
            "ATTACH 'dbname=postgresscanner' AS p(TYPE postgres,SCHEMA 'export_case'); "
            "CREATE TABLE p.docs AS SELECT i FROM range(10)t(i); "
            "CREATE INDEX docs_idx ON p.docs(i); CREATE VIEW p.doc_ids AS SELECT i FROM export_case.docs WHERE i<3; "
            f"USE p;\nEXPORT DATABASE {directory};\nUSE memory; DETACH p;\nIMPORT DATABASE {directory};\n"
            "SELECT count(*),sum(i) FROM export_case.docs; SELECT * FROM export_case.doc_ids ORDER BY i; "
            "SELECT schema_name,type_name FROM duckdb_types() WHERE NOT internal AND type_name IN ('docs','doc_ids') ORDER BY schema_name,type_name")


attach_control.CASES = [
    ('export-roundtrip', export_roundtrip, None),
    ('export-existing-types', export_existing_types, 'Type with name source does not exist'),
    ('copy-without-use', "CALL postgres_execute('p','ALTER TABLE source DROP CONSTRAINT source_pkey'); ATTACH 'dbname=postgresscanner' AS limited(TYPE postgres,SCHEMA 'extra'); COPY FROM DATABASE limited TO memory; SELECT * FROM memory.extra.extra_table; SELECT schema_name,type_name FROM duckdb_types() WHERE database_name='memory' AND NOT internal ORDER BY schema_name,type_name", None),
    ('created-index-metadata', "CREATE UNIQUE INDEX source_s_idx ON p.source(s); SELECT index_name,table_name,is_unique,is_primary,expressions,sql FROM duckdb_indexes() WHERE database_name='p' AND index_name='source_s_idx'; CALL pg_clear_cache(); SELECT index_name,table_name,is_unique,is_primary,expressions,sql FROM duckdb_indexes() WHERE database_name='p' AND index_name='source_s_idx'", None),
    ('index-schema-expression', "CREATE INDEX extra_idx ON p.extra.extra_table ((i+1)); SELECT index_name,schema_name,table_name FROM duckdb_indexes() WHERE database_name='p' AND index_name='extra_idx'; DROP INDEX p.extra.extra_idx; SELECT count(*) FROM duckdb_indexes() WHERE database_name='p' AND index_name='extra_idx'", None),
    ('index-case', 'CREATE TABLE p."MixedTable"("MixedColumn" INTEGER); CREATE INDEX "MixedIndex" ON p.mixedtable("MixedColumn"); SELECT index_name,table_name FROM duckdb_indexes() WHERE database_name=\'p\' AND index_name=\'MixedIndex\'', 'mixedcolumn'),
    ('case-distinct-tables', 'CREATE TABLE p."twins"(i INTEGER DEFAULT 1); CREATE TABLE p."TWINS"(i INTEGER DEFAULT 2); INSERT INTO p."twins" VALUES (DEFAULT); INSERT INTO p."TWINS" VALUES (DEFAULT); SELECT * FROM p."twins"; SELECT * FROM p."TWINS"; DROP TABLE p."twins"; SELECT * FROM p."TWINS"', None),
    ('index-rollback', "BEGIN; CREATE INDEX source_idx ON p.source(s); ROLLBACK; CALL pg_clear_cache(); SELECT count(*) FROM duckdb_indexes() WHERE database_name='p' AND index_name='source_idx'", None),
    ('existing-index', "CREATE INDEX source_idx ON p.source(s); CREATE INDEX IF NOT EXISTS source_idx ON p.source(s)", None),
    ('view-alias', "USE p; CREATE VIEW extra.v(a,b) AS SELECT id,s FROM public.source WHERE id<=2; SELECT * FROM extra.v ORDER BY a; CREATE OR REPLACE VIEW extra.v(a) AS SELECT 99; SELECT * FROM extra.v; DROP VIEW extra.v", None),
    ('view-rollback', "BEGIN; CREATE VIEW p.aborted AS SELECT 42 AS i; ROLLBACK; CALL pg_clear_cache(); SELECT count(*) FROM duckdb_tables() WHERE database_name='p' AND table_name='aborted'", None),
    ('checkpoint-permission', 'CHECKPOINT p', 'pg_checkpoint'),
    ('retained-definitions', "CREATE TABLE p.defaults(i INTEGER DEFAULT 42); CREATE UNIQUE INDEX default_idx ON p.defaults(i); CREATE VIEW p.another AS SELECT 1 AS i; SELECT column_default FROM duckdb_columns() WHERE database_name='p' AND table_name='defaults'; SELECT is_unique,expressions FROM duckdb_indexes() WHERE database_name='p' AND index_name='default_idx'", None),
    ('created-array-setting', "SET pg_array_as_varchar=true; CREATE TABLE p.arrays(a INTEGER[]); INSERT INTO p.arrays VALUES ([1,NULL,3]); SELECT typeof(a),a FROM p.arrays; CALL pg_clear_cache(); SELECT typeof(a),a FROM p.arrays", None),
    ('created-json', "CREATE TABLE p.js AS SELECT '{\"a\":42}'::JSON AS j; SELECT typeof(j),j FROM p.js; CALL pg_clear_cache(); SELECT typeof(j),j FROM p.js", None),
    ('internal-table-metadata', "SELECT internal FROM duckdb_tables() WHERE database_name='p' AND schema_name='pg_catalog' AND table_name='pg_class'", None),
]

if __name__ == '__main__':
    attach_control.main()
