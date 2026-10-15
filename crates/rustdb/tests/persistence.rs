//! Database files: data written by checkpoints is there after reopening.
#![allow(clippy::unwrap_used)]

use rustdb::{Config, Connection, Database};

/// A fresh database path in the temporary directory.
fn temp_path(name: &str) -> String {
    let dir = std::env::temp_dir().join(format!("rustdb-persistence-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.db"));
    let _ = std::fs::remove_file(&path);
    path.to_string_lossy().into_owned()
}

fn open(path: &str) -> (Database, Connection) {
    let database = Database::open(path, &Config::default()).unwrap();
    let connection = database.connect().unwrap();
    (database, connection)
}

fn rows(connection: &mut Connection, sql: &str) -> Vec<Vec<String>> {
    let result = connection.query(sql).unwrap();
    (0..result.row_count())
        .map(|row| {
            (0..result.column_count())
                .map(|column| {
                    let value = result.value(column, row);
                    if value.is_null() {
                        "NULL".to_owned()
                    } else {
                        value.to_varchar().unwrap()
                    }
                })
                .collect()
        })
        .collect()
}

#[test]
fn tables_survive_a_restart() {
    let path = temp_path("tables");
    let big = "x".repeat(10_000);
    {
        let (_database, mut con) = open(&path);
        con.query("CREATE TABLE t(i INTEGER PRIMARY KEY, s VARCHAR, d DOUBLE, b BOOLEAN)")
            .unwrap();
        con.query(&format!(
            "INSERT INTO t VALUES (1, 'a', 1.5, true), (2, NULL, NULL, false), (3, '{big}', -2.0, NULL)"
        ))
        .unwrap();
        con.query("CREATE SCHEMA s").unwrap();
        con.query("CREATE TABLE s.u(x BIGINT)").unwrap();
        con.query("INSERT INTO s.u SELECT * FROM range(5000)").unwrap();
        con.query("DELETE FROM s.u WHERE x > 10 AND x < 4990").unwrap();
        con.query("CREATE VIEW v AS SELECT i FROM t WHERE i > 1").unwrap();
        con.query("CREATE SEQUENCE seq START 10").unwrap();
        con.query("SELECT nextval('seq')").unwrap();
    }
    {
        let (_database, mut con) = open(&path);
        assert_eq!(
            rows(&mut con, "SELECT i, s IS NULL, d, b FROM t ORDER BY i"),
            vec![
                vec!["1", "false", "1.5", "true"],
                vec!["2", "true", "NULL", "false"],
                vec!["3", "false", "-2.0", "NULL"],
            ]
        );
        assert_eq!(
            rows(&mut con, "SELECT s FROM t WHERE i = 3"),
            vec![vec![big.clone()]]
        );
        let remaining: Vec<Vec<String>> = (0..=10).chain(4990..5000).map(|x| vec![x.to_string()]).collect();
        assert_eq!(rows(&mut con, "SELECT x FROM s.u ORDER BY x"), remaining);
        assert_eq!(
            rows(&mut con, "SELECT * FROM v ORDER BY i"),
            vec![vec!["2"], vec!["3"]]
        );
        assert_eq!(rows(&mut con, "SELECT nextval('seq')"), vec![vec!["11"]]);
        // The primary key is enforced on reloaded data.
        assert!(con.query("INSERT INTO t VALUES (1, 'dup', 0, false)").is_err());
        con.query("INSERT INTO t VALUES (4, 'd', 4.0, true)").unwrap();
        con.query("CHECKPOINT").unwrap();
        con.query("DROP TABLE s.u").unwrap();
    }
    {
        let (_database, mut con) = open(&path);
        assert_eq!(
            rows(&mut con, "SELECT i FROM t ORDER BY i"),
            vec![vec!["1"], vec!["2"], vec!["3"], vec!["4"]]
        );
        assert!(con.query("SELECT * FROM s.u").is_err());
    }
}

/// Opens `path` with checkpoints on shutdown disabled, so changes stay in the WAL.
fn open_without_checkpoint(path: &str) -> (Database, Connection) {
    let (database, mut con) = open(path);
    con.query("PRAGMA disable_checkpoint_on_shutdown").unwrap();
    (database, con)
}

fn wal_size(path: &str) -> u64 {
    std::fs::metadata(format!("{path}.wal")).map_or(0, |m| m.len())
}

#[test]
fn committed_changes_are_replayed_from_the_wal() {
    let path = temp_path("wal-replay");
    {
        let (_database, mut con) = open_without_checkpoint(&path);
        con.query("CREATE SCHEMA s").unwrap();
        con.query("CREATE TABLE s.t(i INTEGER PRIMARY KEY, v VARCHAR, d DOUBLE)")
            .unwrap();
        con.query("INSERT INTO s.t SELECT range, 'value', range FROM range(3000)")
            .unwrap();
        con.query("DELETE FROM s.t WHERE i < 100").unwrap();
        con.query("UPDATE s.t SET v = NULL, d = -1 WHERE i > 2997")
            .unwrap();
        con.query("CREATE VIEW vv AS SELECT i FROM s.t WHERE i > 2997")
            .unwrap();
        con.query("CREATE SEQUENCE seq START 5").unwrap();
        con.query("SELECT nextval('seq')").unwrap();
        con.query("CREATE TABLE u(a INTEGER, b INTEGER)").unwrap();
        con.query("INSERT INTO u VALUES (1, 2), (3, 4)").unwrap();
        con.query("ALTER TABLE u ADD COLUMN c VARCHAR DEFAULT 'x'")
            .unwrap();
        con.query("ALTER TABLE u RENAME COLUMN a TO aa").unwrap();
        con.query("ALTER TABLE u RENAME TO uu").unwrap();
        con.query("CREATE INDEX idx ON uu(b)").unwrap();
        con.query("CREATE TABLE gone(x INTEGER)").unwrap();
        con.query("DROP TABLE gone").unwrap();
        // Uncommitted work never reaches the WAL.
        con.query("BEGIN").unwrap();
        con.query("INSERT INTO uu VALUES (5, 6, 'y')").unwrap();
    }
    assert!(wal_size(&path) > 0);
    {
        let (_database, mut con) = open_without_checkpoint(&path);
        assert_eq!(
            rows(
                &mut con,
                "SELECT i, v, d FROM s.t WHERE i > 2996 OR i < 101 ORDER BY i"
            ),
            vec![
                vec!["100", "value", "100.0"],
                vec!["2997", "value", "2997.0"],
                vec!["2998", "NULL", "-1.0"],
                vec!["2999", "NULL", "-1.0"],
            ]
        );
        assert_eq!(
            rows(&mut con, "SELECT * FROM vv ORDER BY i"),
            vec![vec!["2998"], vec!["2999"]]
        );
        assert_eq!(rows(&mut con, "SELECT nextval('seq')"), vec![vec!["6"]]);
        assert_eq!(
            rows(&mut con, "SELECT * FROM uu ORDER BY aa"),
            vec![vec!["1", "2", "x"], vec!["3", "4", "x"]]
        );
        assert!(con.query("SELECT * FROM gone").is_err());
        assert!(con.query("INSERT INTO s.t VALUES (2999, 'dup', 0)").is_err());
        // More changes go to the replayed WAL.
        con.query("INSERT INTO uu VALUES (7, 8, 'z')").unwrap();
    }
    {
        let (_database, mut con) = open(&path);
        assert_eq!(
            rows(&mut con, "SELECT aa FROM uu ORDER BY aa"),
            vec![vec!["1"], vec!["3"], vec!["7"]]
        );
        assert_eq!(rows(&mut con, "SELECT nextval('seq')"), vec![vec!["7"]]);
    }
    // Closing with a checkpoint removed the WAL.
    assert_eq!(wal_size(&path), 0);
    let (_database, mut con) = open(&path);
    assert_eq!(
        rows(&mut con, "SELECT aa FROM uu ORDER BY aa"),
        vec![vec!["1"], vec!["3"], vec!["7"]]
    );
}

#[test]
fn a_torn_wal_tail_is_ignored() {
    let path = temp_path("wal-torn");
    {
        let (_database, mut con) = open_without_checkpoint(&path);
        con.query("CREATE TABLE t(i INTEGER)").unwrap();
        con.query("INSERT INTO t VALUES (1)").unwrap();
    }
    let complete = wal_size(&path);
    {
        let (_database, mut con) = open_without_checkpoint(&path);
        con.query("INSERT INTO t VALUES (2)").unwrap();
    }
    // Cut the last commit in half, as a crash during its write would.
    let full = wal_size(&path);
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(format!("{path}.wal"))
        .unwrap();
    file.set_len(complete + (full - complete) / 2).unwrap();
    drop(file);
    {
        let (_database, mut con) = open_without_checkpoint(&path);
        assert_eq!(rows(&mut con, "SELECT i FROM t"), vec![vec!["1"]]);
        // The torn tail is cut off before the next commit is appended.
        con.query("INSERT INTO t VALUES (3)").unwrap();
    }
    let (_database, mut con) = open(&path);
    assert_eq!(
        rows(&mut con, "SELECT i FROM t ORDER BY i"),
        vec![vec!["1"], vec!["3"]]
    );
}

#[test]
fn checkpoint_empties_the_wal() {
    let path = temp_path("wal-checkpoint");
    let (_database, mut con) = open_without_checkpoint(&path);
    con.query("CREATE TABLE t(i INTEGER)").unwrap();
    con.query("INSERT INTO t VALUES (1)").unwrap();
    assert!(wal_size(&path) > 0);
    con.query("CHECKPOINT").unwrap();
    assert_eq!(wal_size(&path), 0);
    con.query("INSERT INTO t VALUES (2)").unwrap();
    assert!(wal_size(&path) > 0);
}
