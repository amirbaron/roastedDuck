//! Schema evolution must not decode unselected Parquet struct fields.
#![allow(clippy::unwrap_used, reason = "Small deterministic Parquet fixtures")]

use roastedduck::{Database, Result};
use roastedduck_parquet::reader::ParquetReader;
use roastedduck_parquet::schema::ParquetOptions;
use std::path::PathBuf;

struct Fixture(PathBuf);
impl Fixture {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "roastedduck-schema-projection-{}-{name}",
            std::process::id()
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn path(&self, name: &str) -> String {
        self.0.join(name).to_str().unwrap().replace('\'', "''")
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn projected_read_ignores_damaged_sibling(
    name: &str,
    local_expression: &str,
    global_expression: &str,
    projection: &str,
) -> Result<()> {
    let fixture = Fixture::new(name);
    let local = fixture.path("local.parquet");
    let global = fixture.path("global.parquet");
    {
        let mut connection = Database::open_in_memory()?.connect()?;
        for (path, expression, start, end) in [
            (&local, local_expression, 1, 3),
            (&global, global_expression, 3, 5),
        ] {
            connection.query(format!(
                "COPY (SELECT {expression} s FROM range({start},{end})t(i)) \
                 TO '{path}' (FORMAT PARQUET, COMPRESSION SNAPPY)"
            ))?;
        }
    }
    let scan = format!("read_parquet(['{local}','{global}'], union_by_name=true)");
    let query = format!("SELECT {projection} FROM {scan} ORDER BY 1");
    {
        let mut connection = Database::open_in_memory()?.connect()?;
        let clean = connection.query(&query)?;
        for row in 0..4 {
            assert_eq!(clean.value(0, row).as_i128(), Some(row as i128 + 1));
        }
    }
    // Corrupt a data-page header, preserving the footer/schema and selected column.
    // Full reads must still fail, proving the field was actually excluded.
    let reader = ParquetReader::open(local.as_bytes(), ParquetOptions::default())?;
    let metadata = reader.context.metadata.row_groups[0]
        .columns
        .iter()
        .filter_map(|column| column.meta_data.as_ref())
        .find(|column| column.path_in_schema.last().is_some_and(|name| name == "b"))
        .unwrap();
    let offset = usize::try_from(metadata.data_page_offset).unwrap();
    let local_type = reader.columns[0].logical_type.to_string();
    drop(reader);
    let mut bytes = std::fs::read(&local).unwrap();
    bytes[offset..offset + 16].fill(0xff);
    std::fs::write(&local, bytes).unwrap();

    let mut connection = Database::open_in_memory()?.connect()?;
    let result = connection.query(query)?;
    assert_eq!(result.row_count(), 4);
    for row in 0..4 {
        assert_eq!(result.value(0, row).as_i128(), Some(row as i128 + 1));
    }
    // An explicit schema follows C++'s whole-column path, even for identical types.
    // It must still read/reject the damaged sibling rather than silently pruning it.
    assert!(
        connection
            .query(format!(
                "SELECT {projection} FROM read_parquet('{local}', \
                 schema=MAP{{'s':{{'name':'s','type':'{local_type}','default_value':NULL}}}})"
            ))
            .is_err()
    );
    assert!(connection.query(format!("FROM {scan}")).is_err());
    Ok(())
}

#[test]
fn evolved_struct_does_not_read_unused_field() -> Result<()> {
    projected_read_ignores_damaged_sibling(
        "flat",
        "{'a':i::INTEGER,'b':repeat('payload',100)}",
        "{'a':i::BIGINT,'b':repeat('payload',100)}",
        "s.a",
    )
}

#[test]
fn evolved_struct_maps_reordered_fields_by_name() -> Result<()> {
    projected_read_ignores_damaged_sibling(
        "reordered",
        "{'b':repeat('payload',100),'a':i::INTEGER}",
        "{'a':i::BIGINT,'b':repeat('payload',100)}",
        "s.a",
    )
}

#[test]
fn evolved_nested_struct_does_not_read_unused_field() -> Result<()> {
    projected_read_ignores_damaged_sibling(
        "nested",
        "{'inner':{'a':i::INTEGER,'b':repeat('payload',100)}}",
        "{'inner':{'a':i::BIGINT,'b':repeat('payload',100)}}",
        "s.inner.a",
    )
}

#[test]
fn disjoint_struct_cast_keeps_binder_error_during_execution() -> Result<()> {
    let fixture = Fixture::new("disjoint");
    let path = fixture.path("input.parquet");
    let mut connection = Database::open_in_memory()?.connect()?;
    connection.query(format!(
        "COPY (SELECT {{'a':1,'b':[10,11]}} s) TO '{path}' (FORMAT PARQUET)"
    ))?;
    for expression in ["s", "s.c"] {
        let query = format!(
            "SELECT {expression} FROM read_parquet('{path}', \
             schema=MAP{{'s':{{'name':'s','type':'STRUCT(c BIGINT)','default_value':NULL}}}})"
        );
        let prepared = connection.prepare(query)?;
        let parameters = roastedduck_common::CaseInsensitiveMap::<roastedduck_types::Value>::new();
        let error = connection
            .execute_prepared(&prepared, &parameters, false)
            .unwrap_err();
        assert_eq!(error.kind(), roastedduck::ErrorKind::Binder);
        assert_eq!(
            error.raw_message(),
            "STRUCT to STRUCT cast must have at least one matching member"
        );
    }
    Ok(())
}

#[test]
fn incompatible_unselected_member_keeps_reference_conversion_error() -> Result<()> {
    let fixture = Fixture::new("incompatible");
    let path = fixture.path("input.parquet");
    let mut connection = Database::open_in_memory()?.connect()?;
    connection.query(format!(
        "COPY (SELECT {{'a':1,'b':[10,11]}} s) TO '{path}' (FORMAT PARQUET)"
    ))?;
    let error = connection
        .query(format!(
            "SELECT s.a FROM read_parquet('{path}', \
             schema=MAP{{'s':{{'name':'s','type':'STRUCT(a BIGINT,b DATE)','default_value':NULL}}}})"
        ))
        .unwrap_err();
    assert_eq!(error.kind(), roastedduck::ErrorKind::Conversion);
    assert!(
        error
            .raw_message()
            .contains("Unimplemented type for cast (INTEGER[] -> DATE)")
    );
    assert!(error.raw_message().contains("Error while reading file"));
    Ok(())
}

#[test]
fn inferred_disjoint_nested_fields_become_null() -> Result<()> {
    let fixture = Fixture::new("inferred-disjoint");
    let first = fixture.path("first.parquet");
    let second = fixture.path("second.parquet");
    let mut connection = Database::open_in_memory()?.connect()?;
    connection.query(format!(
        "COPY (SELECT {{'a':i::INTEGER,'b':{{'x':(i*10)::INTEGER}}}} s \
         FROM range(1,3)t(i)) TO '{first}' (FORMAT PARQUET)"
    ))?;
    connection.query(format!(
        "COPY (SELECT {{'a':i::BIGINT,'b':{{'y':(i*10)::INTEGER}}}} s \
         FROM range(3,5)t(i)) TO '{second}' (FORMAT PARQUET)"
    ))?;
    let scan = format!("read_parquet(['{first}','{second}'])");
    let result = connection.query(format!("SELECT s.a,s.b.x FROM {scan} ORDER BY 1"))?;
    assert_eq!(result.row_count(), 4);
    for row in 0..4 {
        assert_eq!(result.value(0, row).as_i128(), Some(row as i128 + 1));
        if row < 2 {
            assert_eq!(result.value(1, row).as_i128(), Some((row as i128 + 1) * 10));
        } else {
            assert!(result.value(1, row).is_null());
        }
    }
    let whole = connection.query(format!("SELECT s::VARCHAR FROM {scan}"))?;
    assert_eq!(whole.value(0, 0).as_str(), Some("{'a': 1, 'b': {'x': 10}}"));
    assert_eq!(whole.value(0, 3).as_str(), Some("{'a': 4, 'b': {'x': NULL}}"));
    Ok(())
}

#[test]
fn inferred_list_elements_remap_missing_struct_fields() -> Result<()> {
    let fixture = Fixture::new("inferred-list");
    let first = fixture.path("first.parquet");
    let second = fixture.path("second.parquet");
    let mut connection = Database::open_in_memory()?.connect()?;
    connection.query(format!(
        "COPY (SELECT [{{'x':i::INTEGER}}] s FROM range(1,3)t(i)) TO '{first}' (FORMAT PARQUET)"
    ))?;
    connection.query(format!(
        "COPY (SELECT [{{'y':i::BIGINT}}] s FROM range(3,5)t(i)) TO '{second}' (FORMAT PARQUET)"
    ))?;
    let result = connection.query(format!(
        "SELECT s::VARCHAR FROM read_parquet(['{first}','{second}'])"
    ))?;
    assert_eq!(result.row_count(), 4);
    assert_eq!(result.value(0, 0).as_str(), Some("[{'x': 1}]"));
    assert_eq!(result.value(0, 3).as_str(), Some("[{'x': NULL}]"));
    Ok(())
}

#[test]
fn inferred_map_values_remap_missing_struct_fields() -> Result<()> {
    let fixture = Fixture::new("inferred-map");
    let first = fixture.path("first.parquet");
    let second = fixture.path("second.parquet");
    let mut connection = Database::open_in_memory()?.connect()?;
    connection.query(format!(
        "COPY (SELECT map([i::INTEGER],[{{'x':(i*10)::INTEGER}}]) s FROM range(1,3)t(i)) \
         TO '{first}' (FORMAT PARQUET)"
    ))?;
    connection.query(format!(
        "COPY (SELECT map([i::BIGINT],[{{'y':(i*10)::INTEGER}}]) s FROM range(3,5)t(i)) \
         TO '{second}' (FORMAT PARQUET)"
    ))?;
    let result = connection.query(format!(
        "SELECT s::VARCHAR FROM read_parquet(['{first}','{second}'])"
    ))?;
    assert_eq!(result.row_count(), 4);
    assert_eq!(result.value(0, 0).as_str(), Some("{1={'x': 10}}"));
    assert_eq!(result.value(0, 3).as_str(), Some("{4={'x': NULL}}"));
    Ok(())
}

#[test]
fn remapping_preserves_null_and_empty_containers() -> Result<()> {
    let cases = [
        (
            "nullable-struct",
            "CASE WHEN i=1 THEN NULL ELSE {'a':i::INTEGER,'b':{'x':i::INTEGER}} END",
            "CASE WHEN i=1 THEN NULL ELSE {'a':i::BIGINT,'b':{'y':i::BIGINT}} END",
            "{'a': 3, 'b': {'x': 3}}",
            "{'a': 3, 'b': {'x': NULL}}",
        ),
        (
            "nullable-list",
            "CASE WHEN i=1 THEN NULL WHEN i=2 THEN [] ELSE [{'x':i::INTEGER}] END",
            "CASE WHEN i=1 THEN NULL WHEN i=2 THEN [] ELSE [{'y':i::BIGINT}] END",
            "[{'x': 3}]",
            "[{'x': NULL}]",
        ),
        (
            "nullable-map",
            "CASE WHEN i=1 THEN NULL WHEN i=2 THEN map([]::INTEGER[],[]::STRUCT(x INTEGER)[]) ELSE map([i::INTEGER],[{'x':i::INTEGER}]) END",
            "CASE WHEN i=1 THEN NULL WHEN i=2 THEN map([]::BIGINT[],[]::STRUCT(y BIGINT)[]) ELSE map([i::BIGINT],[{'y':i::BIGINT}]) END",
            "{3={'x': 3}}",
            "{3={'x': NULL}}",
        ),
    ];
    for (name, original, evolved, expected_first, expected_last) in cases {
        let fixture = Fixture::new(name);
        let first = fixture.path("first.parquet");
        let second = fixture.path("second.parquet");
        let mut connection = Database::open_in_memory()?.connect()?;
        for (path, expression) in [(&first, original), (&second, evolved)] {
            connection.query(format!(
                "COPY (SELECT {expression} s FROM range(1,4)t(i)) TO '{path}' (FORMAT PARQUET)"
            ))?;
        }
        let result = connection.query(format!(
            "SELECT s::VARCHAR FROM read_parquet(['{first}','{second}'])"
        ))?;
        assert_eq!(result.row_count(), 6, "{name}");
        assert!(result.value(0, 0).is_null(), "{name}");
        assert!(result.value(0, 3).is_null(), "{name}");
        assert_eq!(result.value(0, 2).as_str(), Some(expected_first), "{name}");
        assert_eq!(result.value(0, 5).as_str(), Some(expected_last), "{name}");
        if name == "nullable-list" {
            assert_eq!(result.value(0, 1).as_str(), Some("[]"));
            assert_eq!(result.value(0, 4).as_str(), Some("[]"));
        } else if name == "nullable-map" {
            assert_eq!(result.value(0, 1).as_str(), Some("{}"));
            assert_eq!(result.value(0, 4).as_str(), Some("{}"));
        }
    }
    Ok(())
}
