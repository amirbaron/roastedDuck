#define _GNU_SOURCE
#include "adbc.h"
#include <assert.h>
#include <dlfcn.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

extern AdbcStatusCode duckdb_adbc_init(int version, void *driver, struct AdbcError *error);

static void ok(AdbcStatusCode status, struct AdbcError *error) {
    if (status != ADBC_STATUS_OK) {
        fprintf(stderr, "ADBC status %d: %s\n", status, error->message ? error->message : "no message");
        abort();
    }
}

int main(void) {
    Dl_info image;
    assert(dladdr((void *)duckdb_adbc_init, &image) && image.dli_fname);
    printf("selected_library=%s\n", image.dli_fname);
    const int versions[] = {ADBC_VERSION_1_0_0, ADBC_VERSION_1_1_0};
    for (size_t i = 0; i < sizeof(versions) / sizeof(versions[0]); i++) {
        struct AdbcDriver driver = {0};
        struct AdbcDatabase database = {0};
        struct AdbcConnection connection = {0};
        struct AdbcStatement statement = {0};
        struct AdbcError error = ADBC_ERROR_INIT;
        struct ArrowArrayStream stream = {0};
        struct ArrowSchema schema = {0};
        struct ArrowArray array = {0}, end = {0};
        int64_t affected = -1;
        ok(duckdb_adbc_init(versions[i], &driver, &error), &error);
        ok(driver.DatabaseNew(&database, &error), &error);
        ok(driver.DatabaseInit(&database, &error), &error);
        ok(driver.ConnectionNew(&connection, &error), &error);
        ok(driver.ConnectionInit(&connection, &database, &error), &error);
        ok(driver.StatementNew(&connection, &statement, &error), &error);
        ok(driver.StatementSetSqlQuery(&statement, "SELECT 42::BIGINT AS answer, 'duck'::VARCHAR AS label", &error), &error);
        ok(driver.StatementExecuteQuery(&statement, &stream, &affected, &error), &error);
        assert(stream.release && stream.get_schema(&stream, &schema) == 0);
        assert(stream.get_next(&stream, &array) == 0 && array.release);
        assert(stream.get_next(&stream, &end) == 0 && end.release == NULL);
        stream.release(&stream);
        assert(stream.release == NULL);
        // Arrow's schema and batches have independent ownership after stream release.
        assert(schema.release && schema.n_children == 2 && strcmp(schema.format, "+s") == 0);
        assert(strcmp(schema.children[0]->format, "l") == 0 && strcmp(schema.children[0]->name, "answer") == 0);
        assert(strcmp(schema.children[1]->format, "u") == 0 && strcmp(schema.children[1]->name, "label") == 0);
        assert(array.length == 1 && array.n_children == 2);
        assert(((const int64_t *)array.children[0]->buffers[1])[array.children[0]->offset] == 42);
        const int32_t *offsets = array.children[1]->buffers[1];
        int64_t offset = array.children[1]->offset;
        assert(offsets[offset + 1] - offsets[offset] == 4);
        assert(memcmp((const char *)array.children[1]->buffers[2] + offsets[offset], "duck", 4) == 0);
        array.release(&array);
        schema.release(&schema);
        assert(array.release == NULL && schema.release == NULL);
        ok(driver.StatementRelease(&statement, &error), &error);
        ok(driver.ConnectionRelease(&connection, &error), &error);
        ok(driver.DatabaseRelease(&database, &error), &error);
        if (driver.release) ok(driver.release(&driver, &error), &error);
        if (error.release) error.release(&error);
    }
    puts("public_adbc_pass");
    return 0;
}
