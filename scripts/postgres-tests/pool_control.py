#!/usr/bin/env python3
"""Paired SQL controls for real pool reuse, reset and live/global settings."""
import attach_control

attach_control.CASES = [
    ('pid-reuse', "CREATE TEMP TABLE seen AS SELECT pid FROM postgres_query('p','SELECT pg_backend_pid() AS pid'); SELECT pid=(SELECT pid FROM seen) FROM postgres_query('p','SELECT pg_backend_pid() AS pid')", None),
    ('reset-state', "CALL postgres_execute('p','SET application_name=''owned-pool-marker''; CREATE TEMP TABLE pool_marker(i INTEGER)'); SELECT * FROM postgres_query('p','SELECT current_setting(''application_name'')<>''owned-pool-marker'' AS setting_reset,to_regclass(''pg_temp.pool_marker'') IS NULL AS temp_removed')", None),
    ('transaction-retains-state', "BEGIN; CALL postgres_execute('p','CREATE TEMP TABLE pool_marker(i INTEGER); INSERT INTO pool_marker VALUES(42)'); SELECT * FROM postgres_query('p','SELECT * FROM pool_marker'); COMMIT; SELECT * FROM postgres_query('p','SELECT to_regclass(''pg_temp.pool_marker'') IS NULL')", None),
    ('disabled-reconnects', "SELECT catalog_name FROM postgres_configure_pool(catalog_name='p',max_connections=0); CREATE TEMP TABLE seen AS SELECT pid FROM postgres_query('p','SELECT pg_backend_pid() AS pid'); SELECT pid<>(SELECT pid FROM seen) FROM postgres_query('p','SELECT pg_backend_pid() AS pid'); SELECT available_connections FROM postgres_configure_pool(catalog_name='p')", None),
    ('new-setting-future-only', "SELECT max_connections FROM postgres_configure_pool(catalog_name='p',max_connections=2); SET pg_pool_max_connections=7; ATTACH 'dbname=postgresscanner' AS q(TYPE postgres); SELECT catalog_name,max_connections FROM postgres_configure_pool() ORDER BY catalog_name", None),
    ('legacy-limit-updates-pools', "ATTACH 'dbname=postgresscanner' AS q(TYPE postgres); SET pg_connection_limit=3; SELECT catalog_name,max_connections FROM postgres_configure_pool() ORDER BY catalog_name; SELECT current_setting('pg_pool_max_connections'),current_setting('pg_connection_limit')", None),
    ('legacy-cache', "SET pg_connection_cache=false; SELECT max_connections,available_connections FROM postgres_configure_pool(catalog_name='p'); SET pg_connection_cache=true; SELECT max_connections>0 FROM postgres_configure_pool(catalog_name='p')", None),
    ('bad-health-check', "SELECT health_check_query FROM postgres_configure_pool(catalog_name='p',health_check_query='SELECT nonexistent_pool_column'); SELECT count(*) FROM p.source; SELECT available_connections FROM postgres_configure_pool(catalog_name='p'); SELECT health_check_query FROM postgres_configure_pool(catalog_name='p',health_check_query='SELECT 42'); SELECT count(*) FROM p.source; SELECT available_connections FROM postgres_configure_pool(catalog_name='p')", None),
    ('command-health-check', "SELECT health_check_query FROM postgres_configure_pool(catalog_name='p',health_check_query='SET application_name=''pool-check'''); SELECT count(*) FROM p.source; SELECT available_connections FROM postgres_configure_pool(catalog_name='p')", None),
    ('custom-types-after-reset', "CREATE TYPE p.pool_mood AS ENUM ('a','b'); CREATE TABLE p.pool_types(m p.pool_mood); INSERT INTO p.pool_types VALUES('a'),('b'); SELECT * FROM p.pool_types ORDER BY m; SELECT * FROM p.pool_types ORDER BY m", None),
    ('null-options-ignored', "SELECT catalog_name,max_connections,health_check_query FROM postgres_configure_pool(catalog_name='p',max_connections=4,health_check_query=''); SELECT catalog_name,max_connections,health_check_query FROM postgres_configure_pool(catalog_name='p',max_connections=NULL,health_check_query=NULL)", None),
    ('mode-case', "SELECT acquire_mode FROM postgres_configure_pool(catalog_name='p',acquire_mode='WaIt')", None),
    ('mode-invalid', "SELECT * FROM postgres_configure_pool(catalog_name='p',acquire_mode='invalid')", "Invalid unsupported acquire mode: 'invalid'"),
    ('catalog-required', "SELECT * FROM postgres_configure_pool(max_connections=2)", "'catalog_name' argument must be specified"),
    ('catalog-exact-case', "SELECT * FROM postgres_configure_pool(catalog_name='P')", "Catalog not found, name: 'P'"),
    ('zero-wait-validation', "SET pg_pool_max_connections=0; SET pg_pool_acquire_mode='wait'", 'requires pg_pool_max_connections > 0'),
]

if __name__=='__main__': attach_control.main()
