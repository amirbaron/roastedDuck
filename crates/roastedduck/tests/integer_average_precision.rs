//! Wide integer averages retain the native C++ finalization precision.
#![allow(clippy::unwrap_used, reason = "Regression queries return typed doubles")]

use roastedduck::{Database, Result};

fn wide_precision() -> bool {
    cfg!(all(target_os = "linux", target_arch = "aarch64"))
        || cfg!(all(
            target_arch = "x86_64",
            any(target_os = "linux", target_vendor = "apple")
        ))
}

fn expected_average() -> f64 {
    if wide_precision() {
        3_002_399_751_580_331.0
    } else {
        3_002_399_751_580_330.5
    }
}

#[test]
fn average_mean_and_list_average_keep_low_integer_bits() -> Result<()> {
    let mut connection = Database::open_in_memory()?.connect()?;
    let result = connection.query(
        "SELECT avg(n), mean(n) FROM \
         (VALUES (9007199254740992::HUGEINT), (1), (0)) t(n)",
    )?;
    for column in 0..2 {
        assert_eq!(
            result.value(column, 0).as_f64().unwrap().to_bits(),
            expected_average().to_bits()
        );
    }
    let result = connection.query("SELECT list_avg([9007199254740992,1,0]::HUGEINT[])")?;
    assert_eq!(
        result.value(0, 0).as_f64().unwrap().to_bits(),
        expected_average().to_bits()
    );
    Ok(())
}

#[test]
fn grouped_and_window_integer_averages_use_the_same_finalization() -> Result<()> {
    let mut connection = Database::open_in_memory()?.connect()?;
    let source = "(VALUES (1,9007199254740992::HUGEINT), (1,1), (1,0), \
                  (2,-9007199254740992::HUGEINT), (2,-1), (2,0)) t(g,n)";
    let grouped = connection.query(format!("SELECT avg(n) FROM {source} GROUP BY g ORDER BY g"))?;
    assert_eq!(
        grouped.value(0, 0).as_f64().unwrap().to_bits(),
        expected_average().to_bits()
    );
    assert_eq!(
        grouped.value(0, 1).as_f64().unwrap().to_bits(),
        (-expected_average()).to_bits()
    );
    let window = connection.query(format!(
        "SELECT avg(n) OVER (PARTITION BY g) FROM {source} ORDER BY g,n"
    ))?;
    for row in 0..6 {
        let expected = if row < 3 {
            expected_average()
        } else {
            -expected_average()
        };
        assert_eq!(
            window.value(0, row).as_f64().unwrap().to_bits(),
            expected.to_bits()
        );
    }
    Ok(())
}

#[test]
fn decimal_average_preserves_the_scaled_dividend() -> Result<()> {
    let mut connection = Database::open_in_memory()?.connect()?;
    let result = connection.query(
        "SELECT avg(n) FROM (VALUES ('9007199.254740992'::DECIMAL(38,9)), \
         ('0.000000001'::DECIMAL(38,9)), ('0'::DECIMAL(38,9))) t(n)",
    )?;
    let expected: f64 = if wide_precision() {
        3_002_399.751_580_331
    } else {
        9_007_199_254_740_992.0 / 3_000_000_000.0
    };
    assert_eq!(result.value(0, 0).as_f64().unwrap().to_bits(), expected.to_bits());
    Ok(())
}

#[test]
fn zero_and_empty_integer_averages_preserve_null_and_zero() -> Result<()> {
    let mut connection = Database::open_in_memory()?.connect()?;
    let zero = connection.query("SELECT avg(n) FROM (VALUES (1::HUGEINT),(-1)) t(n)")?;
    assert_eq!(zero.value(0, 0).as_f64().unwrap().to_bits(), 0);
    let empty = connection.query("SELECT avg(n) FROM (SELECT 1::HUGEINT n WHERE false)")?;
    assert!(empty.value(0, 0).is_null());
    let list = connection.query("SELECT list_avg([NULL,NULL]::HUGEINT[])")?;
    assert!(list.value(0, 0).is_null());
    Ok(())
}
