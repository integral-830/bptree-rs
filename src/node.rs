use crate::page_id::PageId;
use crate::{key::Key, record_id::RecordId};

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
