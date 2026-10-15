// C ABI between the Rust sqllogictest runner (tools/slt) and the C++ DuckDB 1.5.6 oracle.
//
// The surface is tailored to the runner: every function corresponds to one step of DuckDB's own
// `test/sqlite` runner (SQLLogicTestRunner / TestResultHelper) that needs engine internals.
//
// Conventions:
// - Input strings are passed as (pointer, length) pairs and need not be NUL-terminated.
// - `char **error` out-parameters receive a malloc'd message on failure; free it with
//   `slt_string_free`. They are left untouched on success.
// - Strings returned by accessors are owned by the handle they come from.
// - Handles are not thread-safe, except that different connections may be used from
//   different threads concurrently.
#ifndef SLT_ORACLE_H
#define SLT_ORACLE_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define SLT_ORACLE_ABI_VERSION 1

typedef struct slt_config slt_config;   // duckdb::DBConfig
typedef struct slt_db slt_db;           // duckdb::DuckDB
typedef struct slt_conn slt_conn;       // duckdb::Connection
typedef struct slt_result slt_result;   // a materialized result plus its rendered values
typedef struct slt_session slt_session; // client state carried across `restart`

// Outcome of `slt_db_load_extension` (duckdb::ExtensionLoadResult, plus an error case).
#define SLT_EXTENSION_LOADED 0
#define SLT_EXTENSION_UNKNOWN 1
#define SLT_EXTENSION_NOT_LOADED 2
#define SLT_EXTENSION_ERROR -1

// Column classes used by the runner's value comparison.
#define SLT_COLUMN_OTHER 0
#define SLT_COLUMN_NUMERIC 1
#define SLT_COLUMN_BOOLEAN 2

// Settings `require` inspects on the runner's configuration.
#define SLT_SETTING_DEFAULT_BLOCK_SIZE 0
#define SLT_SETTING_AUTOLOAD_KNOWN_EXTENSIONS 1
#define SLT_SETTING_ALLOW_UNSIGNED_EXTENSIONS 2
#define SLT_SETTING_VACUUM_REBUILD_INDEXES 3

int32_t slt_abi_version(void);
// DUCKDB_BUILD_DIRECTORY of the DuckDB build the shim was compiled against.
const char *slt_build_directory(void);
void slt_string_free(char *str);

// The runner's database configuration: GetTestConfig() plus `load_extensions = false`.
slt_config *slt_config_new(char **error);
void slt_config_free(slt_config *config);
bool slt_config_set_option(slt_config *config, const char *name, size_t name_len, const char *value,
                           size_t value_len, char **error);
// `load ... readonly|readwrite`: access mode plus use_temporary_directory.
void slt_config_set_read_only(slt_config *config, bool read_only);
bool slt_config_set_storage_version(slt_config *config, const char *version, size_t version_len, char **error);
void slt_config_inherit_storage_version(slt_config *config, const slt_db *db);
uint64_t slt_config_setting(const slt_config *config, int32_t setting);

slt_db *slt_db_open(const char *path, size_t path_len, const slt_config *config, char **error);
void slt_db_free(slt_db *db);
int32_t slt_db_load_extension(slt_db *db, const char *name, size_t name_len, char **error);

slt_conn *slt_conn_new(slt_db *db, char **error);
void slt_conn_free(slt_conn *conn);
// enable_progress_bar = true, print_progress_bar = false (SQLLogicTestRunner::Reconnect).
void slt_conn_set_test_client_options(slt_conn *conn);

// RestartCommand: copies the running database's options and user settings into `config` and
// captures the main connection's client config, search path and query log path.
slt_session *slt_session_save(slt_conn *main_conn, slt_config *config);
bool slt_session_restore(slt_conn *main_conn, const slt_session *session, char **error);
void slt_session_free(slt_session *session);

// Runs `sql` force-materialized; never returns NULL.
slt_result *slt_query(slt_conn *conn, const char *sql, size_t sql_len);
void slt_result_free(slt_result *result);
// The error message (ErrorData::Message), or NULL on success.
const char *slt_result_error(const slt_result *result);
uint64_t slt_result_column_count(const slt_result *result);
uint64_t slt_result_row_count(const slt_result *result);
const char *slt_result_column_name(const slt_result *result, uint64_t column);
const char *slt_result_column_type(const slt_result *result, uint64_t column);
int32_t slt_result_column_class(const slt_result *result, uint64_t column);

// TestResultHelper::DuckDBConvertResult using `context` for casts. On success the values are
// available row-major through `slt_result_value`.
bool slt_result_render(slt_result *result, slt_conn *context, bool original_sqlite_test, char **error);
uint64_t slt_result_value_count(const slt_result *result);
const char *slt_result_value(const slt_result *result, uint64_t index, size_t *len);

// The numeric branch of TestResultHelper::CompareValues: casts both strings to the column type
// with `context` and compares them with Value::ValuesAreEqual.
bool slt_values_equal(slt_conn *context, const slt_result *result, uint64_t column, const char *actual,
                      size_t actual_len, const char *expected, size_t expected_len);

// TestResultHelper::LoadResultFromFile. Returns a result whose rendered values and column count
// hold the file contents, or an error result.
slt_result *slt_load_result_file(slt_conn *context, const char *path, size_t path_len, const char *const *names,
                                 const size_t *name_lens, uint64_t name_count);

#ifdef __cplusplus
}
#endif

#endif // SLT_ORACLE_H
