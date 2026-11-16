//! Development acceptance of the Rust wire backend against an owned PostgreSQL cluster.
use roastedduck_postgres::{BinaryParameter, Driver, config::Parameters};
use std::sync::Arc;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let parameters = Parameters::parse("dbname=postgresscanner")?;
    let driver = Driver::new()?;
    for mode in [
        "disable",
        "allow",
        "prefer",
        "require",
        "verify-ca",
        "verify-full",
    ] {
        let mut options = parameters.clone();
        options.set("sslmode", mode);
        let connected = driver.connect(&options)?;
        let mut rows = connected.stream("SELECT ssl FROM pg_stat_ssl WHERE pid=pg_backend_pid()", &[])?;
        let expected = u8::from(!matches!(mode, "disable" | "allow"));
        if rows.next_batch(1)? != vec![vec![Some(vec![expected])]] {
            return Err(format!("wrong TLS mode behavior: {mode}").into());
        }
    }
    let mut host = parameters.clone();
    host.set("host", "wrong.invalid");
    host.set("hostaddr", "127.0.0.1");
    host.set("sslmode", "verify-ca");
    driver.connect(&host)?.batch("SELECT 1")?;
    host.set("sslmode", "verify-full");
    if driver.connect(&host).is_ok() {
        return Err("verify-full accepted the wrong hostname".into());
    }
    let mut invalid = parameters.clone();
    invalid.set("password", "incorrect-fixture-password");
    if !driver
        .connect(&invalid)
        .is_err_and(|e| e.to_string().contains("password authentication failed"))
    {
        return Err("wrong-password control failed".into());
    }
    let session = Arc::new(driver.connect(&parameters)?);
    let mut security=session.stream("SELECT current_user::text,current_setting('is_superuser'),ssl FROM pg_stat_ssl WHERE pid=pg_backend_pid()",&[])?;
    if security.next_batch(1)?
        != vec![vec![
            Some(b"postgres".to_vec()),
            Some(b"off".to_vec()),
            Some(vec![1]),
        ]]
    {
        return Err("TLS or test-role privilege mismatch".into());
    }
    let columns =
        session.describe("SELECT 42::int4 AS value, NULL::text AS missing, 12.34::numeric(8,2) AS amount")?;
    if columns.len() != 3 || columns[0].data_type.oid() != 23 || columns[2].type_modifier < 0 {
        return Err("column description mismatch".into());
    }
    let mut rows = session.stream(
        "SELECT $1::int4, NULL::text",
        &[BinaryParameter(Some(42i32.to_be_bytes().to_vec()))],
    )?;
    let batch = rows.next_batch(8)?;
    if batch != vec![vec![Some(42i32.to_be_bytes().to_vec()), None]] {
        return Err("binary row mismatch".into());
    }
    let mismatch = session.stream(
        "SELECT $1::int4",
        &[BinaryParameter(Some(42i64.to_be_bytes().to_vec()))],
    );
    let rejected = match mismatch {
        Err(e) => e.to_string().contains("binary data"),
        Ok(mut rows) => rows.next_batch(1).is_err(),
    };
    if !rejected {
        return Err("server did not reject mismatched binary parameter".into());
    }
    session.batch("BEGIN; CREATE TEMP TABLE wire_probe(value INTEGER, label TEXT);")?;
    let mut bytes = b"PGCOPY\n\xff\r\n\0".to_vec();
    bytes.extend_from_slice(&[0; 8]);
    for (number, label) in [(1, "first"), (2, "second")] {
        bytes.extend_from_slice(&2i16.to_be_bytes());
        bytes.extend_from_slice(&4i32.to_be_bytes());
        bytes.extend_from_slice(&i32::to_be_bytes(number));
        bytes.extend_from_slice(&(label.len() as i32).to_be_bytes());
        bytes.extend_from_slice(label.as_bytes());
    }
    bytes.extend_from_slice(&(-1i16).to_be_bytes());
    let expected = bytes.clone();
    let mut copy = session.copy_in("COPY wire_probe FROM STDIN (FORMAT binary)")?;
    copy.write(bytes.into())?;
    if copy.finish()? != 2 {
        return Err("COPY row count mismatch".into());
    }
    let mut copied = session
        .copy_out("COPY (SELECT value,label FROM wire_probe ORDER BY value) TO STDOUT (FORMAT binary)")?;
    let mut output = Vec::new();
    while let Some(chunk) = copied.next_chunk()? {
        output.extend_from_slice(&chunk);
    }
    if copied.next_chunk()?.is_some() || copied.next_chunk()?.is_some() {
        return Err("COPY stream resumed after EOF".into());
    }
    if output != expected {
        return Err("COPY output bytes mismatch".into());
    }
    session.batch("ROLLBACK")?;
    let mut result = session.stream("SELECT to_regclass('pg_temp.wire_probe')", &[])?;
    if result.next_batch(1)? != vec![vec![None]] {
        return Err("rollback failed".into());
    }
    let active = session.clone();
    let worker = std::thread::spawn(move || active.batch("SELECT pg_sleep(20)"));
    std::thread::sleep(std::time::Duration::from_millis(150));
    session.cancel()?;
    let canceled = worker.join().map_err(|_| "query thread panicked")?;
    if !canceled.is_err_and(|e| e.to_string().contains("canceling statement")) {
        return Err("cancel failed".into());
    }
    let outside = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    outside.block_on(async { session.batch("SELECT 1") })?;
    outside.block_on(async {
        let nested = Driver::new()?;
        let client = nested.connect(&parameters)?;
        client.batch("SELECT 1")?;
        drop(client);
        drop(nested);
        Ok::<_, roastedduck_common::Error>(())
    })?;
    println!("wire-probe-ok");
    Ok(())
}
