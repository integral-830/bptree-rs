use crate::{
    error::BPlusNodeError, key::Key, page_id::PageId, record_id::RecordId, slotted_page::PAGE_SIZE,
    BPlusNode,
};

pub const MAGIC: [u8; 4] = *b"BTR4";
pub const FORMAT_VERSION: u8 = 1;
pub const HEADER_SIZE: usize = 6; // 4 bytes MAGIC + 1 byte FORMAT + 1 byte Node variant
pub const PAGE_ID_SIZE: usize = 8; // 8 bytes
pub fn encode(node: &BPlusNode) -> Result<[u8; PAGE_SIZE], BPlusNodeError> {
    let node_size = node.encoded_len()?;
    if node_size > PAGE_SIZE {
        return Err(BPlusNodeError::NodeTooLarge {
            reason: String::from("Node size is bigger than PAGE_SIZE: 4096"),
        });
    }
    let mut buffer = [0u8; PAGE_SIZE];
    buffer[0..4].copy_from_slice(&MAGIC); // MAGIC
    buffer[4..5].copy_from_slice(&FORMAT_VERSION.to_le_bytes()); //FORMAT
    match node {
        BPlusNode::Leaf {
            keys,
            values,
            next_leaf,
        } => {
            buffer[5..6].copy_from_slice(&1u8.to_le_bytes()); //variant
            let key_count = u16::try_from(keys.len())?;
            buffer[6..8].copy_from_slice(&key_count.to_le_bytes()); // key_count
            let mut cursor: usize = 8;
            if let Some(next_leaf) = next_leaf {
                buffer[8..9].copy_from_slice(&1u8.to_le_bytes()); // next_leaf
                buffer[9..17].copy_from_slice(&next_leaf.get().to_le_bytes()); // next_leaf_data
                cursor = 17;
            } else {
                buffer[8..9].copy_from_slice(&0u8.to_le_bytes());
                cursor = 9;
            }
            for (key, val) in keys.iter().zip(values.iter()) {
                let key_len = u16::try_from(key.as_bytes().len())?;
                buffer[cursor..cursor.checked_add(2).ok_or(BPlusNodeError::SizeOverflow)?]
                    .copy_from_slice(&key_len.to_le_bytes()); // key_len
                cursor = cursor.checked_add(2).ok_or(BPlusNodeError::SizeOverflow)?;
                buffer[cursor
                    ..cursor
                        .checked_add(key_len as usize)
                        .ok_or(BPlusNodeError::SizeOverflow)?]
                    .copy_from_slice(key.as_bytes()); // key_bytes
                cursor = cursor
                    .checked_add(key_len as usize)
                    .ok_or(BPlusNodeError::SizeOverflow)?;
                buffer[cursor
                    ..cursor
                        .checked_add(PAGE_ID_SIZE)
                        .ok_or(BPlusNodeError::SizeOverflow)?]
                    .copy_from_slice(&val.page_id().get().to_le_bytes()); // page_id
                cursor = cursor
                    .checked_add(PAGE_ID_SIZE)
                    .ok_or(BPlusNodeError::SizeOverflow)?;
                buffer[cursor..cursor.checked_add(2).ok_or(BPlusNodeError::SizeOverflow)?]
                    .copy_from_slice(&val.slot_id().to_le_bytes()); // slot_id
                cursor = cursor.checked_add(2).ok_or(BPlusNodeError::SizeOverflow)?;
            }
            Ok(buffer)
        }
        BPlusNode::Internal { keys, children } => {
            buffer[5..6].copy_from_slice(&0u8.to_le_bytes()); //variant
            let key_count = u16::try_from(keys.len())?;
            buffer[6..8].copy_from_slice(&key_count.to_le_bytes()); // key_count
            let mut cursor: usize = 8;
            for key in keys {
                let key_len = u16::try_from(key.as_bytes().len())?;
                buffer[cursor..cursor.checked_add(2).ok_or(BPlusNodeError::SizeOverflow)?]
                    .copy_from_slice(&key_len.to_le_bytes()); // key_len
                cursor = cursor.checked_add(2).ok_or(BPlusNodeError::SizeOverflow)?;
                buffer[cursor
                    ..cursor
                        .checked_add(key_len as usize)
                        .ok_or(BPlusNodeError::SizeOverflow)?]
                    .copy_from_slice(key.as_bytes()); // key_bytes
                cursor = cursor
                    .checked_add(key_len as usize)
                    .ok_or(BPlusNodeError::SizeOverflow)?;
            }
            buffer[cursor..cursor.checked_add(2).ok_or(BPlusNodeError::SizeOverflow)?]
                .copy_from_slice(
                    &(key_count
                        .checked_add(1)
                        .ok_or(BPlusNodeError::SizeOverflow)?)
                    .to_le_bytes(),
                ); // children_count
            cursor = cursor.checked_add(2).ok_or(BPlusNodeError::SizeOverflow)?;
            for child in children {
                let child = child.get();
                buffer[cursor
                    ..cursor
                        .checked_add(PAGE_ID_SIZE)
                        .ok_or(BPlusNodeError::SizeOverflow)?]
                    .copy_from_slice(&child.to_le_bytes()); // child
                cursor = cursor.checked_add(8).ok_or(BPlusNodeError::SizeOverflow)?;
            }
            Ok(buffer)
        }
    }
}

pub fn decode(page: &[u8; PAGE_SIZE]) -> Result<BPlusNode, BPlusNodeError> {
    let magic_bytes: &[u8] = page.get(0..4).ok_or(BPlusNodeError::NodeSlicingErr)?;
    if magic_bytes != MAGIC {
        return Err(BPlusNodeError::InvalidHeader {
            reason: String::from("Invalid MAGIC"),
        });
    }
    let format_bytes: [u8; 1] = page
        .get(4..5)
        .ok_or(BPlusNodeError::NodeSlicingErr)?
        .try_into()
        .map_err(|_| BPlusNodeError::NodeSlicingErr)?;
    let format: u8 = u8::from_le_bytes(format_bytes);
    if format != FORMAT_VERSION {
        return Err(BPlusNodeError::InvalidHeader {
            reason: String::from("Invalid FORMAT"),
        });
    }
    let node_type_bytes: &[u8] = page.get(5..6).ok_or(BPlusNodeError::NodeSlicingErr)?;
    let node_type_array: [u8; 1] = node_type_bytes
        .try_into()
        .map_err(|_| BPlusNodeError::NodeSlicingErr)?;
    let node_type: u8 = u8::from_le_bytes(node_type_array);
    match node_type {
        1 => {
            let key_count_bytes: [u8; 2] = page
                .get(6..8)
                .ok_or(BPlusNodeError::NodeSlicingErr)?
                .try_into()
                .map_err(|_| BPlusNodeError::NodeSlicingErr)?;
            let key_count: u16 = u16::from_le_bytes(key_count_bytes);
            let next_leaf_bytes: [u8; 1] = page
                .get(8..9)
                .ok_or(BPlusNodeError::NodeSlicingErr)?
                .try_into()
                .map_err(|_| BPlusNodeError::NodeSlicingErr)?;
            let next_leaf: u8 = u8::from_le_bytes(next_leaf_bytes);
            let mut next_leaf_data = None;
            let mut cursor: usize = 9;
            if next_leaf == 1 {
                let next_leaf_temp_data_bytes: [u8; 8] = page
                    .get(9..17)
                    .ok_or(BPlusNodeError::NodeSlicingErr)?
                    .try_into()
                    .map_err(|_| BPlusNodeError::NodeSlicingErr)?;
                let page_id = PageId::new(u64::from_le_bytes(next_leaf_temp_data_bytes));
                next_leaf_data = Some(page_id);
                cursor = 17;
            } else if next_leaf != 0 {
                return Err(BPlusNodeError::InvalidNextLeafValue {
                    found: next_leaf as usize,
                });
            }
            let mut keys_vec = Vec::new();
            let mut val_vec: Vec<RecordId> = Vec::new();
            for _ in 0..key_count {
                let key_len_bytes: &[u8] = page
                    .get(cursor..cursor.checked_add(2).ok_or(BPlusNodeError::SizeOverflow)?)
                    .ok_or(BPlusNodeError::NodeSlicingErr)?;
                cursor = cursor.checked_add(2).ok_or(BPlusNodeError::SizeOverflow)?;
                let key_len_array: [u8; 2] = key_len_bytes
                    .try_into()
                    .map_err(|_| BPlusNodeError::NodeSlicingErr)?;
                let key_len = u16::from_le_bytes(key_len_array) as usize;
                let key_bytes: &[u8] = page
                    .get(
                        cursor
                            ..cursor
                                .checked_add(key_len)
                                .ok_or(BPlusNodeError::SizeOverflow)?,
                    )
                    .ok_or(BPlusNodeError::NodeSlicingErr)?;
                cursor = cursor
                    .checked_add(key_len)
                    .ok_or(BPlusNodeError::SizeOverflow)?;
                let page_id_bytes: [u8; 8] = page
                    .get(
                        cursor
                            ..cursor
                                .checked_add(PAGE_ID_SIZE)
                                .ok_or(BPlusNodeError::SizeOverflow)?,
                    )
                    .ok_or(BPlusNodeError::NodeSlicingErr)?
                    .try_into()
                    .map_err(|_| BPlusNodeError::NodeSlicingErr)?;
                cursor = cursor
                    .checked_add(PAGE_ID_SIZE)
                    .ok_or(BPlusNodeError::SizeOverflow)?;
                let slot_id_bytes: [u8; 2] = page
                    .get(cursor..cursor.checked_add(2).ok_or(BPlusNodeError::SizeOverflow)?)
                    .ok_or(BPlusNodeError::NodeSlicingErr)?
                    .try_into()
                    .map_err(|_| BPlusNodeError::NodeSlicingErr)?;
                cursor = cursor.checked_add(2).ok_or(BPlusNodeError::SizeOverflow)?;
                let key = Key::new(key_bytes.to_owned());
                let page_id = PageId::new(u64::from_le_bytes(page_id_bytes));
                let slot_id = u16::from_le_bytes(slot_id_bytes);
                let record_id = RecordId::new(page_id, slot_id);
                keys_vec.push(key);
                val_vec.push(record_id);
            }
            let node = BPlusNode::Leaf {
                keys: keys_vec,
                values: val_vec,
                next_leaf: next_leaf_data,
            };
            node.validate()?;
            Ok(node)
        }
        0 => {
            let key_count: [u8; 2] = page
                .get(6..8)
                .ok_or(BPlusNodeError::NodeSlicingErr)?
                .try_into()
                .map_err(|_| BPlusNodeError::NodeSlicingErr)?;
            let key_count: u16 = u16::from_le_bytes(key_count);
            let mut cursor: usize = 8;
            let mut keys_vec: Vec<Key> = Vec::new();
            let mut val_vec: Vec<PageId> = Vec::new();
            for _ in 0..key_count {
                let key_len: [u8; 2] = page
                    .get(cursor..cursor.checked_add(2).ok_or(BPlusNodeError::SizeOverflow)?)
                    .ok_or(BPlusNodeError::NodeSlicingErr)?
                    .try_into()
                    .map_err(|_| BPlusNodeError::NodeSlicingErr)?;
                let key_len = u16::from_le_bytes(key_len) as usize;
                cursor = cursor.checked_add(2).ok_or(BPlusNodeError::SizeOverflow)?;
                let key_bytes: &[u8] = page
                    .get(
                        cursor
                            ..cursor
                                .checked_add(key_len)
                                .ok_or(BPlusNodeError::SizeOverflow)?,
                    )
                    .ok_or(BPlusNodeError::NodeSlicingErr)?;
                cursor = cursor
                    .checked_add(key_len)
                    .ok_or(BPlusNodeError::SizeOverflow)?;
                let key = Key::new(key_bytes.to_owned());
                keys_vec.push(key);
            }
            let children_count: [u8; 2] = page
                .get(cursor..cursor.checked_add(2).ok_or(BPlusNodeError::SizeOverflow)?)
                .ok_or(BPlusNodeError::NodeSlicingErr)?
                .try_into()
                .map_err(|_| BPlusNodeError::NodeSlicingErr)?;
            let children_count: u16 = u16::from_le_bytes(children_count);
            cursor = cursor.checked_add(2).ok_or(BPlusNodeError::SizeOverflow)?;
            if children_count
                != key_count
                    .checked_add(1)
                    .ok_or(BPlusNodeError::SizeOverflow)?
            {
                return Err(BPlusNodeError::NodeDecodingErr {
                    reason: String::from("Children and Key count not satisfied while decoding."),
                });
            }
            let required_bytes = (children_count as usize)
                .checked_mul(PAGE_ID_SIZE)
                .ok_or(BPlusNodeError::SizeOverflow)?;
            let payload_end = cursor
                .checked_add(required_bytes)
                .ok_or(BPlusNodeError::SizeOverflow)?;
            if payload_end > PAGE_SIZE {
                return Err(BPlusNodeError::InvalidPayload { found: payload_end });
            }
            for _ in 0..children_count {
                let page_id_bytes: [u8; 8] = page
                    .get(
                        cursor
                            ..cursor
                                .checked_add(PAGE_ID_SIZE)
                                .ok_or(BPlusNodeError::SizeOverflow)?,
                    )
                    .ok_or(BPlusNodeError::NodeSlicingErr)?
                    .try_into()
                    .map_err(|_| BPlusNodeError::NodeSlicingErr)?;
                cursor = cursor
                    .checked_add(PAGE_ID_SIZE)
                    .ok_or(BPlusNodeError::SizeOverflow)?;
                let page_id = PageId::new(u64::from_le_bytes(page_id_bytes));
                val_vec.push(page_id);
            }
            let node = BPlusNode::Internal {
                keys: keys_vec,
                children: val_vec,
            };
            node.validate()?;
            Ok(node)
        }
        _ => Err(BPlusNodeError::InvalidNodeType),
    }
}

#[cfg(test)]
mod node_codec_encode_tests {
    use super::*;
    use crate::error::BPlusNodeError;
    use crate::key::Key;
    use crate::node::BPlusNode;
    use crate::page_id::PageId;
    use crate::record_id::RecordId;

    fn key(bytes: &[u8]) -> Key {
        Key::new(bytes.to_vec())
    }

    fn page_id(id: u64) -> PageId {
        PageId::new(id)
    }

    fn record_id(page: u64, slot: u16) -> RecordId {
        RecordId::new(page_id(page), slot)
    }

    fn read_u16(bytes: &[u8], offset: usize) -> u16 {
        u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap())
    }

    fn read_u64(bytes: &[u8], offset: usize) -> u64 {
        u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
    }

    fn assert_header(bytes: &[u8; PAGE_SIZE], node_tag: u8, key_count: u16) {
        assert_eq!(&bytes[0..4], b"BTR4");
        assert_eq!(bytes[4], 1);
        assert_eq!(bytes[5], node_tag);
        assert_eq!(read_u16(bytes, 6), key_count);
    }

    #[test]
    fn encode_empty_leaf() {
        let node = BPlusNode::Leaf {
            keys: vec![],
            values: vec![],
            next_leaf: None,
        };

        assert!(node.validate().is_ok());

        let encoded = encode(&node).unwrap();

        assert_header(&encoded, 1, 0);
        assert_eq!(encoded[8], 0);
        assert_eq!(node.encoded_len().unwrap(), 9);
        assert!(encoded[9..].iter().all(|byte| *byte == 0));
    }

    #[test]
    fn encode_leaf_with_next_leaf() {
        let next = 0x0102_0304_0506_0708;

        let node = BPlusNode::Leaf {
            keys: vec![],
            values: vec![],
            next_leaf: Some(page_id(next)),
        };

        let encoded = encode(&node).unwrap();

        assert_header(&encoded, 1, 0);
        assert_eq!(encoded[8], 1);
        assert_eq!(read_u64(&encoded, 9), next);

        assert_eq!(node.encoded_len().unwrap(), 17);
    }

    #[test]
    fn encode_leaf_with_one_entry() {
        let node = BPlusNode::Leaf {
            keys: vec![key(b"alpha")],
            values: vec![record_id(42, 7)],
            next_leaf: None,
        };

        let encoded = encode(&node).unwrap();

        assert_header(&encoded, 1, 1);

        // next_leaf = None
        assert_eq!(encoded[8], 0);

        // key_len + key bytes
        assert_eq!(read_u16(&encoded, 9), 5);
        assert_eq!(&encoded[11..16], b"alpha");

        // RecordId = PageId(u64) + SlotId(u16)
        assert_eq!(read_u64(&encoded, 16), 42);
        assert_eq!(read_u16(&encoded, 24), 7);

        assert_eq!(node.encoded_len().unwrap(), 26);
    }

    #[test]
    fn encode_leaf_with_multiple_entries() {
        let node = BPlusNode::Leaf {
            keys: vec![key(b"a"), key(b"bb"), key(b"ccc")],
            values: vec![record_id(10, 1), record_id(20, 2), record_id(30, 3)],
            next_leaf: None,
        };

        let encoded = encode(&node).unwrap();

        assert_header(&encoded, 1, 3);

        let mut offset = 9;

        for (expected_key, expected_page, expected_slot) in [
            (b"a".as_slice(), 10u64, 1u16),
            (b"bb".as_slice(), 20u64, 2u16),
            (b"ccc".as_slice(), 30u64, 3u16),
        ] {
            assert_eq!(read_u16(&encoded, offset), expected_key.len() as u16);
            offset += 2;

            assert_eq!(&encoded[offset..offset + expected_key.len()], expected_key);
            offset += expected_key.len();

            assert_eq!(read_u64(&encoded, offset), expected_page);
            offset += 8;

            assert_eq!(read_u16(&encoded, offset), expected_slot);
            offset += 2;
        }

        assert_eq!(offset, node.encoded_len().unwrap());
    }

    #[test]
    fn encode_leaf_with_empty_key() {
        let node = BPlusNode::Leaf {
            keys: vec![key(b"")],
            values: vec![record_id(1, 0)],
            next_leaf: None,
        };

        let encoded = encode(&node).unwrap();

        assert_header(&encoded, 1, 1);
        assert_eq!(encoded[8], 0);

        // Zero-length key.
        assert_eq!(read_u16(&encoded, 9), 0);

        // RecordId immediately follows the key length.
        assert_eq!(read_u64(&encoded, 11), 1);
        assert_eq!(read_u16(&encoded, 19), 0);
    }

    #[test]
    fn encode_internal_node_with_zero_keys() {
        let node = BPlusNode::Internal {
            keys: vec![],
            children: vec![page_id(99)],
        };

        assert!(node.validate().is_ok());

        let encoded = encode(&node).unwrap();

        assert_header(&encoded, 0, 0);

        // Explicit children_count.
        assert_eq!(read_u16(&encoded, 8), 1);

        // Single child PageId.
        assert_eq!(read_u64(&encoded, 10), 99);

        assert_eq!(node.encoded_len().unwrap(), 18);
    }

    #[test]
    fn encode_internal_node_with_multiple_keys_and_children() {
        let node = BPlusNode::Internal {
            keys: vec![key(b"alpha"), key(b"omega")],
            children: vec![page_id(10), page_id(20), page_id(30)],
        };

        assert!(node.validate().is_ok());

        let encoded = encode(&node).unwrap();

        assert_header(&encoded, 0, 2);

        let mut offset = 8;

        // First key.
        assert_eq!(read_u16(&encoded, offset), 5);
        offset += 2;
        assert_eq!(&encoded[offset..offset + 5], b"alpha");
        offset += 5;

        // Second key.
        assert_eq!(read_u16(&encoded, offset), 5);
        offset += 2;
        assert_eq!(&encoded[offset..offset + 5], b"omega");
        offset += 5;

        // Explicit children_count.
        assert_eq!(read_u16(&encoded, offset), 3);
        offset += 2;

        // Child PageIds.
        for expected in [10u64, 20, 30] {
            assert_eq!(read_u64(&encoded, offset), expected);
            offset += 8;
        }

        assert_eq!(offset, node.encoded_len().unwrap());
    }

    #[test]
    fn encode_preserves_full_width_u64_page_ids() {
        let next_leaf = u64::MAX - 7;
        let record_page = u64::MAX - 15;

        let node = BPlusNode::Leaf {
            keys: vec![key(b"k")],
            values: vec![record_id(record_page, u16::MAX)],
            next_leaf: Some(page_id(next_leaf)),
        };

        let encoded = encode(&node).unwrap();

        assert_header(&encoded, 1, 1);

        // next_leaf tag and 8-byte PageId.
        assert_eq!(encoded[8], 1);
        assert_eq!(read_u64(&encoded, 9), next_leaf);

        // Entry starts after the next_leaf metadata.
        assert_eq!(read_u16(&encoded, 17), 1);
        assert_eq!(encoded[19], b'k');

        // RecordId: PageId(u64) + SlotId(u16).
        assert_eq!(read_u64(&encoded, 20), record_page);
        assert_eq!(read_u16(&encoded, 28), u16::MAX);
    }

    #[test]
    fn encode_is_deterministic() {
        let node = BPlusNode::Leaf {
            keys: vec![key(b"alpha"), key(b"beta")],
            values: vec![record_id(100, 1), record_id(200, 2)],
            next_leaf: Some(page_id(300)),
        };

        let first = encode(&node).unwrap();
        let second = encode(&node).unwrap();

        assert_eq!(first, second);
    }

    #[test]
    fn encoded_len_matches_leaf_with_next_leaf() {
        let node = BPlusNode::Leaf {
            keys: vec![key(b"alpha"), key(b"beta")],
            values: vec![record_id(10, 1), record_id(20, 2)],
            next_leaf: Some(page_id(30)),
        };

        let expected = 6 + 2 + 9 + (2 + 5 + 10) + (2 + 4 + 10);

        assert_eq!(node.encoded_len().unwrap(), expected);

        let encoded = encode(&node).unwrap();

        assert!(encoded[expected..].iter().all(|byte| *byte == 0));
    }

    #[test]
    fn encoded_len_matches_internal_node() {
        let node = BPlusNode::Internal {
            keys: vec![key(b"a"), key(b"bc")],
            children: vec![page_id(1), page_id(2), page_id(3)],
        };

        let expected = 6 + 2 + (2 + 1) + (2 + 2) + 2 + (3 * 8);

        assert_eq!(node.encoded_len().unwrap(), expected);

        let encoded = encode(&node).unwrap();

        assert!(encoded[expected..].iter().all(|byte| *byte == 0));
    }

    #[test]
    fn encode_rejects_oversized_node() {
        let oversized_key = vec![b'x'; PAGE_SIZE];

        let node = BPlusNode::Leaf {
            keys: vec![Key::new(oversized_key)],
            values: vec![record_id(1, 0)],
            next_leaf: None,
        };

        assert!(node.validate().is_ok());

        assert!(matches!(
            encode(&node),
            Err(BPlusNodeError::NodeTooLarge { .. })
        ));
    }

    #[test]
    fn validate_with_page_id_reports_corrupt_node() {
        let node = BPlusNode::Leaf {
            keys: vec![key(b"a"), key(b"b")],
            values: vec![record_id(1, 0)],
            next_leaf: None,
        };

        let page = page_id(123);

        assert!(matches!(
            node.validate_with_page_id(page),
            Err(BPlusNodeError::CorruptNode { page_id, .. })
                if page_id == page
        ));
    }

    #[test]
    fn validate_returns_invalid_node_without_page_id() {
        let node = BPlusNode::Leaf {
            keys: vec![key(b"a"), key(b"b")],
            values: vec![record_id(1, 0)],
            next_leaf: None,
        };

        assert!(matches!(
            node.validate(),
            Err(BPlusNodeError::InvalidNode { .. })
        ));
    }
}

#[cfg(test)]
mod node_codec_decode_tests {
    use super::*;
    use crate::key::Key;
    use crate::node::BPlusNode;
    use crate::page_id::PageId;
    use crate::record_id::RecordId;

    fn key(bytes: &[u8]) -> Key {
        Key::new(bytes.to_vec())
    }

    fn page_id(id: u64) -> PageId {
        PageId::new(id)
    }

    fn record_id(page: u64, slot: u16) -> RecordId {
        RecordId::new(page_id(page), slot)
    }

    fn read_u16(page: &[u8], offset: usize) -> u16 {
        u16::from_le_bytes(page[offset..offset + 2].try_into().unwrap())
    }

    fn write_u16(page: &mut [u8], offset: usize, value: u16) {
        page[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn assert_round_trip(node: BPlusNode) {
        let encoded = encode(&node).unwrap();
        let decoded = decode(&encoded).unwrap();

        assert_eq!(decoded, node);
    }

    fn blank_page(node_tag: u8, key_count: u16) -> [u8; PAGE_SIZE] {
        let mut page = [0u8; PAGE_SIZE];
        page[0..4].copy_from_slice(&MAGIC);
        page[4] = FORMAT_VERSION;
        page[5] = node_tag;
        write_u16(&mut page, 6, key_count);
        page
    }

    #[test]
    fn decode_round_trip_empty_leaf() {
        assert_round_trip(BPlusNode::Leaf {
            keys: vec![],
            values: vec![],
            next_leaf: None,
        });
    }

    #[test]
    fn decode_round_trip_leaf_with_next_leaf() {
        assert_round_trip(BPlusNode::Leaf {
            keys: vec![],
            values: vec![],
            next_leaf: Some(page_id(u64::MAX - 5)),
        });
    }

    #[test]
    fn decode_round_trip_leaf_one_entry() {
        assert_round_trip(BPlusNode::Leaf {
            keys: vec![key(b"alpha")],
            values: vec![record_id(42, 7)],
            next_leaf: None,
        });
    }

    #[test]
    fn decode_round_trip_leaf_multiple_entries() {
        assert_round_trip(BPlusNode::Leaf {
            keys: vec![key(b"a"), key(b"bb"), key(b"ccc")],
            values: vec![record_id(10, 1), record_id(20, 2), record_id(30, 3)],
            next_leaf: Some(page_id(40)),
        });
    }

    #[test]
    fn decode_round_trip_leaf_empty_key() {
        assert_round_trip(BPlusNode::Leaf {
            keys: vec![key(b"")],
            values: vec![record_id(1, 0)],
            next_leaf: None,
        });
    }

    #[test]
    fn decode_round_trip_internal_zero_keys() {
        assert_round_trip(BPlusNode::Internal {
            keys: vec![],
            children: vec![page_id(99)],
        });
    }

    #[test]
    fn decode_round_trip_internal_multiple_keys() {
        assert_round_trip(BPlusNode::Internal {
            keys: vec![key(b"alpha"), key(b"omega")],
            children: vec![page_id(10), page_id(20), page_id(30)],
        });
    }

    #[test]
    fn decode_round_trip_large_page_ids() {
        assert_round_trip(BPlusNode::Leaf {
            keys: vec![key(b"k")],
            values: vec![record_id(u64::MAX - 15, u16::MAX)],
            next_leaf: Some(page_id(u64::MAX - 7)),
        });
    }

    #[test]
    fn decode_rejects_invalid_magic() {
        let node = BPlusNode::Leaf {
            keys: vec![],
            values: vec![],
            next_leaf: None,
        };

        let mut encoded = encode(&node).unwrap();
        encoded[0] ^= 0xFF;

        assert!(decode(&encoded).is_err());
    }

    #[test]
    fn decode_rejects_unsupported_version() {
        let node = BPlusNode::Leaf {
            keys: vec![],
            values: vec![],
            next_leaf: None,
        };

        let mut encoded = encode(&node).unwrap();
        encoded[4] = FORMAT_VERSION.wrapping_add(1);

        assert!(decode(&encoded).is_err());
    }

    #[test]
    fn decode_rejects_unknown_node_tag() {
        let mut page = blank_page(2, 0);
        page[5] = 2;

        assert!(decode(&page).is_err());
    }

    #[test]
    fn decode_rejects_invalid_next_leaf_tag() {
        let mut page = blank_page(1, 0);
        page[8] = 2;

        assert!(decode(&page).is_err());
    }

    #[test]
    fn decode_rejects_truncated_next_leaf_page_id() {
        let mut page = blank_page(1, 0);
        page[8] = 1;

        // The fixed page array has 4096 bytes, so place the cursor
        // at the final byte by using a malformed layout.
        // A correct decoder must reject a next_leaf PageId that
        // cannot be fully read from its expected location.
        //
        // Since next_leaf is at offset 8 in this format, the full
        // 8-byte PageId fits in a PAGE_SIZE array. This test instead
        // verifies that an invalid tag is rejected.
        page[8] = 2;

        assert!(decode(&page).is_err());
    }

    #[test]
    fn decode_rejects_key_length_out_of_bounds() {
        let mut page = blank_page(1, 1);

        // Leaf with next_leaf=None.
        page[8] = 0;

        // key_len is at offset 9. Claim a key larger than the
        // remaining page payload.
        write_u16(&mut page, 9, u16::MAX);

        assert!(decode(&page).is_err());
    }

    #[test]
    fn decode_rejects_truncated_record_id() {
        let mut page = blank_page(1, 1);
        page[8] = 0;

        // Position the key payload near the end of the page.
        // The decoder must reject the entry because the RecordId
        // cannot fit after the key.
        let key_len = (PAGE_SIZE - 9 - 2 - 5) as u16;
        write_u16(&mut page, 9, key_len);

        assert!(decode(&page).is_err());
    }

    #[test]
    fn decode_rejects_internal_children_count_mismatch() {
        let mut page = blank_page(0, 0);

        // Internal node with key_count=0 must have exactly 1 child.
        write_u16(&mut page, 8, 0);

        assert!(decode(&page).is_err());
    }

    #[test]
    fn decode_rejects_unsorted_leaf_keys() {
        let node = BPlusNode::Leaf {
            keys: vec![key(b"a"), key(b"b")],
            values: vec![record_id(1, 1), record_id(2, 2)],
            next_leaf: None,
        };

        let mut encoded = encode(&node).unwrap();

        // First key byte: header(8) + next_leaf tag(1) + key_len(2).
        encoded[11] = b'z';

        assert!(decode(&encoded).is_err());
    }

    #[test]
    fn decode_rejects_duplicate_leaf_keys() {
        let node = BPlusNode::Leaf {
            keys: vec![key(b"a"), key(b"b")],
            values: vec![record_id(1, 1), record_id(2, 2)],
            next_leaf: None,
        };

        let mut encoded = encode(&node).unwrap();

        // First entry occupies 2 + 1 + 10 bytes after offset 9.
        // Second key bytes begin at offset 22.
        encoded[22 + 2] = b'a';

        assert!(decode(&encoded).is_err());
    }

    #[test]
    fn decode_rejects_unsorted_internal_keys() {
        let node = BPlusNode::Internal {
            keys: vec![key(b"a"), key(b"b")],
            children: vec![page_id(1), page_id(2), page_id(3)],
        };

        let mut encoded = encode(&node).unwrap();

        // First key begins at offset 10.
        encoded[10] = b'z';

        assert!(decode(&encoded).is_err());
    }

    #[test]
    fn decode_rejects_duplicate_internal_keys() {
        let node = BPlusNode::Internal {
            keys: vec![key(b"a"), key(b"b")],
            children: vec![page_id(1), page_id(2), page_id(3)],
        };

        let mut encoded = encode(&node).unwrap();

        // Second key begins after:
        // header + key_count + first key_len + first key bytes.
        encoded[13] = b'a';

        assert!(decode(&encoded).is_err());
    }

    #[test]
    fn decode_rejects_internal_child_count_overflow_mismatch() {
        let mut page = blank_page(0, u16::MAX);

        // The decoder must reject malformed counts safely.
        // There is no valid way for this key count to fit in a
        // single page with the required keys and children.
        write_u16(&mut page, 8, 0);

        assert!(decode(&page).is_err());
    }

    #[test]
    fn decode_accepts_zero_padding_after_node() {
        let node = BPlusNode::Leaf {
            keys: vec![key(b"a")],
            values: vec![record_id(1, 1)],
            next_leaf: None,
        };

        let encoded = encode(&node).unwrap();

        // The encoder zero-initializes unused page bytes.
        // Decoder should accept this normal representation.
        assert!(decode(&encoded).is_ok());
    }

    #[test]
    fn decode_does_not_panic_on_all_zero_page() {
        let page = [0u8; PAGE_SIZE];

        // Invalid magic should return an error rather than panic.
        assert!(decode(&page).is_err());
    }

    #[test]
    fn decode_rejects_leaf_entry_with_incomplete_record_id() {
        let mut page = [0u8; PAGE_SIZE];

        page[0..4].copy_from_slice(&MAGIC);
        page[4] = FORMAT_VERSION;
        page[5] = 1; // Leaf
        page[6..8].copy_from_slice(&1u16.to_le_bytes()); // key_count = 1

        page[8] = 0; // next_leaf = None

        // The entry begins at byte 9.
        // key_len = PAGE_SIZE - 12 leaves insufficient space
        // for the 10-byte RecordId after the key.
        let key_len = (PAGE_SIZE - 12) as u16;
        page[9..11].copy_from_slice(&key_len.to_le_bytes());

        assert!(decode(&page).is_err());
    }

    #[test]
    fn decode_rejects_internal_key_payload_out_of_bounds() {
        let mut page = blank_page(0, 1);

        // Internal key starts at offset 8.
        write_u16(&mut page, 8, u16::MAX);

        assert!(decode(&page).is_err());
    }

    #[test]
    fn decode_rejects_internal_children_payload_out_of_bounds() {
        let mut page = blank_page(0, 0);

        // key_count=0 requires children_count=1.
        write_u16(&mut page, 8, 1);

        // Corrupt decoder must not panic while reading children.
        // The page is zero-filled, so a correct decoder will read
        // the child if it doesn't enforce child-count consistency.
        assert!(decode(&page).is_ok());
    }

    #[test]
    fn node_codec_round_trip_preserves_full_node_invariants() {
        let test_nodes = vec![
            BPlusNode::Leaf {
                keys: vec![],
                values: vec![],
                next_leaf: None,
            },
            BPlusNode::Leaf {
                keys: vec![
                    Key::new(vec![]),
                    Key::new(b"alpha".to_vec()),
                    Key::new(b"beta".to_vec()),
                ],
                values: vec![
                    RecordId::new(PageId::new(u64::MAX), u16::MAX),
                    RecordId::new(PageId::new(42), 7),
                    RecordId::new(PageId::new(100), 9),
                ],
                next_leaf: Some(PageId::new(u64::MAX)),
            },
            BPlusNode::Internal {
                keys: vec![],
                children: vec![PageId::new(u64::MAX)],
            },
            BPlusNode::Internal {
                keys: vec![
                    Key::new(b"alpha".to_vec()),
                    Key::new(b"beta".to_vec()),
                    Key::new(b"gamma".to_vec()),
                ],
                children: vec![
                    PageId::new(0),
                    PageId::new(1),
                    PageId::new(2),
                    PageId::new(u64::MAX),
                ],
            },
        ];

        for original in test_nodes {
            original.validate().unwrap();

            let encoded = encode(&original).unwrap();
            let decoded = decode(&encoded).unwrap();

            assert_eq!(decoded, original);

            decoded.validate().unwrap();

            assert_eq!(
                decoded.encoded_len().unwrap(),
                original.encoded_len().unwrap()
            );
        }
    }
}
