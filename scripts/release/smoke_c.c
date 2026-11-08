#include <stdio.h>
#include "duckdb.h"

int main(void) {
    duckdb_database db;
    duckdb_connection con;
    duckdb_result result;
    if (duckdb_open(NULL, &db) != DuckDBSuccess) return 1;
    if (duckdb_connect(db, &con) != DuckDBSuccess) {
        duckdb_close(&db);
        return 1;
    }
    int status = duckdb_query(con, "SELECT 42", &result);
    int passed = status == DuckDBSuccess && duckdb_value_int64(&result, 0, 0) == 42;
    duckdb_destroy_result(&result);
    duckdb_disconnect(&con);
    duckdb_close(&db);
    if (passed) puts("C API preview smoke passed");
    return passed ? 0 : 1;
}
