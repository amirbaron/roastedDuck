//! API interruption while a real PostgreSQL server is inside `pg_sleep`.
use roastedduck_postgres::{Driver, config::Parameters};
use std::time::{Duration, Instant};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "legacy".into());
    let driver = Driver::new()?;
    let observer = driver.connect(&Parameters::parse("dbname=postgresscanner")?)?;
    let mut config = roastedduck::Config::default();
    if let Ok(directory) = std::env::var("ROASTEDDUCK_TEST_SECRET_DIRECTORY") {
        config = config.with_option("secret_directory", roastedduck::Value::varchar(directory));
    }
    let database = roastedduck::Database::open(":memory:", &config)?;
    let mut connection = database.connect()?;
    connection.query("LOAD postgres")?;
    if mode != "legacy" {
        connection.query(
            "ATTACH 'dbname=postgresscanner application_name=roastedduck-scan-interrupt' AS p(TYPE postgres)",
        )?;
    }
    let handle = connection.client_handle();
    std::thread::scope(|scope| -> Result<(), Box<dyn std::error::Error>> {
        let watcher = scope.spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                let mut rows = observer.stream("SELECT count(*)::int4 FROM pg_stat_activity WHERE application_name='roastedduck-scan-interrupt' AND wait_event='PgSleep'", &[]).map_err(|e| e.to_string())?;
                let batch = rows.next_batch(1).map_err(|e| e.to_string())?;
                if batch[0][0] == Some(1i32.to_be_bytes().to_vec()) { break; }
                if Instant::now() >= deadline { handle.interrupt(); return Err("PostgreSQL scan never entered pg_sleep".to_owned()); }
                std::thread::sleep(Duration::from_millis(20));
            }
            handle.interrupt();
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let mut rows = observer.stream("SELECT count(*)::int4 FROM pg_stat_activity WHERE application_name='roastedduck-scan-interrupt' AND wait_event='PgSleep'", &[]).map_err(|e| e.to_string())?;
                let batch = rows.next_batch(1).map_err(|e| e.to_string())?;
                if batch[0][0] == Some(0i32.to_be_bytes().to_vec()) { break; }
                if Instant::now() >= deadline { return Err("PostgreSQL query kept sleeping after interruption".to_owned()); }
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok::<_, String>(())
        });
        let sql = match mode.as_str() {
            "legacy" => {
                "SELECT * FROM postgres_scan('dbname=postgresscanner application_name=roastedduck-scan-interrupt','public','slow_scan')"
            }
            "attached" => "SELECT * FROM p.slow_scan",
            "insert" => "INSERT INTO p.slow_insert VALUES (1)",
            "update" => "UPDATE p.slow_mutation SET value=2 WHERE id=1",
            "delete" => "DELETE FROM p.slow_mutation WHERE id=1",
            "merge" => {
                "MERGE INTO p.slow_mutation t USING (VALUES(1,2)) s(id,value) USING(id) WHEN MATCHED THEN UPDATE SET value=s.value"
            }
            _ => return Err("unknown interrupt probe mode".into()),
        };
        let error = connection
            .query(sql)
            .err()
            .ok_or("query succeeded despite interruption")?;
        watcher.join().map_err(|_| "interrupt watcher panicked")??;
        if error.kind() != roastedduck_common::ErrorKind::Interrupt {
            return Err(format!("wrong error kind: {error}").into());
        }
        connection.query("SELECT 42")?;
        let check = driver.connect(&Parameters::parse("dbname=postgresscanner")?)?;
        if mode == "insert" {
            let mut rows = check.stream("SELECT count(*)::int4 FROM slow_insert", &[])?;
            if rows.next_batch(1)? != vec![vec![Some(0i32.to_be_bytes().to_vec())]] {
                return Err("cancelled COPY committed a row".into());
            }
        }
        if matches!(mode.as_str(), "update" | "delete" | "merge") {
            let mut rows = check.stream("SELECT id::int4,value::int4 FROM slow_mutation", &[])?;
            if rows.next_batch(2)?
                != vec![vec![
                    Some(1i32.to_be_bytes().to_vec()),
                    Some(1i32.to_be_bytes().to_vec()),
                ]]
            {
                return Err("cancelled mutation changed the persisted row".into());
            }
        }
        let mut rows=check.stream("SELECT count(*)::int4 FROM pg_stat_activity WHERE application_name='roastedduck-scan-interrupt' AND state LIKE 'idle in transaction%'",&[])?;
        if rows.next_batch(1)? != vec![vec![Some(0i32.to_be_bytes().to_vec())]] {
            return Err("interruption left an open remote transaction".into());
        }
        println!("interrupt-ok");
        Ok(())
    })
}
