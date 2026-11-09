//! Real native database, attach, checkpoint and WAL paths on the external test directory.
use roastedduck::{Config, Database};
use roastedduck_common::Result;
fn sql_string(path: &std::path::Path) -> roastedduck_common::ByteString {
    let mut text = roastedduck_common::ByteString::from("'");
    for byte in path.as_os_str().as_encoded_bytes() {
        text.push(*byte);
        if *byte == b'\'' {
            text.push(b'\'');
        }
    }
    text.push(b'\'');
    text
}

#[test]
fn unicode_database_attach_checkpoint_and_wal_reopen() -> Result<()> {
    let root = std::path::PathBuf::from(std::env::var_os("TMPDIR").expect("external test TMPDIR"))
        .join(format!("native-runtime-paths-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let path = root.join("雪.db");
    let other = root.join("界.db");
    {
        let database = Database::open(path.as_os_str().as_encoded_bytes(), &Config::default())?;
        assert_eq!(database.path(), path.as_os_str().as_encoded_bytes());
        let mut con = database.connect()?;
        con.query("CREATE TABLE \"値\"(i INTEGER); INSERT INTO \"値\" VALUES (42),(7); CHECKPOINT;")?;
        con.query(roastedduck_common::byte_format!("ATTACH {} AS \"東京\"; CREATE TABLE \"東京\".t AS SELECT * FROM \"値\"; CHECKPOINT \"東京\"; DETACH \"東京\";",sql_string(&other)))?;
        con.query("PRAGMA disable_checkpoint_on_shutdown; INSERT INTO \"値\" VALUES (9);")?;
    }
    let mut wal = path.as_os_str().to_os_string();
    wal.push(".wal");
    let wal = std::path::PathBuf::from(wal);
    assert!(wal.metadata().unwrap().len() > 0);
    {
        let database = Database::open(path.as_os_str().as_encoded_bytes(), &Config::default())?;
        let mut con = database.connect()?;
        let result = con.query("SELECT sum(i) FROM \"値\"")?;
        assert_eq!(result.value(0, 0).as_i128(), Some(58));
        con.query(roastedduck_common::byte_format!(
            "ATTACH {} AS \"東京\";",
            sql_string(&other)
        ))?;
        assert_eq!(
            con.query("SELECT sum(i) FROM \"東京\".t")?.value(0, 0).as_i128(),
            Some(49)
        );
    }
    std::fs::remove_dir_all(root).unwrap();
    Ok(())
}
