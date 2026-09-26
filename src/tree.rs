use crate::error::BPlusTreeError;
use crate::key::Key;
use crate::node_store::NodeStore;
use crate::page_id::PageId;
use crate::record_id::RecordId;
use crate::slotted_page::PAGE_SIZE;
use crate::BPlusNode;

pub struct BPlusTree<S: NodeStore> {
    store: S,
    root_page_id: PageId,
}

impl<S: NodeStore> BPlusTree<S> {
    pub fn new(mut store: S) -> Result<Self, BPlusTreeError<S::Error>> {
        let node = BPlusNode::Leaf {
            keys: Vec::new(),
            values: Vec::new(),
            next_leaf: None,
        };
        let allocated_root_id = store
            .allocate_node(&node)
            .map_err(BPlusTreeError::Storage)?;
        Ok(Self {
            store,
            root_page_id: allocated_root_id,
        })
    }

    pub fn scan(&self) -> Result<Vec<(Key, RecordId)>, BPlusTreeError<S::Error>> {
        let mut page_id = self.root_page_id;

        // Descend to the leftmost leaf.
        loop {
            let node = self.store.read_node(page_id)?;

            match node {
                BPlusNode::Internal { children, .. } => {
                    let Some(&first_child) = children.first() else {
                        return Err(BPlusTreeError::InvalidTreeState(
                            "Internal node has no children.",
                        ));
                    };

                    page_id = first_child;
                }

                BPlusNode::Leaf { .. } => break,
            }
        }

        let mut entries = Vec::new();

        loop {
            let node = self.store.read_node(page_id)?;

            match node {
                BPlusNode::Leaf {
                    keys,
                    values,
                    next_leaf,
                } => {
                    entries.extend(keys.into_iter().zip(values));

                    match next_leaf {
                        Some(next_page_id) => {
                            page_id = next_page_id;
                        }
                        None => break,
                    }
                }

                BPlusNode::Internal { .. } => {
                    return Err(BPlusTreeError::InvalidTreeState(
                        "expected a leaf node in the leaf chain, found an internal node",
                    ));
                }
            }
        }

        Ok(entries)
    }

    pub fn search(&self, key: &Key) -> Result<Option<RecordId>, BPlusTreeError<S::Error>> {
        let (_, node) = self.find_leaf(key)?;

        match node {
            BPlusNode::Leaf { keys, values, .. } => {
                Ok(keys.binary_search(key).ok().map(|i| values[i]))
            }
            BPlusNode::Internal { .. } => {
                unreachable!("find_leaf must return a leaf")
            }
        }
    }

    fn find_leaf(&self, key: &Key) -> Result<(PageId, BPlusNode), BPlusTreeError<S::Error>> {
        let mut page_id = self.root_page_id;

        loop {
            let node = self.store.read_node(page_id)?;

            match node {
                BPlusNode::Internal { keys, children } => {
                    debug_assert_eq!(children.len(), keys.len() + 1);

                    let child_index = keys.partition_point(|separator| separator <= key);

                    page_id = children[child_index];
                }

                leaf @ BPlusNode::Leaf { .. } => {
                    return Ok((page_id, leaf));
                }
            }
        }
    }
    pub fn insert(
        &mut self,
        key: Key,
        record_id: RecordId,
    ) -> Result<(), BPlusTreeError<S::Error>> {
        let (page_id, node) = self.find_leaf(&key)?;

        let BPlusNode::Leaf {
            mut keys,
            mut values,
            next_leaf,
        } = node
        else {
            return Err(BPlusTreeError::InvalidTreeState(
                "find_leaf returned an internal node",
            ));
        };

        let insert_index = keys.partition_point(|existing_key| existing_key < &key);

        if keys
            .get(insert_index)
            .is_some_and(|existing_key| existing_key == &key)
        {
            return Err(BPlusTreeError::DuplicateKey);
        }

        keys.insert(insert_index, key);
        values.insert(insert_index, record_id);

        let updated_leaf = BPlusNode::Leaf {
            keys,
            values,
            next_leaf,
        };

        if updated_leaf.encoded_len().map_err(BPlusTreeError::Node)? > PAGE_SIZE {
            return Err(BPlusTreeError::LeafFull);
        }

        self.store.write_node(page_id, &updated_leaf)?;

        Ok(())
    }
}

#[cfg(test)]
impl<S: NodeStore> BPlusTree<S> {
    fn from_root(store: S, root_page_id: PageId) -> Self {
        Self {
            store,
            root_page_id,
        }
    }
}

#[cfg(test)]
pub mod tree_tests {
    use crate::error::BPlusTreeError;
    use crate::key::Key;
    use crate::node_store::NodeStore;
    use crate::page_id::PageId;
    use crate::record_id::RecordId;
    use crate::storage::memory::MemoryNodeStore;
    use crate::tree::BPlusTree;
    use crate::BPlusNode;

    fn key(value: &str) -> Key {
        Key::new(value.as_bytes().to_vec())
    }

    fn record(page_id: u64, slot_id: u16) -> RecordId {
        RecordId::new(PageId::new(page_id), slot_id)
    }

    #[test]
    fn new_tree_scan_is_empty() {
        let store = MemoryNodeStore::new();

        let tree = BPlusTree::new(store).unwrap();

        let result = tree.scan().unwrap();

        assert!(result.is_empty());
    }

    #[test]
    fn scan_single_leaf() {
        let mut store = MemoryNodeStore::new();

        let leaf = BPlusNode::Leaf {
            keys: vec![key("a"), key("b"), key("c")],
            values: vec![record(1, 0), record(1, 1), record(1, 2)],
            next_leaf: None,
        };

        let root_page_id = store.allocate_node(&leaf).unwrap();

        let tree = BPlusTree::from_root(store, root_page_id);

        let result = tree.scan().unwrap();

        assert_eq!(
            result,
            vec![
                (key("a"), record(1, 0)),
                (key("b"), record(1, 1)),
                (key("c"), record(1, 2)),
            ]
        );
    }

    #[test]
    fn scan_multiple_leaf_chain() {
        let mut store = MemoryNodeStore::new();

        let leaf3 = BPlusNode::Leaf {
            keys: vec![key("e"), key("f")],
            values: vec![record(3, 0), record(3, 1)],
            next_leaf: None,
        };

        let leaf3_id = store.allocate_node(&leaf3).unwrap();

        let leaf2 = BPlusNode::Leaf {
            keys: vec![key("c"), key("d")],
            values: vec![record(2, 0), record(2, 1)],
            next_leaf: Some(leaf3_id),
        };

        let leaf2_id = store.allocate_node(&leaf2).unwrap();

        let leaf1 = BPlusNode::Leaf {
            keys: vec![key("a"), key("b")],
            values: vec![record(1, 0), record(1, 1)],
            next_leaf: Some(leaf2_id),
        };

        let leaf1_id = store.allocate_node(&leaf1).unwrap();

        let tree = BPlusTree::from_root(store, leaf1_id);

        let result = tree.scan().unwrap();

        assert_eq!(
            result,
            vec![
                (key("a"), record(1, 0)),
                (key("b"), record(1, 1)),
                (key("c"), record(2, 0)),
                (key("d"), record(2, 1)),
                (key("e"), record(3, 0)),
                (key("f"), record(3, 1)),
            ]
        );
    }

    #[test]
    fn scan_descends_to_leftmost_leaf() {
        let mut store = MemoryNodeStore::new();

        let left_leaf = BPlusNode::Leaf {
            keys: vec![key("a"), key("b")],
            values: vec![record(1, 0), record(1, 1)],
            next_leaf: None,
        };

        let left_leaf_id = store.allocate_node(&left_leaf).unwrap();

        let right_leaf = BPlusNode::Leaf {
            keys: vec![key("c"), key("d")],
            values: vec![record(2, 0), record(2, 1)],
            next_leaf: None,
        };

        let right_leaf_id = store.allocate_node(&right_leaf).unwrap();

        // The leaf chain must represent the complete ordered sequence.
        let left_leaf = BPlusNode::Leaf {
            keys: vec![key("a"), key("b")],
            values: vec![record(1, 0), record(1, 1)],
            next_leaf: Some(right_leaf_id),
        };

        store.write_node(left_leaf_id, &left_leaf).unwrap();

        let root = BPlusNode::Internal {
            keys: vec![key("c")],
            children: vec![left_leaf_id, right_leaf_id],
        };

        let root_id = store.allocate_node(&root).unwrap();

        let tree = BPlusTree::from_root(store, root_id);

        let result = tree.scan().unwrap();

        assert_eq!(
            result,
            vec![
                (key("a"), record(1, 0)),
                (key("b"), record(1, 1)),
                (key("c"), record(2, 0)),
                (key("d"), record(2, 1)),
            ]
        );
    }

    #[test]
    fn scan_preserves_global_sorted_order() {
        let mut store = MemoryNodeStore::new();

        let leaf3 = BPlusNode::Leaf {
            keys: vec![key("e"), key("f")],
            values: vec![record(3, 0), record(3, 1)],
            next_leaf: None,
        };

        let leaf3_id = store.allocate_node(&leaf3).unwrap();

        let leaf2 = BPlusNode::Leaf {
            keys: vec![key("c"), key("d")],
            values: vec![record(2, 0), record(2, 1)],
            next_leaf: Some(leaf3_id),
        };

        let leaf2_id = store.allocate_node(&leaf2).unwrap();

        let leaf1 = BPlusNode::Leaf {
            keys: vec![key("a"), key("b")],
            values: vec![record(1, 0), record(1, 1)],
            next_leaf: Some(leaf2_id),
        };

        let leaf1_id = store.allocate_node(&leaf1).unwrap();

        let tree = BPlusTree::from_root(store, leaf1_id);

        let result = tree.scan().unwrap();

        let keys: Vec<Key> = result.into_iter().map(|(key, _)| key).collect();

        assert_eq!(
            keys,
            vec![key("a"), key("b"), key("c"), key("d"), key("e"), key("f"),]
        );
    }

    #[test]
    fn scan_handles_empty_leaf_between_non_empty_leaves() {
        let mut store = MemoryNodeStore::new();

        let leaf3 = BPlusNode::Leaf {
            keys: vec![key("c")],
            values: vec![record(3, 0)],
            next_leaf: None,
        };

        let leaf3_id = store.allocate_node(&leaf3).unwrap();

        let leaf2 = BPlusNode::Leaf {
            keys: vec![],
            values: vec![],
            next_leaf: Some(leaf3_id),
        };

        let leaf2_id = store.allocate_node(&leaf2).unwrap();

        let leaf1 = BPlusNode::Leaf {
            keys: vec![key("a")],
            values: vec![record(1, 0)],
            next_leaf: Some(leaf2_id),
        };

        let leaf1_id = store.allocate_node(&leaf1).unwrap();

        let tree = BPlusTree::from_root(store, leaf1_id);

        let result = tree.scan().unwrap();

        assert_eq!(
            result,
            vec![(key("a"), record(1, 0)), (key("c"), record(3, 0)),]
        );
    }

    #[test]
    fn scan_handles_multiple_internal_levels() {
        let mut store = MemoryNodeStore::new();

        let leaf4 = BPlusNode::Leaf {
            keys: vec![key("g"), key("h")],
            values: vec![record(4, 0), record(4, 1)],
            next_leaf: None,
        };

        let leaf4_id = store.allocate_node(&leaf4).unwrap();

        let leaf3 = BPlusNode::Leaf {
            keys: vec![key("e"), key("f")],
            values: vec![record(3, 0), record(3, 1)],
            next_leaf: Some(leaf4_id),
        };

        let leaf3_id = store.allocate_node(&leaf3).unwrap();

        let leaf2 = BPlusNode::Leaf {
            keys: vec![key("c"), key("d")],
            values: vec![record(2, 0), record(2, 1)],
            next_leaf: Some(leaf3_id),
        };

        let leaf2_id = store.allocate_node(&leaf2).unwrap();

        let leaf1 = BPlusNode::Leaf {
            keys: vec![key("a"), key("b")],
            values: vec![record(1, 0), record(1, 1)],
            next_leaf: Some(leaf2_id),
        };

        let leaf1_id = store.allocate_node(&leaf1).unwrap();

        let left_internal = BPlusNode::Internal {
            keys: vec![key("c")],
            children: vec![leaf1_id, leaf2_id],
        };

        let left_internal_id = store.allocate_node(&left_internal).unwrap();

        let right_internal = BPlusNode::Internal {
            keys: vec![key("g")],
            children: vec![leaf3_id, leaf4_id],
        };

        let right_internal_id = store.allocate_node(&right_internal).unwrap();

        let root = BPlusNode::Internal {
            keys: vec![key("e")],
            children: vec![left_internal_id, right_internal_id],
        };

        let root_id = store.allocate_node(&root).unwrap();

        let tree = BPlusTree::from_root(store, root_id);

        let result = tree.scan().unwrap();

        assert_eq!(
            result,
            vec![
                (key("a"), record(1, 0)),
                (key("b"), record(1, 1)),
                (key("c"), record(2, 0)),
                (key("d"), record(2, 1)),
                (key("e"), record(3, 0)),
                (key("f"), record(3, 1)),
                (key("g"), record(4, 0)),
                (key("h"), record(4, 1)),
            ]
        );
    }
    #[test]
    fn search_empty_tree_returns_none() {
        let store = MemoryNodeStore::new();
        let tree = BPlusTree::new(store).unwrap();

        let result = tree.search(&key("missing")).unwrap();

        assert_eq!(result, None);
    }

    #[test]
    fn search_existing_key_returns_record_id() {
        let mut store = MemoryNodeStore::new();

        let leaf = BPlusNode::Leaf {
            keys: vec![key("a"), key("b"), key("c")],
            values: vec![record(1, 0), record(1, 1), record(1, 2)],
            next_leaf: None,
        };

        let root_id = store.allocate_node(&leaf).unwrap();

        let tree = BPlusTree::from_root(store, root_id);

        assert_eq!(tree.search(&key("a")).unwrap(), Some(record(1, 0)));

        assert_eq!(tree.search(&key("b")).unwrap(), Some(record(1, 1)));

        assert_eq!(tree.search(&key("c")).unwrap(), Some(record(1, 2)));
    }

    #[test]
    fn search_missing_key_returns_none() {
        let mut store = MemoryNodeStore::new();

        let leaf = BPlusNode::Leaf {
            keys: vec![key("a"), key("c"), key("e")],
            values: vec![record(1, 0), record(1, 1), record(1, 2)],
            next_leaf: None,
        };

        let root_id = store.allocate_node(&leaf).unwrap();

        let tree = BPlusTree::from_root(store, root_id);

        assert_eq!(tree.search(&key("b")).unwrap(), None);
        assert_eq!(tree.search(&key("d")).unwrap(), None);
        assert_eq!(tree.search(&key("f")).unwrap(), None);
    }

    #[test]
    fn search_routes_to_left_child() {
        let mut store = MemoryNodeStore::new();

        let left_leaf = BPlusNode::Leaf {
            keys: vec![key("a"), key("b")],
            values: vec![record(1, 0), record(1, 1)],
            next_leaf: None,
        };

        let left_id = store.allocate_node(&left_leaf).unwrap();

        let right_leaf = BPlusNode::Leaf {
            keys: vec![key("d"), key("e")],
            values: vec![record(2, 0), record(2, 1)],
            next_leaf: None,
        };

        let right_id = store.allocate_node(&right_leaf).unwrap();

        let root = BPlusNode::Internal {
            keys: vec![key("d")],
            children: vec![left_id, right_id],
        };

        let root_id = store.allocate_node(&root).unwrap();

        let tree = BPlusTree::from_root(store, root_id);

        assert_eq!(tree.search(&key("a")).unwrap(), Some(record(1, 0)));

        assert_eq!(tree.search(&key("b")).unwrap(), Some(record(1, 1)));
    }

    #[test]
    fn search_routes_to_middle_child() {
        let mut store = MemoryNodeStore::new();

        let left_leaf = BPlusNode::Leaf {
            keys: vec![key("a")],
            values: vec![record(1, 0)],
            next_leaf: None,
        };

        let left_id = store.allocate_node(&left_leaf).unwrap();

        let middle_leaf = BPlusNode::Leaf {
            keys: vec![key("c"), key("d")],
            values: vec![record(2, 0), record(2, 1)],
            next_leaf: None,
        };

        let middle_id = store.allocate_node(&middle_leaf).unwrap();

        let right_leaf = BPlusNode::Leaf {
            keys: vec![key("f")],
            values: vec![record(3, 0)],
            next_leaf: None,
        };

        let right_id = store.allocate_node(&right_leaf).unwrap();

        let root = BPlusNode::Internal {
            keys: vec![key("c"), key("f")],
            children: vec![left_id, middle_id, right_id],
        };

        let root_id = store.allocate_node(&root).unwrap();

        let tree = BPlusTree::from_root(store, root_id);

        assert_eq!(tree.search(&key("c")).unwrap(), Some(record(2, 0)));

        assert_eq!(tree.search(&key("d")).unwrap(), Some(record(2, 1)));
    }

    #[test]
    fn search_routes_to_right_child() {
        let mut store = MemoryNodeStore::new();

        let left_leaf = BPlusNode::Leaf {
            keys: vec![key("a")],
            values: vec![record(1, 0)],
            next_leaf: None,
        };

        let left_id = store.allocate_node(&left_leaf).unwrap();

        let right_leaf = BPlusNode::Leaf {
            keys: vec![key("z")],
            values: vec![record(2, 0)],
            next_leaf: None,
        };

        let right_id = store.allocate_node(&right_leaf).unwrap();

        let root = BPlusNode::Internal {
            keys: vec![key("m")],
            children: vec![left_id, right_id],
        };

        let root_id = store.allocate_node(&root).unwrap();

        let tree = BPlusTree::from_root(store, root_id);

        assert_eq!(tree.search(&key("z")).unwrap(), Some(record(2, 0)));
    }

    #[test]
    fn search_separator_equality_routes_right() {
        let mut store = MemoryNodeStore::new();

        // If equality incorrectly routes left, this test will fail.
        let left_leaf = BPlusNode::Leaf {
            keys: vec![key("a"), key("m")],
            values: vec![record(1, 0), record(1, 1)],
            next_leaf: None,
        };

        let left_id = store.allocate_node(&left_leaf).unwrap();

        let right_leaf = BPlusNode::Leaf {
            keys: vec![key("m"), key("z")],
            values: vec![record(2, 0), record(2, 1)],
            next_leaf: None,
        };

        let right_id = store.allocate_node(&right_leaf).unwrap();

        let root = BPlusNode::Internal {
            keys: vec![key("m")],
            children: vec![left_id, right_id],
        };

        let root_id = store.allocate_node(&root).unwrap();

        let tree = BPlusTree::from_root(store, root_id);

        assert_eq!(tree.search(&key("m")).unwrap(), Some(record(2, 0)));
    }

    #[test]
    fn search_handles_key_smaller_than_all_separators() {
        let mut store = MemoryNodeStore::new();

        let left_leaf = BPlusNode::Leaf {
            keys: vec![key("a")],
            values: vec![record(1, 0)],
            next_leaf: None,
        };

        let left_id = store.allocate_node(&left_leaf).unwrap();

        let middle_leaf = BPlusNode::Leaf {
            keys: vec![key("m")],
            values: vec![record(2, 0)],
            next_leaf: None,
        };

        let middle_id = store.allocate_node(&middle_leaf).unwrap();

        let right_leaf = BPlusNode::Leaf {
            keys: vec![key("z")],
            values: vec![record(3, 0)],
            next_leaf: None,
        };

        let right_id = store.allocate_node(&right_leaf).unwrap();

        let root = BPlusNode::Internal {
            keys: vec![key("m"), key("z")],
            children: vec![left_id, middle_id, right_id],
        };

        let root_id = store.allocate_node(&root).unwrap();

        let tree = BPlusTree::from_root(store, root_id);

        assert_eq!(tree.search(&key("0")).unwrap(), None);
    }

    #[test]
    fn search_handles_key_greater_than_all_separators() {
        let mut store = MemoryNodeStore::new();

        let left_leaf = BPlusNode::Leaf {
            keys: vec![key("a")],
            values: vec![record(1, 0)],
            next_leaf: None,
        };

        let left_id = store.allocate_node(&left_leaf).unwrap();

        let middle_leaf = BPlusNode::Leaf {
            keys: vec![key("m")],
            values: vec![record(2, 0)],
            next_leaf: None,
        };

        let middle_id = store.allocate_node(&middle_leaf).unwrap();

        let right_leaf = BPlusNode::Leaf {
            keys: vec![key("z")],
            values: vec![record(3, 0)],
            next_leaf: None,
        };

        let right_id = store.allocate_node(&right_leaf).unwrap();

        let root = BPlusNode::Internal {
            keys: vec![key("m"), key("z")],
            children: vec![left_id, middle_id, right_id],
        };

        let root_id = store.allocate_node(&root).unwrap();

        let tree = BPlusTree::from_root(store, root_id);

        assert_eq!(tree.search(&key("zz")).unwrap(), None);
    }

    #[test]
    fn search_handles_multiple_internal_levels() {
        let mut store = MemoryNodeStore::new();

        let leaf1 = BPlusNode::Leaf {
            keys: vec![key("a"), key("b")],
            values: vec![record(1, 0), record(1, 1)],
            next_leaf: None,
        };

        let leaf1_id = store.allocate_node(&leaf1).unwrap();

        let leaf2 = BPlusNode::Leaf {
            keys: vec![key("c"), key("d")],
            values: vec![record(2, 0), record(2, 1)],
            next_leaf: None,
        };

        let leaf2_id = store.allocate_node(&leaf2).unwrap();

        let leaf3 = BPlusNode::Leaf {
            keys: vec![key("e"), key("f")],
            values: vec![record(3, 0), record(3, 1)],
            next_leaf: None,
        };

        let leaf3_id = store.allocate_node(&leaf3).unwrap();

        let left_internal = BPlusNode::Internal {
            keys: vec![key("c")],
            children: vec![leaf1_id, leaf2_id],
        };

        let left_internal_id = store.allocate_node(&left_internal).unwrap();

        let right_internal = BPlusNode::Internal {
            keys: vec![],
            children: vec![leaf3_id],
        };

        let right_internal_id = store.allocate_node(&right_internal).unwrap();

        let root = BPlusNode::Internal {
            keys: vec![key("e")],
            children: vec![left_internal_id, right_internal_id],
        };

        let root_id = store.allocate_node(&root).unwrap();

        let tree = BPlusTree::from_root(store, root_id);

        assert_eq!(tree.search(&key("a")).unwrap(), Some(record(1, 0)));

        assert_eq!(tree.search(&key("c")).unwrap(), Some(record(2, 0)));

        assert_eq!(tree.search(&key("e")).unwrap(), Some(record(3, 0)));

        assert_eq!(tree.search(&key("f")).unwrap(), Some(record(3, 1)));

        assert_eq!(tree.search(&key("x")).unwrap(), None);
    }

    #[test]
    fn find_leaf_returns_root_when_root_is_leaf() {
        let mut store = MemoryNodeStore::new();

        let leaf = BPlusNode::Leaf {
            keys: vec![key("a"), key("b")],
            values: vec![record(1, 0), record(1, 1)],
            next_leaf: None,
        };

        let root_id = store.allocate_node(&leaf).unwrap();

        let tree = BPlusTree::from_root(store, root_id);

        let (page_id, node) = tree.find_leaf(&key("b")).unwrap();

        assert_eq!(page_id, root_id);
        assert_eq!(node, leaf);
    }
    #[test]
    fn find_leaf_routes_to_left_child() {
        let mut store = MemoryNodeStore::new();

        let left = BPlusNode::Leaf {
            keys: vec![key("a"), key("b")],
            values: vec![record(1, 0), record(1, 1)],
            next_leaf: None,
        };

        let left_id = store.allocate_node(&left).unwrap();

        let right = BPlusNode::Leaf {
            keys: vec![key("d"), key("e")],
            values: vec![record(2, 0), record(2, 1)],
            next_leaf: None,
        };

        let right_id = store.allocate_node(&right).unwrap();

        let root = BPlusNode::Internal {
            keys: vec![key("d")],
            children: vec![left_id, right_id],
        };

        let root_id = store.allocate_node(&root).unwrap();

        let tree = BPlusTree::from_root(store, root_id);

        let (page_id, _) = tree.find_leaf(&key("b")).unwrap();

        assert_eq!(page_id, left_id);
    }
    #[test]
    fn find_leaf_routes_to_middle_child() {
        let mut store = MemoryNodeStore::new();

        let left = BPlusNode::Leaf {
            keys: vec![key("a")],
            values: vec![record(1, 0)],
            next_leaf: None,
        };

        let left_id = store.allocate_node(&left).unwrap();

        let middle = BPlusNode::Leaf {
            keys: vec![key("c"), key("d")],
            values: vec![record(2, 0), record(2, 1)],
            next_leaf: None,
        };

        let middle_id = store.allocate_node(&middle).unwrap();

        let right = BPlusNode::Leaf {
            keys: vec![key("f")],
            values: vec![record(3, 0)],
            next_leaf: None,
        };

        let right_id = store.allocate_node(&right).unwrap();

        let root = BPlusNode::Internal {
            keys: vec![key("c"), key("f")],
            children: vec![left_id, middle_id, right_id],
        };

        let root_id = store.allocate_node(&root).unwrap();

        let tree = BPlusTree::from_root(store, root_id);

        let (page_id, _) = tree.find_leaf(&key("d")).unwrap();

        assert_eq!(page_id, middle_id);
    }
    #[test]
    fn find_leaf_routes_to_right_child() {
        let mut store = MemoryNodeStore::new();

        let left = BPlusNode::Leaf {
            keys: vec![key("a")],
            values: vec![record(1, 0)],
            next_leaf: None,
        };

        let left_id = store.allocate_node(&left).unwrap();

        let right = BPlusNode::Leaf {
            keys: vec![key("z")],
            values: vec![record(2, 0)],
            next_leaf: None,
        };

        let right_id = store.allocate_node(&right).unwrap();

        let root = BPlusNode::Internal {
            keys: vec![key("m")],
            children: vec![left_id, right_id],
        };

        let root_id = store.allocate_node(&root).unwrap();

        let tree = BPlusTree::from_root(store, root_id);

        let (page_id, _) = tree.find_leaf(&key("z")).unwrap();

        assert_eq!(page_id, right_id);
    }
    #[test]
    fn find_leaf_separator_equality_routes_right() {
        let mut store = MemoryNodeStore::new();

        let left = BPlusNode::Leaf {
            keys: vec![key("a"), key("l")],
            values: vec![record(1, 0), record(1, 1)],
            next_leaf: None,
        };

        let left_id = store.allocate_node(&left).unwrap();

        let right = BPlusNode::Leaf {
            keys: vec![key("m"), key("z")],
            values: vec![record(2, 0), record(2, 1)],
            next_leaf: None,
        };

        let right_id = store.allocate_node(&right).unwrap();

        let root = BPlusNode::Internal {
            keys: vec![key("m")],
            children: vec![left_id, right_id],
        };

        let root_id = store.allocate_node(&root).unwrap();

        let tree = BPlusTree::from_root(store, root_id);

        let (page_id, _) = tree.find_leaf(&key("m")).unwrap();

        assert_eq!(page_id, right_id);
    }
    #[test]
    fn find_leaf_handles_multiple_internal_levels() {
        let mut store = MemoryNodeStore::new();

        let leaf1 = BPlusNode::Leaf {
            keys: vec![key("a"), key("b")],
            values: vec![record(1, 0), record(1, 1)],
            next_leaf: None,
        };

        let leaf1_id = store.allocate_node(&leaf1).unwrap();

        let leaf2 = BPlusNode::Leaf {
            keys: vec![key("c"), key("d")],
            values: vec![record(2, 0), record(2, 1)],
            next_leaf: None,
        };

        let leaf2_id = store.allocate_node(&leaf2).unwrap();

        let leaf3 = BPlusNode::Leaf {
            keys: vec![key("e"), key("f")],
            values: vec![record(3, 0), record(3, 1)],
            next_leaf: None,
        };

        let leaf3_id = store.allocate_node(&leaf3).unwrap();

        let left_internal = BPlusNode::Internal {
            keys: vec![key("c")],
            children: vec![leaf1_id, leaf2_id],
        };

        let left_internal_id = store.allocate_node(&left_internal).unwrap();

        let right_internal = BPlusNode::Internal {
            keys: vec![],
            children: vec![leaf3_id],
        };

        let right_internal_id = store.allocate_node(&right_internal).unwrap();

        let root = BPlusNode::Internal {
            keys: vec![key("e")],
            children: vec![left_internal_id, right_internal_id],
        };

        let root_id = store.allocate_node(&root).unwrap();

        let tree = BPlusTree::from_root(store, root_id);

        let (page_id, _) = tree.find_leaf(&key("f")).unwrap();

        assert_eq!(page_id, leaf3_id);
    }

    #[test]
    fn insert_into_empty_leaf() {
        let store = MemoryNodeStore::new();
        let mut tree = BPlusTree::new(store).unwrap();

        let key = key("a");
        let record_id = record(1, 0);

        tree.insert(key.clone(), record_id).unwrap();

        assert_eq!(tree.search(&key).unwrap(), Some(record_id));

        let result = tree.scan().unwrap();

        assert_eq!(result, vec![(key, record_id)]);
    }

    #[test]
    fn insert_at_beginning() {
        let mut store = MemoryNodeStore::new();

        let leaf = BPlusNode::Leaf {
            keys: vec![key("b"), key("c")],
            values: vec![record(1, 1), record(1, 2)],
            next_leaf: None,
        };

        let root_id = store.allocate_node(&leaf).unwrap();

        let mut tree = BPlusTree::from_root(store, root_id);

        tree.insert(key("a"), record(1, 0)).unwrap();

        assert_eq!(
            tree.scan().unwrap(),
            vec![
                (key("a"), record(1, 0)),
                (key("b"), record(1, 1)),
                (key("c"), record(1, 2)),
            ]
        );
    }

    #[test]
    fn insert_in_middle() {
        let mut store = MemoryNodeStore::new();

        let leaf = BPlusNode::Leaf {
            keys: vec![key("a"), key("c")],
            values: vec![record(1, 0), record(1, 2)],
            next_leaf: None,
        };

        let root_id = store.allocate_node(&leaf).unwrap();

        let mut tree = BPlusTree::from_root(store, root_id);

        tree.insert(key("b"), record(1, 1)).unwrap();

        assert_eq!(
            tree.scan().unwrap(),
            vec![
                (key("a"), record(1, 0)),
                (key("b"), record(1, 1)),
                (key("c"), record(1, 2)),
            ]
        );
    }

    #[test]
    fn insert_at_end() {
        let mut store = MemoryNodeStore::new();

        let leaf = BPlusNode::Leaf {
            keys: vec![key("a"), key("b")],
            values: vec![record(1, 0), record(1, 1)],
            next_leaf: None,
        };

        let root_id = store.allocate_node(&leaf).unwrap();

        let mut tree = BPlusTree::from_root(store, root_id);

        tree.insert(key("c"), record(1, 2)).unwrap();

        assert_eq!(
            tree.scan().unwrap(),
            vec![
                (key("a"), record(1, 0)),
                (key("b"), record(1, 1)),
                (key("c"), record(1, 2)),
            ]
        );
    }

    #[test]
    fn insert_preserves_sorted_order() {
        let store = MemoryNodeStore::new();
        let mut tree = BPlusTree::new(store).unwrap();

        tree.insert(key("d"), record(1, 3)).unwrap();
        tree.insert(key("a"), record(1, 0)).unwrap();
        tree.insert(key("c"), record(1, 2)).unwrap();
        tree.insert(key("b"), record(1, 1)).unwrap();

        assert_eq!(
            tree.scan().unwrap(),
            vec![
                (key("a"), record(1, 0)),
                (key("b"), record(1, 1)),
                (key("c"), record(1, 2)),
                (key("d"), record(1, 3)),
            ]
        );
    }

    #[test]
    fn insert_preserves_key_value_alignment() {
        let store = MemoryNodeStore::new();
        let mut tree = BPlusTree::new(store).unwrap();

        tree.insert(key("a"), record(10, 0)).unwrap();
        tree.insert(key("c"), record(10, 2)).unwrap();
        tree.insert(key("b"), record(10, 1)).unwrap();

        assert_eq!(tree.search(&key("a")).unwrap(), Some(record(10, 0)));

        assert_eq!(tree.search(&key("b")).unwrap(), Some(record(10, 1)));

        assert_eq!(tree.search(&key("c")).unwrap(), Some(record(10, 2)));
    }

    #[test]
    fn insert_duplicate_key_returns_error() {
        let store = MemoryNodeStore::new();
        let mut tree = BPlusTree::new(store).unwrap();

        tree.insert(key("a"), record(1, 0)).unwrap();

        let result = tree.insert(key("a"), record(2, 0));

        assert!(matches!(result, Err(BPlusTreeError::DuplicateKey)));
    }

    #[test]
    fn duplicate_insert_does_not_modify_existing_value() {
        let store = MemoryNodeStore::new();
        let mut tree = BPlusTree::new(store).unwrap();

        tree.insert(key("a"), record(1, 0)).unwrap();

        let result = tree.insert(key("a"), record(2, 0));

        assert!(matches!(result, Err(BPlusTreeError::DuplicateKey)));

        assert_eq!(tree.search(&key("a")).unwrap(), Some(record(1, 0)));
    }

    #[test]
    fn duplicate_insert_does_not_modify_tree() {
        let store = MemoryNodeStore::new();
        let mut tree = BPlusTree::new(store).unwrap();

        tree.insert(key("a"), record(1, 0)).unwrap();
        tree.insert(key("c"), record(1, 2)).unwrap();

        let before = tree.scan().unwrap();

        let result = tree.insert(key("a"), record(99, 99));

        assert!(matches!(result, Err(BPlusTreeError::DuplicateKey)));

        let after = tree.scan().unwrap();

        assert_eq!(after, before);
    }

    #[test]
    fn insert_fills_leaf_until_capacity() {
        let store = MemoryNodeStore::new();
        let mut tree = BPlusTree::new(store).unwrap();

        let mut inserted = Vec::new();

        for i in 0..100 {
            let key = key(&format!("{i:03}"));
            let record_id = record(1, i as u16);

            match tree.insert(key.clone(), record_id) {
                Ok(()) => inserted.push((key, record_id)),
                Err(BPlusTreeError::LeafFull) => break,
                Err(error) => panic!("unexpected insertion error: {error}"),
            }
        }

        assert!(!inserted.is_empty(), "at least one record should fit");

        assert_eq!(tree.scan().unwrap(), inserted);
    }

    #[test]
    fn leaf_full_does_not_modify_tree() {
        let store = MemoryNodeStore::new();
        let mut tree = BPlusTree::new(store).unwrap();

        let mut inserted = Vec::new();

        for i in 0..1000 {
            let key = key(&format!("{i:04}"));
            let record_id = record(1, i as u16);

            match tree.insert(key.clone(), record_id) {
                Ok(()) => inserted.push((key, record_id)),
                Err(BPlusTreeError::LeafFull) => {
                    let after = tree.scan().unwrap();

                    assert_eq!(after, inserted);
                    return;
                }
                Err(error) => panic!("unexpected insertion error: {error}"),
            }
        }

        panic!("expected leaf to become full");
    }

    #[test]
    fn insert_after_leaf_full_returns_leaf_full() {
        let store = MemoryNodeStore::new();
        let mut tree = BPlusTree::new(store).unwrap();

        for i in 0..1000 {
            let current_key = key(&format!("{i:04}"));
            let record_id = record(1, i as u16);

            match tree.insert(current_key, record_id) {
                Ok(()) => {}

                Err(BPlusTreeError::LeafFull) => {
                    let next_key = key(&format!("{:04}", i + 1));

                    let result = tree.insert(next_key, record(2, 0));

                    assert!(matches!(result, Err(BPlusTreeError::LeafFull)));

                    return;
                }

                Err(error) => {
                    panic!("unexpected insertion error: {error}");
                }
            }
        }

        panic!("expected leaf to become full");
    }

    #[test]
    fn insert_then_search_finds_key() {
        let store = MemoryNodeStore::new();
        let mut tree = BPlusTree::new(store).unwrap();

        tree.insert(key("hello"), record(42, 7)).unwrap();

        assert_eq!(tree.search(&key("hello")).unwrap(), Some(record(42, 7)));
    }

    #[test]
    fn insert_then_scan_returns_inserted_entries() {
        let store = MemoryNodeStore::new();
        let mut tree = BPlusTree::new(store).unwrap();

        tree.insert(key("c"), record(1, 2)).unwrap();
        tree.insert(key("a"), record(1, 0)).unwrap();
        tree.insert(key("b"), record(1, 1)).unwrap();

        assert_eq!(
            tree.scan().unwrap(),
            vec![
                (key("a"), record(1, 0)),
                (key("b"), record(1, 1)),
                (key("c"), record(1, 2)),
            ]
        );
    }

    #[test]
    fn insert_preserves_existing_next_leaf() {
        let mut store = MemoryNodeStore::new();

        let next_leaf = BPlusNode::Leaf {
            keys: vec![key("z")],
            values: vec![record(2, 0)],
            next_leaf: None,
        };

        let next_leaf_id = store.allocate_node(&next_leaf).unwrap();

        let leaf = BPlusNode::Leaf {
            keys: vec![key("a"), key("c")],
            values: vec![record(1, 0), record(1, 2)],
            next_leaf: Some(next_leaf_id),
        };

        let root_id = store.allocate_node(&leaf).unwrap();

        let mut tree = BPlusTree::from_root(store, root_id);

        tree.insert(key("b"), record(1, 1)).unwrap();

        assert_eq!(
            tree.scan().unwrap(),
            vec![
                (key("a"), record(1, 0)),
                (key("b"), record(1, 1)),
                (key("c"), record(1, 2)),
                (key("z"), record(2, 0)),
            ]
        );
    }
}
