//! CREATE/DROP and INSERT/UPDATE/DELETE end to end, with constraints and transactions.
#![allow(clippy::unwrap_used)]

use rustdb::{Connection, Database};

fn connect() -> Connection {
    Database::open_in_memory().unwrap().connect().unwrap()
}

/// The result rows rendered as strings (NULL as "NULL").
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

fn error(connection: &mut Connection, sql: &str) -> String {
    connection.query(sql).unwrap_err().to_string()
}

#[test]
fn insert_update_delete_with_defaults_and_returning() {
    let mut con = connect();
    con.query("CREATE TABLE t(i INTEGER PRIMARY KEY, j VARCHAR DEFAULT 'x', k INTEGER CHECK (k > 0))")
        .unwrap();
    assert_eq!(
        rows(&mut con, "INSERT INTO t VALUES (1, 'a', 1), (2, 'b', 2)"),
        vec![vec!["2"]]
    );
    con.query("INSERT INTO t(i) VALUES (3)").unwrap();
    con.query("INSERT INTO t(k, i) VALUES (5, 4)").unwrap();
    assert_eq!(
        rows(&mut con, "SELECT * FROM t ORDER BY i"),
        vec![
            vec!["1", "a", "1"],
            vec!["2", "b", "2"],
            vec!["3", "x", "NULL"],
            vec!["4", "x", "5"]
        ]
    );
    assert_eq!(
        rows(&mut con, "UPDATE t SET j = 'z' WHERE i = 2"),
        vec![vec!["1"]]
    );
    con.query("UPDATE t SET k = DEFAULT WHERE i = 1").unwrap();
    assert_eq!(rows(&mut con, "DELETE FROM t WHERE i = 3"), vec![vec!["1"]]);
    assert_eq!(
        rows(&mut con, "SELECT * FROM t ORDER BY i"),
        vec![vec!["1", "a", "NULL"], vec!["2", "z", "2"], vec!["4", "x", "5"]]
    );
    assert_eq!(
        rows(&mut con, "INSERT INTO t VALUES (7, 'r', 7) RETURNING i * 2, j"),
        vec![vec!["14", "r"]]
    );
    assert_eq!(
        rows(&mut con, "DELETE FROM t WHERE i = 7 RETURNING *"),
        vec![vec!["7", "r", "7"]]
    );
    assert_eq!(
        rows(&mut con, "UPDATE t SET i = i + 100 WHERE i < 3 RETURNING i"),
        vec![vec!["101"], vec!["102"]]
    );
}

#[test]
fn constraint_violations_report_duckdb_messages() {
    let mut con = connect();
    con.query("CREATE TABLE t(i INTEGER PRIMARY KEY, k INTEGER CHECK (k > 0))")
        .unwrap();
    con.query("INSERT INTO t VALUES (1, 1)").unwrap();
    assert_eq!(
        error(&mut con, "INSERT INTO t VALUES (1, 1)"),
        "Constraint Error: Duplicate key \"i: 1\" violates primary key constraint."
    );
    assert_eq!(
        error(&mut con, "INSERT INTO t VALUES (2, -1)"),
        "Constraint Error: CHECK constraint failed on table t with expression CHECK((k > 0))"
    );
    assert_eq!(
        error(&mut con, "INSERT INTO t VALUES (NULL, 1)"),
        "Constraint Error: NOT NULL constraint failed: t.i"
    );
    assert_eq!(
        error(&mut con, "UPDATE t SET k = 0"),
        "Constraint Error: CHECK constraint failed on table t with expression CHECK((k > 0))"
    );
    con.query("CREATE TABLE pk(id INT PRIMARY KEY)").unwrap();
    con.query("CREATE TABLE fk(id INT REFERENCES pk(id))").unwrap();
    con.query("INSERT INTO pk VALUES (1)").unwrap();
    con.query("INSERT INTO fk VALUES (1)").unwrap();
    assert_eq!(
        error(&mut con, "INSERT INTO fk VALUES (2)"),
        "Constraint Error: Violates foreign key constraint because key \"id: 2\" does not exist in the referenced table"
    );
    assert!(error(&mut con, "DELETE FROM pk").contains("is still referenced by a foreign key"));
    assert_eq!(
        error(&mut con, "DROP TABLE pk"),
        "Catalog Error: Could not drop the table because this table is main key table of the table \"fk\""
    );
    con.query("DROP TABLE fk").unwrap();
    con.query("DROP TABLE pk").unwrap();
}

#[test]
fn create_table_as_schemas_views_and_drop() {
    let mut con = connect();
    con.query("CREATE TABLE t AS SELECT 1 AS a, 'x' AS a").unwrap();
    assert_eq!(rows(&mut con, "SELECT a, a_1 FROM t"), vec![vec!["1", "x"]]);
    con.query("CREATE SCHEMA s").unwrap();
    con.query("CREATE TABLE s.u(a INT)").unwrap();
    con.query("INSERT INTO s.u VALUES (42)").unwrap();
    assert_eq!(rows(&mut con, "SELECT * FROM s.u"), vec![vec!["42"]]);
    assert!(error(&mut con, "DROP SCHEMA s").starts_with("Dependency Error: Cannot drop entry \"s\""));
    con.query("DROP SCHEMA s CASCADE").unwrap();
    con.query("CREATE VIEW v AS SELECT a FROM t").unwrap();
    assert_eq!(rows(&mut con, "SELECT * FROM v"), vec![vec!["1"]]);
    assert_eq!(
        error(&mut con, "DROP VIEW t"),
        "Catalog Error: Existing object t is of type Table, trying to drop type View"
    );
    con.query("DROP VIEW v").unwrap();
    con.query("DROP TABLE t").unwrap();
    con.query("DROP TABLE IF EXISTS t").unwrap();
    con.query("DROP TABLE IF EXISTS nosuch.t").unwrap();
    assert_eq!(
        error(&mut con, "DROP TABLE nosuch.t"),
        "Catalog Error: Table with name \"nosuch.t\" does not exist because schema \"nosuch\" does not exist."
    );
    con.query("CHECKPOINT").unwrap();
}

#[test]
fn rolled_back_ddl_and_dml_disappear() {
    let mut con = connect();
    con.query("CREATE TABLE t(a INT)").unwrap();
    con.query("BEGIN").unwrap();
    con.query("CREATE TABLE tx(a INT)").unwrap();
    con.query("INSERT INTO t VALUES (1)").unwrap();
    con.query("DROP TABLE t").unwrap();
    con.query("ROLLBACK").unwrap();
    assert!(
        error(&mut con, "SELECT * FROM tx").starts_with("Catalog Error: Table with name tx does not exist!")
    );
    assert_eq!(rows(&mut con, "SELECT * FROM t"), Vec::<Vec<String>>::new());
    con.query("INSERT INTO t VALUES (2)").unwrap();
    assert_eq!(rows(&mut con, "SELECT * FROM t"), vec![vec!["2"]]);
}

#[test]
fn create_and_drop_index() {
    let mut con = connect();
    con.query("CREATE TABLE t(i INT, j INT)").unwrap();
    con.query("INSERT INTO t VALUES (1, 10), (2, 20), (1, 30)")
        .unwrap();
    assert_eq!(
        error(&mut con, "CREATE INDEX idx ON t(rowid)"),
        "Binder Error: cannot create an index on the rowid"
    );
    con.query("CREATE INDEX idx ON t(i)").unwrap();
    assert_eq!(
        error(&mut con, "CREATE INDEX idx ON t(j)"),
        "Catalog Error: Index with name \"idx\" already exists!"
    );
    con.query("CREATE INDEX IF NOT EXISTS idx ON t(j)").unwrap();
    assert_eq!(rows(&mut con, "SELECT j FROM t WHERE i = 2"), vec![vec!["20"]]);
    assert_eq!(
        error(&mut con, "CREATE UNIQUE INDEX u ON t(i)"),
        "Constraint Error: Data contains duplicates on indexed column(s)"
    );
    con.query("CREATE UNIQUE INDEX u ON t((i + j))").unwrap();
    assert!(error(&mut con, "INSERT INTO t VALUES (5, 6)").starts_with("Constraint Error: Duplicate key"));
    // A rolled back index disappears with its constraint.
    con.query("BEGIN").unwrap();
    con.query("DROP INDEX u").unwrap();
    con.query("ROLLBACK").unwrap();
    assert!(con.query("INSERT INTO t VALUES (5, 6)").is_err());
    con.query("DROP INDEX u").unwrap();
    con.query("INSERT INTO t VALUES (5, 6)").unwrap();
    assert_eq!(
        error(&mut con, "DROP INDEX u"),
        "Catalog Error: Index with name u does not exist!"
    );
    // Dropping the table drops its indexes.
    con.query("DROP TABLE t").unwrap();
    con.query("CREATE TABLE t(i INT)").unwrap();
    con.query("CREATE INDEX idx ON t(i)").unwrap();
}

#[test]
fn alter_table() {
    let mut con = connect();
    con.query("CREATE TABLE t(i INT, j INT DEFAULT 3, k VARCHAR)")
        .unwrap();
    con.query("INSERT INTO t VALUES (1, 2, 'a')").unwrap();
    assert_eq!(
        error(&mut con, "ALTER TABLE t RENAME COLUMN i TO j"),
        "Catalog Error: Column with name j already exists!"
    );
    con.query("ALTER TABLE t RENAME COLUMN i TO ii").unwrap();
    con.query("ALTER TABLE t ADD COLUMN l INT DEFAULT 7").unwrap();
    con.query("ALTER TABLE t ADD COLUMN IF NOT EXISTS l INT").unwrap();
    con.query("ALTER TABLE t DROP COLUMN k").unwrap();
    assert_eq!(rows(&mut con, "SELECT * FROM t"), vec![vec!["1", "2", "7"]]);
    con.query("ALTER TABLE t ALTER COLUMN ii SET NOT NULL").unwrap();
    assert_eq!(
        error(&mut con, "INSERT INTO t(j) VALUES (1)"),
        "Constraint Error: NOT NULL constraint failed: t.ii"
    );
    con.query("ALTER TABLE t ALTER COLUMN j TYPE BIGINT USING j * 10")
        .unwrap();
    assert_eq!(rows(&mut con, "SELECT j FROM t"), vec![vec!["20"]]);
    // A rolled back alter leaves the table as it was.
    con.query("BEGIN").unwrap();
    con.query("ALTER TABLE t DROP COLUMN l").unwrap();
    con.query("ALTER TABLE t RENAME TO t2").unwrap();
    con.query("ROLLBACK").unwrap();
    assert_eq!(rows(&mut con, "SELECT * FROM t"), vec![vec!["1", "20", "7"]]);
    con.query("ALTER TABLE t ADD PRIMARY KEY (ii)").unwrap();
    assert_eq!(
        error(&mut con, "INSERT INTO t VALUES (1, 1, 1)"),
        "Constraint Error: Duplicate key \"ii: 1\" violates primary key constraint."
    );
    con.query("CREATE INDEX idx ON t(j)").unwrap();
    assert_eq!(
        error(&mut con, "ALTER TABLE t DROP COLUMN l"),
        "Dependency Error: Cannot alter entry \"t\" because there are entries that depend on it."
    );
}

#[test]
fn sequences() {
    let mut con = connect();
    con.query("CREATE SEQUENCE seq").unwrap();
    assert_eq!(
        error(&mut con, "SELECT currval('seq')"),
        "Sequence Error: currval: sequence is not yet defined in this session"
    );
    assert_eq!(
        rows(&mut con, "SELECT nextval('seq'), nextval('seq')"),
        vec![vec!["1", "2"]]
    );
    assert_eq!(rows(&mut con, "SELECT currval('seq')"), vec![vec!["2"]]);
    con.query("CREATE SEQUENCE down START 10 INCREMENT BY -5 MINVALUE 5 MAXVALUE 10")
        .unwrap();
    assert_eq!(rows(&mut con, "SELECT nextval('down')"), vec![vec!["10"]]);
    assert_eq!(rows(&mut con, "SELECT nextval('down')"), vec![vec!["5"]]);
    assert_eq!(
        error(&mut con, "SELECT nextval('down')"),
        "Sequence Error: nextval: reached minimum value of sequence \"down\" (5)"
    );
    con.query("CREATE TABLE t(id BIGINT DEFAULT nextval('seq'), v VARCHAR)")
        .unwrap();
    con.query("INSERT INTO t(v) VALUES ('a'), ('b')").unwrap();
    assert_eq!(
        rows(&mut con, "SELECT * FROM t"),
        vec![vec!["3", "a"], vec!["4", "b"]]
    );
    // Sequence values are not transactional.
    con.query("BEGIN").unwrap();
    con.query("SELECT nextval('seq')").unwrap();
    con.query("ROLLBACK").unwrap();
    assert_eq!(rows(&mut con, "SELECT nextval('seq')"), vec![vec!["6"]]);
    // The table's DEFAULT depends on the sequence (DuckDB's dependency manager).
    assert!(error(&mut con, "DROP SEQUENCE seq").starts_with(
        "Dependency Error: Cannot drop entry \"seq\" because there are entries that depend on it."
    ));
    con.query("DROP TABLE t").unwrap();
    con.query("DROP SEQUENCE seq").unwrap();
    assert!(
        error(&mut con, "SELECT nextval('seq')")
            .starts_with("Catalog Error: Sequence with name seq does not exist!")
    );
}
