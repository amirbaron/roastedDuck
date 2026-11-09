#!/bin/bash
# Creates the benchmark databases of scripts/perf/bench.py with DuckDB's CLI, in
# $ROASTEDDUCK_PERF_DIR (default .context/perf). Both engines read the same files.
#
# usage: scripts/perf/data.sh tpch SF          TPC-H, CALL dbgen(sf=SF) -> tpchSF.duckdb
#        scripts/perf/data.sh tpcds SF         TPC-DS, CALL dsdgen(sf=SF) -> tpcdsSF.duckdb
#        scripts/perf/data.sh h2o              h2oai G1_1e7_1e2_5_0 (groupby) and J1_1e7_* (join)
#                                              -> h2o_g.duckdb, h2o_j.duckdb (DuckDB's benchmark data)
#        scripts/perf/data.sh hits [N]         the first N (default 3) of ClickBench's 100 hits
#                                              partitions -> hits.duckdb (N million rows)
# Downloads are deleted once loaded.
set -euo pipefail
SELF="$(cd "$(dirname "$0")" && pwd)/$(basename "$0")"
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
DIR="${ROASTEDDUCK_PERF_DIR:-${RUSTDB_PERF_DIR:-$ROOT/.context/perf}}"
REF="${DUCKDB_SRC:-$ROOT/.context/duckdb-download/duckdb-1.5.6}"
DUCK="$REF/build/release/duckdb"
mkdir -p "$DIR/dl"
cd "$DIR"

case "${1:-}" in
tpch)
  "$DUCK" "tpch$2.duckdb" -c "CALL dbgen(sf=$2)" > /dev/null
  ;;
tpcds)
  "$DUCK" "tpcds$2.duckdb" -c "CALL dsdgen(sf=$2)" > /dev/null
  ;;
h2o)
  BASE=https://github.com/duckdb/duckdb-data/releases/download/v1.0
  for f in G1_1e7_1e2_5_0 J1_1e7_NA_0_0 J1_1e7_1e1_0_0 J1_1e7_1e4_0_0 J1_1e7_1e7_0_0; do
    [ -f "dl/$f.csv.gz" ] || curl -sfL -o "dl/$f.csv.gz" "$BASE/$f.csv.gz"
  done
  "$DUCK" h2o_g.duckdb -c "CREATE TABLE x_group AS SELECT * FROM read_csv_auto('dl/G1_1e7_1e2_5_0.csv.gz');"
  "$DUCK" h2o_j.duckdb -c "CREATE TABLE x AS SELECT * FROM read_csv_auto('dl/J1_1e7_NA_0_0.csv.gz');
    CREATE TABLE small AS SELECT * FROM read_csv_auto('dl/J1_1e7_1e1_0_0.csv.gz');
    CREATE TABLE medium AS SELECT * FROM read_csv_auto('dl/J1_1e7_1e4_0_0.csv.gz');
    CREATE TABLE big AS SELECT * FROM read_csv_auto('dl/J1_1e7_1e7_0_0.csv.gz');"
  rm -f dl/*.csv.gz
  ;;
hits)
  N="${2:-3}"
  # DuckDB's benchmark/clickbench/queries/load.sql, one local partition at a time.
  sed -n '1,/^);/p' "$REF/benchmark/clickbench/queries/load.sql" | "$DUCK" hits.duckdb
  for i in $(seq 0 $((N - 1))); do
    curl -sfL -o dl/hits.parquet "https://datasets.clickhouse.com/hits_compatible/athena_partitioned/hits_$i.parquet"
    "$DUCK" hits.duckdb -c "INSERT INTO hits BY NAME SELECT * REPLACE (make_date(EventDate) AS EventDate,
      epoch_ms(EventTime * 1000) AS EventTime, epoch_ms(ClientEventTime * 1000) AS ClientEventTime,
      epoch_ms(LocalEventTime * 1000) AS LocalEventTime)
      FROM read_parquet('dl/hits.parquet', binary_as_string=True);"
    rm -f dl/hits.parquet
  done
  "$DUCK" hits.duckdb -c "CHECKPOINT"
  ;;
*)
  sed -n '2,11p' "$SELF"
  exit 1
  ;;
esac
ls -la "$DIR"/*.duckdb
