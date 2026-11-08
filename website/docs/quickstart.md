# Your first queries

Start the [installed shell](install.md) with a database filename:

```sh
./bin/roastedDuck example.duckdb
```

Create a tiny dataset and aggregate it:

```sql
CREATE TABLE sales (category VARCHAR, amount INTEGER);
INSERT INTO sales VALUES ('books', 20), ('coffee', 8), ('books', 15);

SELECT category, sum(amount) AS total
FROM sales
GROUP BY category
ORDER BY total DESC;
```

The totals are `books: 35` and `coffee: 8`. With a database filename, committed
changes persist when you close and reopen the shell. Without a filename, the
database is in memory. End statements with a semicolon; use Ctrl+D to exit.

## Read a local file

```sql
COPY sales TO 'sales.parquet' (FORMAT PARQUET);
SELECT * FROM 'sales.parquet' ORDER BY category, amount;
```

CSV and Parquet readers, joins, aggregates, and window functions are part of
the compatibility target. SQL can access files and configured network resources;
run untrusted SQL inside an appropriate application or operating-system sandbox.

## Run a query from the command line

```sh
./bin/roastedDuck -csv -c 'SELECT i, i * i AS square FROM range(5) t(i);'
```

## Check before migrating

Work on a copy of an existing database during evaluation. The candidate has
recorded bidirectional persistence/WAL tests against the pinned reference, but
that is not a promise of compatibility with every database or future DuckDB
version. Read the [compatibility contract](compatibility.md).
