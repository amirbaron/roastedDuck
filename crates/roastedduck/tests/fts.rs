//! SQL-visible FTS creation, catalog/autoload and persisted macro behavior.
use roastedduck::{Config, Database, Result, Value};

#[test]
fn fts_autoload_index_rank_rebuild_and_drop() -> Result<()> {
    let db = Database::open(
        "",
        &Config {
            load_extensions: false,
            options: vec![("autoload_known_extensions".into(), Value::boolean(true))],
            ..Config::default()
        },
    )?;
    let mut c = db.connect()?;
    assert_eq!(
        c.query("SELECT stem('connections', 'porter')")?.value(0, 0),
        Value::varchar("connect")
    );
    c.query("CREATE TABLE documents(id INTEGER, body VARCHAR); INSERT INTO documents VALUES (1, 'red fox running'), (2, 'blue bird'), (3, 'red fox fox')")?;
    c.query(
        "PRAGMA create_fts_index('documents', 'id', 'body', stemmer='none', stopwords='none', lower=false)",
    )?;
    assert_eq!(
        c.query("SELECT count(*) FROM fts_main_documents.dict WHERE term='running'")?
            .value(0, 0),
        Value::bigint(1)
    );
    assert_eq!(c.query("SELECT id FROM documents WHERE fts_main_documents.match_bm25(id, 'fox') IS NOT NULL ORDER BY fts_main_documents.match_bm25(id, 'fox') DESC")?.value(0, 0), Value::integer(3));
    assert!(
        c.query("PRAGMA create_fts_index('documents', 'id', 'body')")
            .is_err()
    );
    c.query("PRAGMA create_fts_index('documents', 'id', '*', overwrite=true)")?;
    assert_eq!(
        c.query("SELECT count(*) FROM fts_main_documents.dict WHERE term='run'")?
            .value(0, 0),
        Value::bigint(1)
    );
    c.query("PRAGMA drop_fts_index('documents')")?;
    assert!(c.query("PRAGMA drop_fts_index('documents')").is_err());
    Ok(())
}

#[test]
fn fts_pragma_autoload_and_named_parameter_validation() -> Result<()> {
    let db = Database::open(
        "",
        &Config {
            load_extensions: false,
            options: vec![("autoload_known_extensions".into(), Value::boolean(true))],
            ..Config::default()
        },
    )?;
    let mut c = db.connect()?;
    c.query("CREATE TABLE docs(id INT, text VARCHAR); INSERT INTO docs VALUES (1, 'Running')")?;
    c.query("PRAGMA create_fts_index('docs', 'id', 'text', stemmer='none', stopwords='none', lower=false)")?;
    assert_eq!(
        c.query("SELECT term FROM fts_main_docs.dict")?.value(0, 0),
        Value::varchar("Running")
    );
    assert!(
        c.query("PRAGMA create_fts_index('docs', 'id', 'text', unknown_option=true)")
            .is_err()
    );
    Ok(())
}

#[test]
fn qualified_catalog_quirk_and_failed_rebuild_match_pinned_fts() -> Result<()> {
    let mut c = Database::open_in_memory()?.connect()?;
    c.query("LOAD fts")?;
    c.query("ATTACH ':memory:' AS other; CREATE TABLE other.main.d(id INT, body VARCHAR); INSERT INTO other.main.d VALUES (1, 'original')")?;
    c.query("PRAGMA create_fts_index('other.main.d', 'id', 'body', stemmer='none', stopwords='none')")?;
    // The pinned implementation does not recognize the qualified index schema
    // during its preflight lookup, so duplicate create rebuilds without overwrite.
    c.query("PRAGMA create_fts_index('other.main.d', 'id', 'body', stemmer='none', stopwords='none')")?;
    let error = c.query("PRAGMA drop_fts_index('other.main.d')").unwrap_err();
    assert!(
        error
            .to_string()
            .contains("a FTS index does not exist on table 'main.d'")
    );
    c.query("USE other")?;
    assert!(
        c.query("PRAGMA create_fts_index('d', 'id', 'body', overwrite=true, stemmer='not_a_stemmer')")
            .is_err()
    );
    assert_eq!(
        c.query("SELECT term FROM fts_main_d.dict")?.value(0, 0),
        Value::varchar("original")
    );
    c.query("PRAGMA drop_fts_index('d')")?;
    Ok(())
}
