-- roastedDuck's compatible HTTPFS implementation is bundled in this source build.
-- LOAD is sufficient; INSTALL is an offline acknowledgement.
INSTALL httpfs;
LOAD httpfs;

-- Configure a scoped S3 secret before accessing private objects, for example:
-- CREATE SECRET storage (TYPE s3, KEY_ID '...', SECRET '...', REGION 'us-east-1');
-- SELECT * FROM read_parquet('s3://your-bucket/data/*.parquet');
-- Automatic AWS credential-chain support is a separate implementation milestone.

SELECT extension_name, installed, loaded
FROM duckdb_extensions()
WHERE extension_name = 'httpfs';
