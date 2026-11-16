//! Development probe for query cancellation during an AWS process or Azure CLI lookup.
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let marker = PathBuf::from(std::env::args().nth(1).ok_or("missing helper-ready path")?);
    let azure = std::env::args().nth(2).is_some_and(|name| name == "azure");
    let database = roastedduck::Database::open_in_memory()?;
    let mut connection = database.connect()?;
    connection.query(if azure {
        "SET allow_persistent_secrets=false; LOAD azure;"
    } else {
        "SET allow_persistent_secrets=false; LOAD httpfs; LOAD aws;"
    })?;
    let handle = connection.client_handle();
    std::thread::scope(|scope| -> Result<(), Box<dyn std::error::Error>> {
        let watcher = scope.spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            while !marker.exists() {
                if Instant::now() >= deadline {
                    handle.interrupt();
                    return Err("credential helper did not start");
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            handle.interrupt();
            Ok(())
        });
        let result = connection.query(if azure {
            "CREATE SECRET fixture(TYPE azure, PROVIDER credential_chain, CHAIN 'cli', ACCOUNT_NAME 'fixture'); SELECT * FROM 'az://fixture/input.csv';"
        } else {
            "CREATE SECRET fixture(TYPE s3, PROVIDER credential_chain, CHAIN 'process', PROFILE 'slow');"
        });
        watcher.join().map_err(|_| "interrupt watcher panicked")??;
        let error = result
            .err()
            .ok_or("credential query succeeded after interruption")?;
        if error.kind() != roastedduck_common::ErrorKind::Interrupt {
            return Err(format!("wrong error kind: {:?}", error.kind()).into());
        }
        println!("interrupt-ok");
        Ok(())
    })
}
