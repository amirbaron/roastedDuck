//! Real runtime file and WAL interchange; the driver supplies isolated SSD paths.
use roastedduck::{Config, Database};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let mode = args.next().ok_or("mode")?;
    let path = args.next().ok_or("path")?;
    let database = Database::open(path.as_encoded_bytes(), &Config::default())?;
    let mut connection = database.connect()?;
    if mode == "create" {
        connection.query("CREATE TABLE \"値\"(i INTEGER, s VARCHAR); INSERT INTO \"値\" VALUES (42,'雪'),(7,NULL); CHECKPOINT; PRAGMA disable_checkpoint_on_shutdown; INSERT INTO \"値\" VALUES (9,'界');")?;
    } else if mode == "verify" {
        let result = connection.query("SELECT count(*),sum(i),count(s) FROM \"値\"")?;
        assert_eq!(result.value(0, 0).as_i64(), Some(3));
        assert_eq!(result.value(1, 0).as_i128(), Some(58));
        assert_eq!(result.value(2, 0).as_i64(), Some(2));
        println!("count=3 sum=58 nonnull=2");
    } else {
        return Err("unknown mode".into());
    }
    Ok(())
}
