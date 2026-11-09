/* ABI smoke test: compiled against DuckDB's own duckdb.h, linked against roastedduck's libduckdb. */
#include "duckdb.h"
#include <stdio.h>
#include <string.h>

int main(void) {
	duckdb_database db;
	duckdb_connection con;
	duckdb_result result;
	if (duckdb_open(NULL, &db) != DuckDBSuccess) { puts("open failed"); return 1; }
	if (duckdb_connect(db, &con) != DuckDBSuccess) { puts("connect failed"); return 1; }
	duckdb_state state = duckdb_query(con, "SELECT 42 AS answer, 'hello' AS greeting", &result);
	if (state == DuckDBSuccess) {
		char *answer = duckdb_value_varchar(&result, 0, 0);
		printf("%s=%s rows=%llu cols=%llu type=%d\n", duckdb_column_name(&result, 0), answer,
		       (unsigned long long)duckdb_row_count(&result), (unsigned long long)duckdb_column_count(&result),
		       (int)duckdb_column_type(&result, 0));
		duckdb_free(answer);
	} else {
		printf("query error: %s\n", duckdb_result_error(&result));
	}
	duckdb_destroy_result(&result);
	duckdb_disconnect(&con);
	duckdb_close(&db);
	printf("version %s\n", duckdb_library_version());
	return state == DuckDBSuccess ? 0 : 2;
}
