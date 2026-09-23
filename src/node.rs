use crate::error::BPlusTreeError;
use crate::node_codec::HEADER_SIZE;
use crate::page_id::PageId;
use crate::utilities::has_duplicates;
use crate::{key::Key, record_id::RecordId};

#[derive(Debug, PartialEq, Eq)]
pub enum BPlusNode {
    Leaf {
        keys: Vec<Key>,
        values: Vec<RecordId>,
        next_leaf: Option<PageId>,
    },
    Internal {
        keys: Vec<Key>,
        children: Vec<PageId>,
    },
}

impl BPlusNode {
    pub fn validate_with_page_id(&self, page_id: PageId) -> Result<(), BPlusTreeError> {
        match self {
            BPlusNode::Leaf {
                keys,
                values,
                next_leaf: _,
            } => {
                if keys.len() != values.len() {
                    return Err(BPlusTreeError::CorruptNode {
                        page_id,
                        reason: String::from("Keys and Values length mismatch."),
                    });
                }
                if !keys.is_sorted() {
                    return Err(BPlusTreeError::CorruptNode {
                        page_id,
                        reason: String::from("Keys aren't sorted."),
                    });
                }
                if has_duplicates(keys) {
                    return Err(BPlusTreeError::CorruptNode {
                        page_id,
                        reason: String::from("Keys must be unique."),
                    });
                }
                Ok(())
            }
            BPlusNode::Internal { keys, children } => {
                if children.len() != keys.len().checked_add(1).unwrap_or(0) {
                    return Err(BPlusTreeError::CorruptNode {
                        page_id,
                        reason: String::from("Keys and Children length mismatch."),
                    });
                }
                if !keys.is_sorted() {
                    return Err(BPlusTreeError::CorruptNode {
                        page_id,
                        reason: String::from("Keys aren't sorted."),
                    });
                }
                if has_duplicates(keys) {
                    return Err(BPlusTreeError::CorruptNode {
                        page_id,
                        reason: String::from("Keys must be unique."),
                    });
                }
                if children.is_empty() {
                    return Err(BPlusTreeError::CorruptNode {
                        page_id,
                        reason: String::from("Atleast one child is required."),
                    });
                }
                Ok(())
            }
        }
    }

    pub fn validate(&self) -> Result<(), BPlusTreeError> {
        match self {
            BPlusNode::Leaf {
                keys,
                values,
                next_leaf: _,
            } => {
                if keys.len() != values.len() {
                    return Err(BPlusTreeError::InvalidNode {
                        reason: String::from("Keys and Values length mismatch."),
                    });
                }
                if !keys.is_sorted() {
                    return Err(BPlusTreeError::InvalidNode {
                        reason: String::from("Keys aren't sorted."),
                    });
                }
                if has_duplicates(keys) {
                    return Err(BPlusTreeError::InvalidNode {
                        reason: String::from("Keys must be unique."),
                    });
                }
                Ok(())
            }
            BPlusNode::Internal { keys, children } => {
                if children.len() != keys.len().checked_add(1).unwrap_or(0) {
                    return Err(BPlusTreeError::InvalidNode {
                        reason: String::from("Keys and Children length mismatch."),
                    });
                }
                if !keys.is_sorted() {
                    return Err(BPlusTreeError::InvalidNode {
                        reason: String::from("Keys aren't sorted."),
                    });
                }
                if has_duplicates(keys) {
                    return Err(BPlusTreeError::InvalidNode {
                        reason: String::from("Keys must be unique."),
                    });
                }
                if children.is_empty() {
                    return Err(BPlusTreeError::InvalidNode {
                        reason: String::from("Atleast one child is required."),
                    });
                }
                Ok(())
            }
        }
    }
    pub fn encoded_len(&self) -> Result<usize, BPlusTreeError> {
        self.validate()?;
        let mut len = 0usize;
        len = len
            .checked_add(HEADER_SIZE)
            .ok_or(BPlusTreeError::SizeOverflow)?; // 4 bytes MAGIC + 1 byte FORMAT + 1 byte Node variant

        match self {
            BPlusNode::Leaf {
                keys,
                values: _,
                next_leaf,
            } => {
                if next_leaf.is_some() {
                    len = len.checked_add(9).ok_or(BPlusTreeError::SizeOverflow)?;
                // next_leaf with it's data PageId
                } else {
                    len = len.checked_add(1).ok_or(BPlusTreeError::SizeOverflow)?;
                    // next_leaf
                }
                len = len.checked_add(2).ok_or(BPlusTreeError::SizeOverflow)?; // key_count
                for i in keys {
                    len = len.checked_add(2).ok_or(BPlusTreeError::SizeOverflow)?; // key_len
                    len = len
                        .checked_add(i.as_bytes().len())
                        .ok_or(BPlusTreeError::SizeOverflow)?; // key_bytes
                    len = len.checked_add(10).ok_or(BPlusTreeError::SizeOverflow)?;
                    // record
                }
                Ok(len)
            }
            BPlusNode::Internal { keys, children } => {
                len = len.checked_add(2).ok_or(BPlusTreeError::SizeOverflow)?; // key_count
                for i in keys {
                    len = len.checked_add(2).ok_or(BPlusTreeError::SizeOverflow)?; // key_len
                    len = len
                        .checked_add(i.as_bytes().len())
                        .ok_or(BPlusTreeError::SizeOverflow)?; // key_bytes
                }
                len = len.checked_add(2).ok_or(BPlusTreeError::SizeOverflow)?; //children_count
                for _ in children {
                    len = len.checked_add(8).ok_or(BPlusTreeError::SizeOverflow)?;
                    //children_bytes
                }
                Ok(len)
            }
        }
    }
}

#[cfg(test)]
mod node_tests {
    use super::*;

    #[test]
    fn leaf_node_stores_keys_and_values() {
        let keys = vec![Key::new(b"alpha".to_vec()), Key::new(b"beta".to_vec())];
        let values = vec![
            RecordId::new(PageId::new(1), 0),
            RecordId::new(PageId::new(1), 1),
        ];

        let node = BPlusNode::Leaf {
            keys: keys.clone(),
            values: values.clone(),
            next_leaf: None,
        };

        match node {
            BPlusNode::Leaf {
                keys: actual_keys,
                values: actual_values,
                next_leaf,
            } => {
                assert_eq!(actual_keys, keys);
                assert_eq!(actual_values, values);
                assert_eq!(next_leaf, None);
            }
            _ => panic!("expected leaf node"),
        }
    }

    #[test]
    fn leaf_node_can_be_empty() {
        let node = BPlusNode::Leaf {
            keys: vec![],
            values: vec![],
            next_leaf: None,
        };

        assert!(matches!(
            node,
            BPlusNode::Leaf {
                keys,
                values,
                next_leaf: None
            } if keys.is_empty() && values.is_empty()
        ));
    }

    #[test]
    fn leaf_node_stores_next_leaf_page_id() {
        let next = PageId::new(42);

        let node = BPlusNode::Leaf {
            keys: vec![],
            values: vec![],
            next_leaf: Some(next),
        };

        assert!(matches!(
            node,
            BPlusNode::Leaf {
                next_leaf: Some(page_id),
                ..
            } if page_id == next
        ));
    }

    #[test]
    fn leaf_node_can_have_no_next_leaf() {
        let node = BPlusNode::Leaf {
            keys: vec![],
            values: vec![],
            next_leaf: None,
        };

        assert!(matches!(
            node,
            BPlusNode::Leaf {
                next_leaf: None,
                ..
            }
        ));
    }

    #[test]
    fn internal_node_stores_keys_and_children() {
        let keys = vec![Key::new(b"m".to_vec())];
        let children = vec![PageId::new(10), PageId::new(20)];

        let node = BPlusNode::Internal {
            keys: keys.clone(),
            children: children.clone(),
        };

        match node {
            BPlusNode::Internal {
                keys: actual_keys,
                children: actual_children,
            } => {
                assert_eq!(actual_keys, keys);
                assert_eq!(actual_children, children);
            }
            _ => panic!("expected internal node"),
        }
    }

    #[test]
    fn internal_node_can_have_zero_keys_and_one_child() {
        let child = PageId::new(7);

        let node = BPlusNode::Internal {
            keys: vec![],
            children: vec![child],
        };

        assert!(matches!(
            node,
            BPlusNode::Internal { keys, children }
                if keys.is_empty() && children == vec![child]
        ));
    }

    #[test]
    fn internal_node_preserves_child_order() {
        let children = vec![PageId::new(30), PageId::new(10), PageId::new(20)];

        let node = BPlusNode::Internal {
            keys: vec![],
            children: children.clone(),
        };

        assert!(matches!(
            node,
            BPlusNode::Internal { children: actual, .. }
                if actual == children
        ));
    }

    #[test]
    fn leaf_node_preserves_record_id_associations() {
        let key = Key::new(b"key".to_vec());
        let record_id = RecordId::new(PageId::new(9), 4);

        let node = BPlusNode::Leaf {
            keys: vec![key.clone()],
            values: vec![record_id],
            next_leaf: None,
        };

        assert!(matches!(
            node,
            BPlusNode::Leaf { keys, values, .. }
                if keys[0] == key && values[0] == record_id
        ));
    }

    #[test]
    fn node_variants_are_distinguishable() {
        let leaf = BPlusNode::Leaf {
            keys: vec![],
            values: vec![],
            next_leaf: None,
        };

        let internal = BPlusNode::Internal {
            keys: vec![],
            children: vec![PageId::new(1)],
        };

        assert!(matches!(leaf, BPlusNode::Leaf { .. }));
        assert!(matches!(internal, BPlusNode::Internal { .. }));
    }
}

#[cfg(test)]
mod validation_tests {
    use crate::BPlusNode;
    use crate::error::BPlusTreeError;
    use crate::key::Key;
    use crate::page_id::PageId;
    use crate::record_id::RecordId;

    fn key(bytes: &[u8]) -> Key {
        Key::new(bytes.to_vec())
    }

    fn page_id() -> PageId {
        PageId::new(7)
    }

    fn record_id(slot: u16) -> RecordId {
        RecordId::new(PageId::new(100), slot)
    }

    fn assert_corrupt_node(result: Result<(), BPlusTreeError>, expected_page_id: PageId) {
        match result {
            Err(BPlusTreeError::CorruptNode { page_id, reason: _ }) => {
                assert_eq!(page_id, expected_page_id);
            }
            other => panic!("expected CorruptNode error, got {other:?}"),
        }
    }

    #[test]
    fn valid_empty_leaf_passes_validation() {
        let node = BPlusNode::Leaf {
            keys: vec![],
            values: vec![],
            next_leaf: None,
        };

        assert!(node.validate().is_ok());
    }

    #[test]
    fn valid_leaf_with_sorted_keys_passes_validation() {
        let node = BPlusNode::Leaf {
            keys: vec![key(b"a"), key(b"b"), key(b"c")],
            values: vec![record_id(0), record_id(1), record_id(2)],
            next_leaf: None,
        };

        assert!(node.validate().is_ok());
    }

    #[test]
    fn valid_leaf_with_next_leaf_passes_validation() {
        let node = BPlusNode::Leaf {
            keys: vec![key(b"a")],
            values: vec![record_id(0)],
            next_leaf: Some(PageId::new(8)),
        };

        assert!(node.validate().is_ok());
    }

    #[test]
    fn leaf_with_mismatched_key_value_counts_is_rejected() {
        let node = BPlusNode::Leaf {
            keys: vec![key(b"a"), key(b"b")],
            values: vec![record_id(0)],
            next_leaf: None,
        };

        assert_corrupt_node(node.validate_with_page_id(page_id()), page_id());
    }

    #[test]
    fn leaf_with_unsorted_keys_is_rejected() {
        let node = BPlusNode::Leaf {
            keys: vec![key(b"b"), key(b"a")],
            values: vec![record_id(0), record_id(1)],
            next_leaf: None,
        };

        assert_corrupt_node(node.validate_with_page_id(page_id()), page_id());
    }

    #[test]
    fn leaf_with_duplicate_keys_is_rejected() {
        let node = BPlusNode::Leaf {
            keys: vec![key(b"a"), key(b"a")],
            values: vec![record_id(0), record_id(1)],
            next_leaf: None,
        };

        assert_corrupt_node(node.validate_with_page_id(page_id()), page_id());
    }

    #[test]
    fn valid_zero_key_internal_node_with_one_child_passes() {
        let node = BPlusNode::Internal {
            keys: vec![],
            children: vec![PageId::new(10)],
        };

        assert!(node.validate().is_ok());
    }

    #[test]
    fn valid_internal_node_passes_validation() {
        let node = BPlusNode::Internal {
            keys: vec![key(b"m"), key(b"t")],
            children: vec![PageId::new(10), PageId::new(20), PageId::new(30)],
        };

        assert!(node.validate().is_ok());
    }

    #[test]
    fn internal_node_with_no_children_is_rejected() {
        let node = BPlusNode::Internal {
            keys: vec![],
            children: vec![],
        };

        assert_corrupt_node(node.validate_with_page_id(page_id()), page_id());
    }

    #[test]
    fn internal_node_with_too_few_children_is_rejected() {
        let node = BPlusNode::Internal {
            keys: vec![key(b"m"), key(b"t")],
            children: vec![PageId::new(10), PageId::new(20)],
        };

        assert_corrupt_node(node.validate_with_page_id(page_id()), page_id());
    }

    #[test]
    fn internal_node_with_too_many_children_is_rejected() {
        let node = BPlusNode::Internal {
            keys: vec![key(b"m")],
            children: vec![PageId::new(10), PageId::new(20), PageId::new(30)],
        };

        assert_corrupt_node(node.validate_with_page_id(page_id()), page_id());
    }

    #[test]
    fn internal_node_with_unsorted_keys_is_rejected() {
        let node = BPlusNode::Internal {
            keys: vec![key(b"t"), key(b"m")],
            children: vec![PageId::new(10), PageId::new(20), PageId::new(30)],
        };

        assert_corrupt_node(node.validate_with_page_id(page_id()), page_id());
    }

    #[test]
    fn internal_node_with_duplicate_keys_is_rejected() {
        let node = BPlusNode::Internal {
            keys: vec![key(b"m"), key(b"m")],
            children: vec![PageId::new(10), PageId::new(20), PageId::new(30)],
        };

        assert_corrupt_node(node.validate_with_page_id(page_id()), page_id());
    }

    #[test]
    fn validation_error_contains_the_supplied_page_id() {
        let node = BPlusNode::Internal {
            keys: vec![],
            children: vec![],
        };

        let expected_page_id = PageId::new(123);

        assert_corrupt_node(
            node.validate_with_page_id(expected_page_id),
            expected_page_id,
        );
    }
}
