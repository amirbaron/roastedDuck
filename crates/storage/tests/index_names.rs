//! Native index storage metadata and lookup regression coverage.
#[path = "../examples/support/index_names.rs"]
mod index_names;
use roastedduck_buffer::{BufferManager, BufferManagerConfig};
use roastedduck_common::{ByteString, Result};
use roastedduck_storage::index::VerifyExistenceType;
use roastedduck_storage::index::art::ArtStorageInfo;
use roastedduck_storage::index::conflict_manager::ConflictManager;
use roastedduck_storage::index::{
    Art, IndexConstraintType, IndexKeyExpressions, TableIndexList, UnboundIndex, UnboundIndexStorage,
};
use roastedduck_types::LogicalType;
use std::sync::Arc;

#[test]
fn index_storage_wires_match_cpp() -> Result<()> {
    let mut count = 0;
    for line in include_str!("data/index_names_1.5.6.tsv").lines() {
        index_names::compare(line)?;
        count += 1;
    }
    assert_eq!(count, 24);
    Ok(())
}

#[test]
fn raw_names_survive_unbound_binding_delta_lookup_and_removal() -> Result<()> {
    let manager = BufferManager::new(BufferManagerConfig::default())?;
    let list = TableIndexList::default();
    let names: Vec<ByteString> = (0..=255).map(|byte| vec![b'I', byte, b'X'].into()).collect();
    for name in &names {
        list.add_unbound(UnboundIndex::new(
            name.as_bytes(),
            "ART",
            IndexConstraintType::Primary,
            vec![0],
            Arc::new(()),
            UnboundIndexStorage::Disk(ArtStorageInfo {
                name: name.clone(),
                ..ArtStorageInfo::default()
            }),
        ));
        assert!(list.contains(name));
        assert!(!list.name_is_unique(name));
    }
    list.bind(&mut |unbound| {
        Art::new(
            unbound.name().as_bytes(),
            unbound.constraint_type(),
            unbound.column_ids().to_vec(),
            vec![LogicalType::INTEGER],
            IndexKeyExpressions::Columns(vec!["key".into()]),
            manager.clone(),
            262_136,
        )
    })?;
    let delta = list.delta_list()?;
    let mut conflicts = ConflictManager::new(VerifyExistenceType::Append, 1);
    for name in &names {
        assert_eq!(list.find(name).unwrap().name(), name);
        assert_eq!(delta.find(name).unwrap().name(), name);
        conflicts.add_index(name);
        assert!(conflicts.index_matches(name.to_ascii_lowercase()));
    }
    for name in &names {
        assert!(list.remove(name));
        assert!(!list.contains(name));
        assert!(list.name_is_unique(name));
        assert!(list.find(name).is_none());
    }
    assert!(list.is_empty());
    Ok(())
}

#[test]
fn selective_binding_leaves_unsupported_formats_opaque() -> Result<()> {
    let manager = BufferManager::new(BufferManagerConfig::default())?;
    let list = TableIndexList::default();
    for (name, kind) in [("first", "HNSW"), ("middle", "ART"), ("last", "HNSW")] {
        list.add_unbound(UnboundIndex::new(
            name,
            kind,
            IndexConstraintType::None,
            vec![0],
            Arc::new(()),
            UnboundIndexStorage::Deprecated,
        ));
    }
    let mut calls = 0;
    for _ in 0..2 {
        list.bind_matching(
            &|index| index.index_type().eq_ignore_ascii_case("ART"),
            &mut |index| {
                calls += 1;
                assert!(index.index_type().eq_ignore_ascii_case("ART"));
                Art::new(
                    index.name(),
                    IndexConstraintType::None,
                    vec![0],
                    vec![LogicalType::INTEGER],
                    IndexKeyExpressions::Columns(vec!["key".into()]),
                    manager.clone(),
                    262_136,
                )
            },
        )?;
    }
    assert_eq!(calls, 1);
    assert_eq!(list.len(), 3);
    assert_eq!(list.entries().len(), 1);
    assert!(list.has_unbound());
    assert!(list.find("middle").is_some());
    assert!(list.find("first").is_none());
    assert!(list.contains("first") && list.contains("last"));
    // Mandatory binding still rejects the unsupported index before publishing it.
    assert!(
        list.bind(&mut |_| Err(roastedduck_common::Error::not_implemented("opaque index")))
            .is_err()
    );
    assert_eq!(list.len(), 3);
    Ok(())
}
