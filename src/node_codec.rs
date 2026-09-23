use crate::{BPlusNode, error::BPlusTreeError, slotted_page::PAGE_SIZE};

pub const MAGIC: [u8; 4] = *b"BTR4";
pub const FORMAT_VERSION: u8 = 1;
pub const HEADER_SIZE: usize = 6; // 4 bytes MAGIC + 1 byte FORMAT + 1 byte Node variant

pub fn encode(node: &BPlusNode) -> Result<[u8; PAGE_SIZE], BPlusTreeError> {
    let node_size = node.encoded_len()?;
    if node_size > PAGE_SIZE {
        return Err(BPlusTreeError::NodeTooLarge {
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
                buffer[cursor..cursor.checked_add(2).ok_or(BPlusTreeError::SizeOverflow)?]
                    .copy_from_slice(&key_len.to_le_bytes()); // key_len
                cursor = cursor.checked_add(2).ok_or(BPlusTreeError::SizeOverflow)?;
                buffer[cursor
                    ..cursor
                        .checked_add(key_len as usize)
                        .ok_or(BPlusTreeError::SizeOverflow)?]
                    .copy_from_slice(key.as_bytes()); // key_bytes
                cursor = cursor
                    .checked_add(key_len as usize)
                    .ok_or(BPlusTreeError::SizeOverflow)?;
                buffer[cursor..cursor.checked_add(8).ok_or(BPlusTreeError::SizeOverflow)?]
                    .copy_from_slice(&val.page_id().get().to_le_bytes()); // page_id
                cursor = cursor.checked_add(8).ok_or(BPlusTreeError::SizeOverflow)?;
                buffer[cursor..cursor.checked_add(2).ok_or(BPlusTreeError::SizeOverflow)?]
                    .copy_from_slice(&val.slot_id().to_le_bytes()); // slot_id
                cursor = cursor.checked_add(2).ok_or(BPlusTreeError::SizeOverflow)?;
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
                buffer[cursor..cursor.checked_add(2).ok_or(BPlusTreeError::SizeOverflow)?]
                    .copy_from_slice(&key_len.to_le_bytes()); // key_len
                cursor = cursor.checked_add(2).ok_or(BPlusTreeError::SizeOverflow)?;
                buffer[cursor
                    ..cursor
                        .checked_add(key_len as usize)
                        .ok_or(BPlusTreeError::SizeOverflow)?]
                    .copy_from_slice(key.as_bytes()); // key_bytes
                cursor = cursor
                    .checked_add(key_len as usize)
                    .ok_or(BPlusTreeError::SizeOverflow)?;
            }
            buffer[cursor..cursor.checked_add(2).ok_or(BPlusTreeError::SizeOverflow)?]
                .copy_from_slice(
                    &(key_count
                        .checked_add(1)
                        .ok_or(BPlusTreeError::SizeOverflow)?)
                    .to_le_bytes(),
                ); // children_count
            cursor = cursor.checked_add(2).ok_or(BPlusTreeError::SizeOverflow)?;
            for child in children {
                let child = child.get();
                buffer[cursor..cursor.checked_add(8).ok_or(BPlusTreeError::SizeOverflow)?]
                    .copy_from_slice(&child.to_le_bytes()); // child
                cursor = cursor.checked_add(8).ok_or(BPlusTreeError::SizeOverflow)?;
            }
            Ok(buffer)
        }
    }
}

#[cfg(test)]
mod node_codec_encode_tests {
    use super::*;
    use crate::error::BPlusTreeError;
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
            Err(BPlusTreeError::NodeTooLarge { .. })
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
            Err(BPlusTreeError::CorruptNode { page_id, .. })
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
            Err(BPlusTreeError::InvalidNode { .. })
        ));
    }
}
