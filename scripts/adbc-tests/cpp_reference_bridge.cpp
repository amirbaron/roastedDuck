// Reference-only helper: the original upstream test accesses these same C++ internals.
#include "duckdb/common/adbc/adbc.hpp"
#include "duckdb/common/adbc/wrappers.hpp"
#include "duckdb/main/connection.hpp"
#include "duckdb/main/client_context.hpp"
extern "C" duckdb_connection adbc_cpp_test_connection(AdbcConnection *connection){return static_cast<duckdb::DuckDBAdbcConnectionWrapper *>(connection->private_data)->connection;}
extern "C" duckdb_connection adbc_cpp_test_connect(AdbcConnection *connection){auto native=reinterpret_cast<duckdb::Connection *>(adbc_cpp_test_connection(connection));return reinterpret_cast<duckdb_connection>(new duckdb::Connection(*native->context->db));}
extern "C" void adbc_cpp_test_initialize_error(AdbcError *error){duckdb_adbc::InitializeADBCError(error);}
