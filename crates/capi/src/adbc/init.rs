//! Versioned ADBC initializer; writes only callback slots present in the requested prefix.
use super::*;
/// DuckDB 1.5.6 ADBC 1.0/1.1 driver entry point.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_adbc_init(
    version: c_int,
    driver: *mut std::ffi::c_void,
    _error: *mut AdbcError,
) -> u8 {
    if driver.is_null() {
        return INVALID_ARGUMENT;
    }
    if version != 1_000_000 && version != 1_001_000 {
        return NOT_IMPLEMENTED;
    }
    let driver = driver.cast::<AdbcDriver>();
    // SAFETY: ADBC supplies a writable driver prefix for the negotiated version. Each
    // callback is assigned individually; no full struct/reference accesses the 1.1 tail
    // in a 1.0 allocation. Unassigned private/release/Substrait slots remain untouched,
    // exactly as in the frozen C++ initializer.
    unsafe {
        (*driver).DatabaseNew = Some(database_new);
        (*driver).DatabaseSetOption = Some(database_set_option);
        (*driver).DatabaseInit = Some(database_init);
        (*driver).DatabaseRelease = Some(database_release);
        (*driver).ConnectionNew = Some(connection_new);
        (*driver).ConnectionSetOption = Some(connection_set_option);
        (*driver).ConnectionInit = Some(connection_init);
        (*driver).ConnectionRelease = Some(connection_release);
        (*driver).ConnectionGetTableTypes = Some(metadata::get_table_types);
        (*driver).StatementNew = Some(statement::new);
        (*driver).StatementRelease = Some(statement::release);
        (*driver).StatementBind = Some(statement::bind);
        (*driver).StatementBindStream = Some(statement::bind_stream);
        (*driver).StatementExecuteQuery = Some(statement::execute);
        (*driver).StatementPrepare = Some(statement::prepare);
        (*driver).StatementSetOption = Some(options::statement_set);
        (*driver).StatementSetSqlQuery = Some(statement::set_sql);
        (*driver).ConnectionGetObjects = Some(metadata::get_objects);
        (*driver).ConnectionCommit = Some(connection_commit);
        (*driver).ConnectionRollback = Some(connection_rollback);
        (*driver).ConnectionReadPartition = Some(options::read_partition);
        (*driver).StatementExecutePartitions = Some(options::execute_partitions);
        (*driver).ConnectionGetInfo = Some(info::get_info);
        (*driver).StatementGetParameterSchema = Some(statement::parameter_schema);
        (*driver).ConnectionGetTableSchema = Some(metadata::get_table_schema);
        if version >= 1_001_000 {
            (*driver).ErrorGetDetailCount = Some(errors::count);
            (*driver).ErrorGetDetail = Some(errors::detail);
            (*driver).ErrorFromArrayStream = Some(stream::error_from_stream);
            (*driver).DatabaseGetOption = Some(database_get_option);
            (*driver).DatabaseGetOptionBytes = Some(options::database_get_bytes);
            (*driver).DatabaseGetOptionDouble = Some(options::database_get_double);
            (*driver).DatabaseGetOptionInt = Some(options::database_get_int);
            (*driver).DatabaseSetOptionBytes = Some(options::database_set_bytes);
            (*driver).DatabaseSetOptionInt = Some(database_set_int);
            (*driver).DatabaseSetOptionDouble = Some(database_set_double);
            (*driver).ConnectionCancel = Some(connection_cancel);
            (*driver).ConnectionGetOption = Some(connection_get_option);
            (*driver).ConnectionGetOptionBytes = Some(options::connection_get_bytes);
            (*driver).ConnectionGetOptionDouble = Some(options::connection_get_double);
            (*driver).ConnectionGetOptionInt = Some(connection_get_int);
            (*driver).ConnectionGetStatistics = Some(statistics::get_statistics);
            (*driver).ConnectionGetStatisticNames = Some(statistics::get_names);
            (*driver).ConnectionSetOptionBytes = Some(options::connection_set_bytes);
            (*driver).ConnectionSetOptionInt = Some(connection_set_int);
            (*driver).ConnectionSetOptionDouble = Some(options::connection_set_double);
            (*driver).StatementCancel = Some(statement::cancel);
            (*driver).StatementExecuteSchema = Some(statement::execute_schema);
            (*driver).StatementGetOption = Some(options::statement_get);
            (*driver).StatementGetOptionBytes = Some(options::statement_get_bytes);
            (*driver).StatementGetOptionDouble = Some(options::statement_get_double);
            (*driver).StatementGetOptionInt = Some(options::statement_get_int);
            (*driver).StatementSetOptionBytes = Some(options::statement_set_bytes);
            (*driver).StatementSetOptionDouble = Some(options::statement_set_double);
            (*driver).StatementSetOptionInt = Some(options::statement_set_int);
        }
    }
    OK
}
