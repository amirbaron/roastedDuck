-- The C++ side of `benches/casts.rs`: the same 10M-row inputs, cast single-threaded.
-- Each benchmark runs twice; the second timing is the one reported.
-- Run: duckdb < crates/function/benches/casts_duckdb.sql
SET threads=1;
SET TimeZone='UTC';
CREATE TABLE ints AS SELECT (i * 7919 % 2000000000)::INTEGER AS x FROM range(10000000) t(i);
CREATE TABLE doubles AS SELECT x / 1000 AS x FROM ints;
CREATE TABLE int_text AS SELECT x::VARCHAR AS x FROM ints;
CREATE TABLE double_text AS SELECT x::VARCHAR AS x FROM doubles;
CREATE TABLE date_text AS SELECT ('1970-01-01'::DATE + (i % 100000)::INTEGER)::VARCHAR AS x FROM range(10000000) t(i);
CREATE TABLE timestamps AS SELECT '1970-01-01'::TIMESTAMP + to_microseconds(i * 7919 * 1000) AS x FROM range(10000000) t(i);

SELECT 'bench varchar_to_integer';
.timer on
SELECT count(CAST(x AS INTEGER)) FROM int_text;
SELECT count(CAST(x AS INTEGER)) FROM int_text;
.timer off

SELECT 'bench varchar_to_double';
.timer on
SELECT count(CAST(x AS DOUBLE)) FROM double_text;
SELECT count(CAST(x AS DOUBLE)) FROM double_text;
.timer off

SELECT 'bench integer_to_varchar';
.timer on
SELECT count(CAST(x AS VARCHAR)) FROM ints;
SELECT count(CAST(x AS VARCHAR)) FROM ints;
.timer off

SELECT 'bench double_to_varchar';
.timer on
SELECT count(CAST(x AS VARCHAR)) FROM doubles;
SELECT count(CAST(x AS VARCHAR)) FROM doubles;
.timer off

SELECT 'bench varchar_to_date';
.timer on
SELECT count(CAST(x AS DATE)) FROM date_text;
SELECT count(CAST(x AS DATE)) FROM date_text;
.timer off

SELECT 'bench timestamp_to_varchar';
.timer on
SELECT count(CAST(x AS VARCHAR)) FROM timestamps;
SELECT count(CAST(x AS VARCHAR)) FROM timestamps;
.timer off
