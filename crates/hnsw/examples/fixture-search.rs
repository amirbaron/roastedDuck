//! Probe a numeric CSV fixture exported in physical row-ID order by the SQL engine.
//! Usage: `fixture-search input.csv query_coordinates_comma_separated k`
//! Input: `row_id,user_id,coordinate_0,...` (no header). Output: graph candidates.
use std::collections::HashMap;
use std::io::{BufRead, BufReader};

use roastedduck_hnsw::{Config, Hnsw, Metric};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 4 {
        return Err("usage: fixture-search input.csv comma-separated-query k".into());
    }
    let query: Vec<f32> = args[2].split(',').map(str::parse).collect::<Result<_, _>>()?;
    let k: usize = args[3].parse()?;
    let mut graph = Hnsw::new(Config::new(query.len(), Metric::L2Squared))?;
    let mut records = HashMap::new();
    for line in BufReader::new(std::fs::File::open(&args[1])?).lines() {
        let line = line?;
        let columns: Vec<_> = line.split(',').collect();
        if columns.len() != query.len() + 2 {
            return Err("unexpected fixture column count".into());
        }
        let label: i64 = columns[0].parse()?;
        let user_id: i64 = columns[1].parse()?;
        let vector: Vec<f32> = columns[2..]
            .iter()
            .map(|value| value.parse())
            .collect::<Result<_, _>>()?;
        graph.insert(label, &vector)?;
        records.insert(label, (user_id, vector));
    }
    let result = graph.search(&query, k, None)?;
    eprintln!("graph={:?}; search={:?}", graph.stats(), result.stats);
    println!("row_id,user_id,distance_squared,vector");
    for neighbor in result.neighbors {
        let (user_id, vector) = records
            .get(&neighbor.label)
            .ok_or("candidate is absent from input")?;
        println!(
            "{},{},{},\"{:?}\"",
            neighbor.label, user_id, neighbor.distance, vector
        );
    }
    Ok(())
}
