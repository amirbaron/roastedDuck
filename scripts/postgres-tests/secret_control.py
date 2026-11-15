#!/usr/bin/env python3
"""Paired PostgreSQL config-secret and transactional storage behavior."""
import attach_control

STORE = "ATTACH 'dbname=postgresscanner' AS vault(TYPE POSTGRES,SECRET_STORAGE_TABLE duckdb_secrets);"
SECRET = "CREATE SECRET remote IN postgres_vault(TYPE HTTP,BEARER_TOKEN 'fixture-token',VERIFY_SSL false);"
COUNT = "SELECT count(*) FROM duckdb_secrets() WHERE name='remote';"
attach_control.CASES = [
    ('aliases-scope-and-redaction', "CREATE SECRET cfg(TYPE POSTGRES,HOSTNAME 'host',USERNAME 'name',DATABASE 'db',PORT 5432,PASSWORD 'fixture-secret',SSLPASSWORD 'fixture-ssl',OAUTH_CLIENT_SECRET 'fixture-oauth',SCOPE 'ignored'); SELECT type,provider,scope,secret_string FROM duckdb_secrets() WHERE name='cfg'", None),
    ('alias-collision-one', "CREATE SECRET cfg(TYPE POSTGRES,DBNAME 'one',DATABASE 'two'); SELECT secret_string FROM duckdb_secrets() WHERE name='cfg'", None),
    ('alias-collision-two', "CREATE SECRET cfg(TYPE POSTGRES,DATABASE 'two',DBNAME 'one'); SELECT secret_string FROM duckdb_secrets() WHERE name='cfg'", None),
    ('null-option-is-string', "CREATE SECRET cfg(TYPE POSTGRES,APPLICATION_NAME NULL); ATTACH 'dbname=postgresscanner' AS q(TYPE POSTGRES,SECRET cfg); SELECT * FROM postgres_query('q','SELECT current_setting(''application_name'')')", None),
    ('path-overrides-config', "CREATE SECRET cfg(TYPE POSTGRES,DBNAME 'not_a_database',APPLICATION_NAME 'configured'); ATTACH 'dbname=postgresscanner application_name=override' AS q(TYPE POSTGRES,SECRET cfg); SELECT * FROM postgres_query('q','SELECT current_setting(''application_name'')')", None),
    ('default-secret', "CREATE SECRET(TYPE POSTGRES,DBNAME 'postgresscanner',APPLICATION_NAME 'default-secret'); ATTACH '' AS q(TYPE POSTGRES); SELECT * FROM postgres_query('q','SELECT current_setting(''application_name'')')", None),
    ('wrong-provider', "CREATE SECRET cfg(TYPE POSTGRES,PROVIDER env)", "Secret provider 'env' not found"),
    ('uri-conflict', "CREATE SECRET cfg(TYPE POSTGRES,URI 'unused',HOST 'also-unused'); ATTACH '' AS q(TYPE POSTGRES,SECRET cfg)", "'URI' option"),
    ('secret-name-ambiguity', "CREATE SECRET cfg(TYPE POSTGRES); CREATE PERSISTENT SECRET cfg(TYPE POSTGRES); ATTACH 'dbname=postgresscanner' AS q(TYPE POSTGRES,SECRET cfg)", 'Ambiguity detected'),
    ('store-rollback-create', STORE + "BEGIN;" + SECRET + COUNT + "ROLLBACK;" + COUNT, None),
    ('store-rollback-drop', STORE + SECRET + "BEGIN; DROP SECRET remote FROM postgres_vault;" + COUNT + "ROLLBACK;" + COUNT, None),
    ('store-replace-and-ignore', STORE + SECRET + "CREATE OR REPLACE SECRET remote IN postgres_vault(TYPE HTTP,VERIFY_SSL true); CREATE SECRET IF NOT EXISTS remote IN postgres_vault(TYPE HTTP,VERIFY_SSL false); SELECT secret_string FROM duckdb_secrets() WHERE name='remote'", None),
    ('store-conflict', STORE + SECRET + SECRET, "already exists in secret storage 'postgres_vault'"),
    ('store-temporary-rejected', STORE + "CREATE TEMPORARY SECRET remote IN postgres_vault(TYPE HTTP)", 'Cannot create temporary secrets in a persistent secret storage'),
    ('store-detach-reattach', STORE + SECRET + "DETACH vault;" + COUNT + "ATTACH 'dbname=postgresscanner' AS vault(TYPE POSTGRES);" + COUNT, None),
    ('store-drop-ambiguity', STORE + SECRET + "CREATE SECRET remote(TYPE HTTP); DROP SECRET remote", 'Ambiguity found'),
    ('store-scope-priority', STORE + "CREATE SECRET remote IN postgres_vault(TYPE HTTP,SCOPE 'http://fixture/long'); CREATE SECRET mem(TYPE HTTP,SCOPE 'http://fixture/'); SELECT * FROM which_secret('http://fixture/long/object','http')", None),
    ('store-tie-priority', STORE + "CREATE SECRET remote IN postgres_vault(TYPE HTTP,SCOPE 'http://fixture/'); CREATE SECRET mem(TYPE HTTP,SCOPE 'http://fixture/'); SELECT * FROM which_secret('http://fixture/object','http')", None),
    ('store-quoted-name', "ATTACH 'dbname=postgresscanner' AS vault(TYPE POSTGRES,SECRET_STORAGE_TABLE '\"Quoted Storage\"');" + SECRET + COUNT, None),
    ('store-default-storage', STORE + "SET default_secret_storage='postgres_vault'; CREATE PERSISTENT SECRET remote(TYPE HTTP);" + COUNT, None),
]
# Alias collisions with enough unrelated keys to exercise map bucket growth.
for count in [1,3,7,15,25]:
    names=['application_name','client_encoding','options','sslmode','connect_timeout','port','passfile','service','keepalives','sslcert','sslkey','sslrootcert','sslcrl','sslcrldir','sslsni','requirepeer','gssencmode','krbsrvname','fallback_application_name','target_session_attrs','load_balance_hosts','oauth_issuer','oauth_client_id','oauth_scope','tcp_user_timeout'][:count]
    padding=','.join(name+" 'fixture'" for name in names)
    for order,aliases in [('a',"DBNAME 'one',DATABASE 'two',HOST 'three',HOSTNAME 'four',USER 'five',USERNAME 'six'"),('b',"DATABASE 'two',DBNAME 'one',HOSTNAME 'four',HOST 'three',USERNAME 'six',USER 'five'")]:
        attach_control.CASES.append((f'alias-buckets-{count}-{order}',"CREATE SECRET cfg(TYPE POSTGRES,"+aliases+','+padding+"); SELECT secret_string FROM duckdb_secrets() WHERE name='cfg'",None))
if __name__ == '__main__':
    attach_control.main()
