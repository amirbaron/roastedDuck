/* Public-C-API regression: materialized results release the connection, while
 * streaming results retain it. Run unchanged against C++ and Rust libduckdb. */
#include "duckdb.h"
#include <assert.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>

static unsigned destroyed;

static void destroy_extra(void *extra) {
    free(extra);
    destroyed++;
}

static void add_seven(duckdb_function_info info, duckdb_data_chunk input, duckdb_vector output) {
    int64_t *extra = duckdb_scalar_function_get_extra_info(info);
    assert(extra && *extra == 7);
    int64_t *source = duckdb_vector_get_data(duckdb_data_chunk_get_vector(input, 0));
    int64_t *target = duckdb_vector_get_data(output);
    for (idx_t i = 0; i < duckdb_data_chunk_get_size(input); i++) target[i] = source[i] + *extra;
}

static void register_function(duckdb_connection connection) {
    duckdb_scalar_function function = duckdb_create_scalar_function();
    duckdb_logical_type type = duckdb_create_logical_type(DUCKDB_TYPE_BIGINT);
    int64_t *extra = malloc(sizeof(*extra));
    assert(extra);
    *extra = 7;
    duckdb_scalar_function_set_name(function, "result_lifetime_add7");
    duckdb_scalar_function_add_parameter(function, type);
    duckdb_scalar_function_set_return_type(function, type);
    duckdb_scalar_function_set_function(function, add_seven);
    duckdb_scalar_function_set_extra_info(function, extra, destroy_extra);
    assert(duckdb_register_scalar_function(connection, function) == DuckDBSuccess);
    duckdb_destroy_logical_type(&type);
    duckdb_destroy_scalar_function(&function);
}

/* mode 0: materialized; mode 1: streaming; mode 2: streaming converted through
 * the deprecated value API; mode 3: a stream destroyed before fetching. */
static void run_case(unsigned mode) {
    destroyed = 0;
    duckdb_database database = NULL;
    duckdb_connection connection = NULL;
    assert(duckdb_open(NULL, &database) == DuckDBSuccess);
    assert(duckdb_connect(database, &connection) == DuckDBSuccess);
    register_function(connection);
    duckdb_prepared_statement statement = NULL;
    assert(duckdb_prepare(connection, "SELECT result_lifetime_add7(i) FROM range(10000)t(i)", &statement) == DuckDBSuccess);
    duckdb_disconnect(&connection);
    duckdb_close(&database);
    assert(destroyed == 0);
    duckdb_result result;
    assert((mode ? duckdb_execute_prepared_streaming(statement, &result) : duckdb_execute_prepared(statement, &result)) == DuckDBSuccess);
    assert(duckdb_result_is_streaming(result) == (mode != 0));
    duckdb_destroy_prepare(&statement);
    if (mode == 0) assert(destroyed == 1);
    else assert(destroyed == 0);
    if (mode == 1) {
        idx_t count = 0;
        duckdb_data_chunk chunk;
        while ((chunk = duckdb_fetch_chunk(result))) {
            int64_t *values = duckdb_vector_get_data(duckdb_data_chunk_get_vector(chunk, 0));
            for (idx_t i = 0; i < duckdb_data_chunk_get_size(chunk); i++) assert(values[i] == (int64_t)(count++ + 7));
            duckdb_destroy_data_chunk(&chunk);
        }
        assert(count == 10000);
    } else if (mode != 3) {
        assert(duckdb_value_int64(&result, 0, 0) == 7);
        assert(duckdb_value_int64(&result, 0, 9999) == 10006);
        assert(destroyed == 1);
    }
    duckdb_destroy_result(&result);
    assert(destroyed == 1);
    printf("PASS mode=%u\n", mode);
}

int main(void) {
    for (unsigned mode = 0; mode < 4; mode++) run_case(mode);
    return 0;
}
