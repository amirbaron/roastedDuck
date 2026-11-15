//! Disposable-server acceptance of leases, reset, saturation and maintenance.
use roastedduck_common::{Error, ErrorKind, Result};
use roastedduck_postgres::{
    Driver, Session,
    codec::{Codec, Options},
    config::Parameters,
    pool::{AcquireMode, Config, Pool},
};
use roastedduck_types::Value;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::{Duration, Instant};

fn scalar(session: &Session, sql: &str) -> Result<Value> {
    let mut rows = session.stream(sql, &[])?;
    let column = rows
        .columns
        .first()
        .ok_or_else(|| Error::internal("Probe query returned no column"))?;
    let codec = Codec::for_query(&column.data_type, column.type_modifier, Options::default())?;
    let batch = rows.next_batch(2)?;
    let value = batch
        .first()
        .and_then(|r| r.first())
        .ok_or_else(|| Error::internal("Probe query returned no row"))?;
    codec.decode(value.as_deref())
}
fn pid(session: &Session) -> Result<i32> {
    scalar(session, "SELECT pg_backend_pid()")?
        .as_i32()
        .ok_or_else(|| Error::internal("Invalid backend PID"))
}
fn make_pool(config: Config) -> Result<Arc<Pool>> {
    let driver = Driver::new()?;
    let parameters = Parameters::parse("dbname=postgresscanner")?;
    Pool::new(config, Arc::new(move || driver.connect(&parameters)))
}
fn check(condition: bool, label: &str) -> Result<()> {
    if condition {
        println!("PASS {label}");
        Ok(())
    } else {
        Err(Error::internal(format!("pool probe failed: {label}")))
    }
}
fn main() -> Result<()> {
    let pool = make_pool(Config {
        max_connections: 1,
        reaper: false,
        ..Config::default()
    })?;
    let first = pool.acquire(None)?;
    let first_pid = pid(first.session()?)?;
    first.session()?.batch("CREATE TEMP TABLE pool_temp(i INTEGER); SET application_name='pool-dirty'; CREATE SCHEMA pool_probe; CREATE TYPE pool_probe.mood AS ENUM ('a','b')")?;
    scalar(first.session()?, "SELECT 'a'::pool_probe.mood")?;
    drop(first);
    let second = pool.acquire(None)?;
    check(pid(second.session()?)? == first_pid, "session-reuse")?;
    check(
        scalar(
            second.session()?,
            "SELECT current_setting('application_name')='' AND to_regclass('pg_temp.pool_temp') IS NULL",
        )?
        .as_bool()
            == Some(true),
        "discard-clears-session-state",
    )?;
    scalar(second.session()?, "SELECT 'b'::pool_probe.mood")?;
    check(pool.snapshot().1.cache_misses == 1, "prepared-type-cache-reset")?;
    check(pool.try_acquire()?.is_none(), "try-saturation")?;
    check(pool.snapshot().1.try_failures == 1, "try-failure-counter")?;
    let excess = pool.acquire(None)?;
    check(pid(excess.session()?)? != first_pid, "force-exceeds-limit")?;
    drop(second);
    drop(excess);
    check(
        pool.snapshot().1.total_connections == 1 && pool.snapshot().1.available_connections == 1,
        "force-return-trims-cache",
    )?;
    pool.update(|c| {
        c.acquire_mode = AcquireMode::Wait;
        c.wait_timeout_millis = 2000;
    });
    let held = pool.acquire(None)?;
    let held_pid = pid(held.session()?)?;
    let (send, recv) = mpsc::channel();
    let waiting = pool.clone();
    let waiter = std::thread::spawn(move || -> Result<(i32, Duration)> {
        let start = Instant::now();
        send.send(())
            .map_err(|_| Error::internal("Probe synchronization failed"))?;
        let lease = waiting.acquire(None)?;
        Ok((pid(lease.session()?)?, start.elapsed()))
    });
    recv.recv()
        .map_err(|_| Error::internal("Probe synchronization failed"))?;
    std::thread::sleep(Duration::from_millis(60));
    drop(held);
    let (resumed, elapsed) = waiter
        .join()
        .map_err(|_| Error::internal("Probe waiter panicked"))??;
    check(
        resumed == held_pid && elapsed >= Duration::from_millis(50),
        "wait-wakes-on-return",
    )?;
    pool.update(|c| c.wait_timeout_millis = 70);
    let held = pool.acquire(None)?;
    let start = Instant::now();
    check(
        pool.acquire(None)
            .is_err_and(|e| e.to_string().contains("waited 70ms"))
            && start.elapsed() >= Duration::from_millis(60),
        "wait-timeout",
    )?;
    pool.update(|c| c.wait_timeout_millis = 5000);
    let flag = Arc::new(AtomicBool::new(false));
    let interrupt = flag.clone();
    let waiting = pool.clone();
    let waiter = std::thread::spawn(move || {
        waiting
            .acquire(Some(&interrupt))
            .map(|_| ())
            .map_err(|e| e.kind())
    });
    std::thread::sleep(Duration::from_millis(60));
    flag.store(true, Ordering::Relaxed);
    check(
        waiter
            .join()
            .map_err(|_| Error::internal("Probe waiter panicked"))?
            == Err(ErrorKind::Interrupt),
        "wait-interruption",
    )?;
    drop(held);
    pool.set_max_connections(0);
    let uncached = pool.acquire(None)?;
    let p = pid(uncached.session()?)?;
    drop(uncached);
    let uncached = pool.acquire(None)?;
    check(pid(uncached.session()?)? != p, "disabled-pool-reconnects")?;
    drop(uncached);
    check(
        pool.snapshot().1.total_connections == 0 && pool.snapshot().1.available_connections == 0,
        "disabled-pool-holds-no-sessions",
    )?;
    pool.set_max_connections(1);
    pool.update(|c| {
        c.acquire_mode = AcquireMode::Force;
        c.health_check_query = "SELECT nonexistent_probe_column".into();
    });
    let lease = pool.acquire(None)?;
    drop(lease);
    check(
        pool.snapshot().1.available_connections == 0,
        "unhealthy-return-discarded",
    )?;
    pool.update(|c| c.health_check_query = "SELECT 1".into());
    let lease = pool.acquire(None)?;
    let old = pid(lease.session()?)?;
    drop(lease);
    let killer = Driver::new()?.connect(&Parameters::parse("dbname=postgresscanner")?)?;
    scalar(&killer, &format!("SELECT pg_terminate_backend({old})"))?;
    let replacement = pool.acquire(None)?;
    check(pid(replacement.session()?)? != old, "dead-idle-session-replaced")?;
    drop(replacement);
    pool.update(|c| c.health_check_query = String::new());
    let failed = pool.acquire(None)?;
    let failed_pid = pid(failed.session()?)?;
    failed.session()?.batch("BEGIN")?;
    check(
        failed
            .session()?
            .batch("SELECT nonexistent_probe_column")
            .is_err(),
        "failed-transaction-fixture",
    )?;
    drop(failed);
    let recovered = pool.acquire(None)?;
    check(
        pid(recovered.session()?)? == failed_pid
            && scalar(recovered.session()?, "SELECT 42::INTEGER")?.as_i32() == Some(42),
        "failed-transaction-reset-with-health-disabled",
    )?;
    drop(recovered);
    pool.update(|c| c.health_check_query = "SELECT 1".into());
    pool.update(|c| c.idle_timeout_millis = 30);
    let held = pool.acquire(None)?;
    let old = pid(held.session()?)?;
    drop(held);
    std::thread::sleep(Duration::from_millis(50));
    let next = pool.acquire(None)?;
    check(pid(next.session()?)? != old, "idle-expiry")?;
    drop(next);
    pool.update(|c| {
        c.idle_timeout_millis = 60000;
        c.max_lifetime_millis = 30;
    });
    let held = pool.acquire(None)?;
    std::thread::sleep(Duration::from_millis(50));
    drop(held);
    check(
        pool.snapshot().1.available_connections == 0,
        "lifetime-expiry-on-return",
    )?;
    let tls = make_pool(Config {
        max_connections: 2,
        thread_local_cache: true,
        reaper: false,
        ..Config::default()
    })?;
    let lease = tls.acquire(None)?;
    let p = pid(lease.session()?)?;
    drop(lease);
    check(
        tls.snapshot().1.available_connections == 0 && tls.snapshot().1.total_connections == 1,
        "thread-local-pins-session",
    )?;
    let lease = tls.acquire(None)?;
    check(
        pid(lease.session()?)? == p && tls.snapshot().1.thread_local_cache_hits == 1,
        "thread-local-reuse",
    )?;
    drop(lease);
    tls.set_max_connections(0);
    let lease = tls.acquire(None)?;
    check(
        pid(lease.session()?)? == p,
        "disabled-pool-can-drain-thread-local-cache",
    )?;
    drop(lease);
    check(tls.snapshot().1.total_connections == 0, "thread-local-limit-trim")?;
    drop(tls);
    let returned = make_pool(Config {
        max_connections: 2,
        thread_local_cache: true,
        reaper: false,
        ..Config::default()
    })?;
    let worker_pool = returned.clone();
    std::thread::spawn(move || -> Result<()> {
        drop(worker_pool.acquire(None)?);
        Ok(())
    })
    .join()
    .map_err(|_| Error::internal("Thread-local worker panicked"))??;
    check(
        returned.snapshot().1.available_connections == 1,
        "thread-exit-returns-cached-session",
    )?;
    returned.shutdown();
    drop(returned);
    let driver = Driver::new()?;
    let parameters = Parameters::parse("dbname=postgresscanner")?;
    let slow = Pool::new(
        Config {
            max_lifetime_millis: 20,
            reaper: false,
            ..Config::default()
        },
        Arc::new(move || {
            std::thread::sleep(Duration::from_millis(40));
            driver.connect(&parameters)
        }),
    )?;
    drop(slow.acquire(None)?);
    check(
        slow.snapshot().1.total_connections == 0,
        "lifetime-includes-connect-time",
    )?;
    slow.shutdown();
    drop(slow);
    let reaper = make_pool(Config {
        max_connections: 2,
        idle_timeout_millis: 1000,
        reaper: true,
        ..Config::default()
    })?;
    drop(reaper.acquire(None)?);
    let deadline = Instant::now() + Duration::from_secs(4);
    while reaper.snapshot().1.available_connections != 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    check(
        reaper.snapshot().1.available_connections == 0 && reaper.snapshot().1.reaper_running,
        "background-idle-reaper",
    )?;
    reaper.set_reaper(false)?;
    check(!reaper.snapshot().1.reaper_running, "reaper-stops")?;
    reaper.set_reaper(true)?;
    check(reaper.snapshot().1.reaper_running, "reaper-restarts")?;
    reaper.shutdown();
    check(reaper.acquire(None).is_err(), "shutdown-rejects-acquisition")?;
    pool.shutdown();
    drop(pool);
    drop(reaper);
    drop(killer);
    println!("pool-probe-ok");
    Ok(())
}
