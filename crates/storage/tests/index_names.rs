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
