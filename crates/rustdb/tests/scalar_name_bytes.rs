//! Scalar catalog identities and overload diagnostics must preserve every name byte.
use rustdb::{Database, ErrorKind, LogicalType, Result};
use rustdb_function::{ScalarFunction, ScalarFunctionSet};

fn identity(name: &[u8]) -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new(name);
    set.add(ScalarFunction::new(
        name,
        vec![LogicalType::BIGINT],
        LogicalType::BIGINT,
        |input, _, output| {
            *output = input.column(0).clone();
            Ok(())
        },
    ));
    set
}

#[test]
fn raw_scalar_name_executes_and_stays_in_overload_diagnostics() -> Result<()> {
    let mut connection = Database::open_in_memory()?.connect()?;
    let name = b"scalar_\xFF";
    connection.register_scalar_function_set(identity(name))?;
    let result = connection.query(b"SELECT \"scalar_\xFF\"(41::BIGINT)")?;
    assert_eq!(result.value(0, 0).as_i64(), Some(41));
    let error = connection
        .query(b"SELECT \"scalar_\xFF\"([1,2])")
        .expect_err("no list overload");
    assert_eq!(error.kind(), ErrorKind::Binder);
    assert!(error.to_bytes().windows(name.len()).any(|part| part == name));
    assert!(
        error
            .extra_info()
            .iter()
            .any(|(key, value)| *key == "name" && *value == name)
    );
    connection.remove_scalar_function(name)?;
    assert_eq!(
        connection
            .query(b"SELECT \"scalar_\xFF\"(41::BIGINT)")
            .expect_err("removed")
            .kind(),
        ErrorKind::Catalog
    );
    Ok(())
}

#[test]
fn nul_scalar_name_survives_catalog_scan_and_removal() -> Result<()> {
    let mut connection = Database::open_in_memory()?.connect()?;
    let name = b"scalar_\0name";
    connection.register_scalar_function_set(identity(name))?;
    let exists = |result: &rustdb::MaterializedResult| {
        (0..result.row_count()).any(|row| result.value(0, row).as_bytes() == Some(name.as_slice()))
    };
    assert!(exists(
        &connection.query("SELECT function_name FROM duckdb_functions()")?
    ));
    connection.remove_scalar_function(name)?;
    assert!(!exists(
        &connection.query("SELECT function_name FROM duckdb_functions()")?
    ));
    Ok(())
}
