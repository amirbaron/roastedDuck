//! API interruption while a real PostgreSQL server is inside `pg_sleep`.
use roastedduck_postgres::{Driver, config::Parameters};
use std::time::{Duration, Instant};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let driver = Driver::new()?;
    let observer = driver.connect(&Parameters::parse("dbname=postgresscanner")?)?;
    let database = roastedduck::Database::open_in_memory()?;
    let mut connection = database.connect()?;
    connection.query("LOAD postgres")?;
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
        let error = connection.query("SELECT * FROM postgres_scan('dbname=postgresscanner application_name=roastedduck-scan-interrupt','public','slow_scan')")
            .err().ok_or("query succeeded despite interruption")?;
        watcher.join().map_err(|_| "interrupt watcher panicked")??;
        if error.kind() != roastedduck_common::ErrorKind::Interrupt {
            return Err(format!("wrong error kind: {error}").into());
        }
        connection.query("SELECT 42")?;
        println!("interrupt-ok");
        Ok(())
    })
}
