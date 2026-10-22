//! The TPC-DS extension: `dsdgen` generates DuckDB's data, row for row.
//!
//! The fingerprints are DuckDB 1.5.6's `bit_xor(hash(rowid, *COLUMNS(*)))` of each table,
//! which covers every value and its row position.
#![allow(clippy::unwrap_used)]

use rustdb::{Connection, Database};

fn connect() -> Connection {
    let database = Database::open_in_memory().unwrap();
    database.load_extension("tpcds").unwrap();
    database.connect().unwrap()
}

/// `(row count, fingerprint)` of table `name`.
fn fingerprint(connection: &mut Connection, name: &str) -> (String, String) {
    let result = connection
        .query(&format!(
            "SELECT count(*), bit_xor(hash(rowid, *COLUMNS(*))) FROM {name}"
        ))
        .unwrap();
    (
        result.value(0, 0).to_varchar().unwrap(),
        result.value(1, 0).to_varchar().unwrap(),
    )
}

fn assert_fingerprints(connection: &mut Connection, suffix: &str, expected: &[(&str, &str, &str)]) {
    let mut mismatches = Vec::new();
    for &(table, count, hash) in expected {
        let name = format!("{table}{suffix}");
        let actual = fingerprint(connection, &name);
        if actual != (count.to_owned(), hash.to_owned()) {
            mismatches.push(format!("{name}: {actual:?}, expected ({count}, {hash})"));
        }
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

/// DuckDB's tables at SF 0.01.
const SF0_01: [(&str, &str, &str); 24] = [
    ("call_center", "1", "1944090953861531479"),
    ("catalog_page", "11718", "3826883159209534738"),
    ("catalog_returns", "1358", "7425355191937031328"),
    ("catalog_sales", "14313", "17840822462392974645"),
    ("customer", "1000", "3770749845111467835"),
    ("customer_address", "500", "897589536006385782"),
    ("customer_demographics", "19208", "9341822430451809993"),
    ("date_dim", "73049", "6176409018532978672"),
    ("household_demographics", "7200", "126212023640649607"),
    ("income_band", "20", "8513393290920467437"),
    ("inventory", "23490", "15420259478691105587"),
    ("item", "180", "5695718238752074752"),
    ("promotion", "3", "13990160250973656369"),
    ("reason", "1", "14441912238429149690"),
    ("ship_mode", "20", "13393619709370694191"),
    ("store", "1", "1453242478416003135"),
    ("store_returns", "2810", "8188629775977580617"),
    ("store_sales", "28810", "5895575359592603745"),
    ("time_dim", "86400", "13144900238045725405"),
    ("warehouse", "1", "2530142916008107245"),
    ("web_page", "1", "2263845820916138289"),
    ("web_returns", "679", "12245523135078263930"),
    ("web_sales", "7212", "16575409801754172175"),
    ("web_site", "1", "9427992427625808376"),
];

#[test]
fn dsdgen_matches_duckdb() {
    let mut con = connect();
    con.query("CALL dsdgen(sf=0.01)").unwrap();
    assert_fingerprints(&mut con, "", &SF0_01);
}
