//! Frozen C++ 1.5.6 `IndexStorageInfo` wire comparison, including native name bytes.
use roastedduck_common::{ByteString, Error, Result};
use roastedduck_serde::binary::{BinaryDeserializer, BinarySerializer};
use roastedduck_serde::{Deserializer, SerializationOptions, Serializer};
use roastedduck_storage::checkpoint::pointers::{
    deserialize_index_storage_info, serialize_index_storage_info,
};
use roastedduck_storage::index::art::ArtStorageInfo;
use roastedduck_storage::index::{BufferPointer, FixedSizeAllocatorInfo};
use roastedduck_types::Value;

fn decode(hex: &str) -> Result<Vec<u8>> {
    if !hex.len().is_multiple_of(2) {
        return Err(Error::invalid_input("odd oracle hex length"));
    }
    hex.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(
            |pair| match (char::from(pair[0]).to_digit(16), char::from(pair[1]).to_digit(16)) {
                (Some(a), Some(b)) => Ok((a * 16 + b) as u8),
                _ => Err(Error::invalid_input("invalid oracle hex")),
            },
        )
        .collect()
}

pub fn compare(line: &str) -> Result<()> {
    let fields: Vec<_> = line.split('\t').collect();
    let name = ByteString::from(decode(fields[1])?);
    let mut expected = ArtStorageInfo {
        name: name.clone(),
        ..ArtStorageInfo::default()
    };
    match fields[0] {
        "0" => (),
        "1" | "2" => {
            expected.root = 0x1234_5678_9abc_def0;
            expected
                .options
                .insert("v1_0_0_storage".to_owned(), Value::boolean(false));
        }
        _ => return Err(Error::invalid_input("invalid oracle mode")),
    }
    if fields[0] == "2" {
        expected.allocator_infos.push(FixedSizeAllocatorInfo {
            segment_size: 24,
            buffer_ids: vec![0, 2],
            block_pointers: vec![
                BufferPointer {
                    block_id: 42,
                    offset: 16,
                },
                BufferPointer {
                    block_id: 45,
                    offset: 32,
                },
            ],
            segment_counts: vec![1, 2],
            allocation_sizes: vec![4096, 8192],
            buffers_with_free_space: vec![2],
        });
    }
    let encoded = decode(fields[2])?;
    let serialize = |info: &ArtStorageInfo| -> Result<Vec<u8>> {
        let mut writer = BinarySerializer::new(SerializationOptions::default());
        writer.begin();
        (&mut writer as &mut dyn Serializer)
            .write_object_property(100, "payload", |s| serialize_index_storage_info(s, info))?;
        writer.end();
        Ok(writer.bytes().to_vec())
    };
    assert_eq!(serialize(&expected)?, encoded, "constructed index storage wire");
    let mut reader = BinaryDeserializer::new(&encoded);
    reader.begin()?;
    let restored = (&mut reader as &mut dyn Deserializer).read_object_property(
        100,
        "payload",
        deserialize_index_storage_info,
    )?;
    reader.end()?;
    assert_eq!(restored.name, name, "restored index name bytes");
    assert_eq!(serialize(&restored)?, encoded, "restored index storage wire");
    Ok(())
}
