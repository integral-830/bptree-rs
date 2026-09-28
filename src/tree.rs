use crate::BPlusNode;
use crate::error::BPlusTreeError;
use crate::key::Key;
use crate::node_store::NodeStore;
use crate::page_id::PageId;
use crate::record_id::RecordId;
use crate::slotted_page::PAGE_SIZE;

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
    fn find_leaf_path(
        &self,
        key: &Key,
    ) -> Result<(Vec<PageId>, PageId, BPlusNode), BPlusTreeError<S::Error>> {
        let mut page_id = self.root_page_id;
        let mut ancestors = Vec::new();

        loop {
            let node = self.store.read_node(page_id)?;

            match node {
                BPlusNode::Internal { keys, children } => {
                    debug_assert_eq!(children.len(), keys.len() + 1);

                    ancestors.push(page_id);

                    let child_index = keys.partition_point(|separator| separator <= key);

                    page_id = children[child_index];
                }

                leaf @ BPlusNode::Leaf { .. } => {
                    return Ok((ancestors, page_id, leaf));
                }
            }
        }
    }
    fn split_leaf(
        &mut self,
        page_id: PageId,
        keys: Vec<Key>,
        values: Vec<RecordId>,
        next_leaf: Option<PageId>,
    ) -> Result<(PageId, Key), BPlusTreeError<S::Error>> {
        debug_assert!(!keys.is_empty());
        debug_assert_eq!(keys.len(), values.len());

        let split_index = keys.len() / 2;

        debug_assert!(split_index > 0);
        debug_assert!(split_index < keys.len());

        let right_keys = keys[split_index..].to_vec();
        let right_values = values[split_index..].to_vec();

        let left_keys = keys[..split_index].to_vec();
        let left_values = values[..split_index].to_vec();

        let separator = right_keys
            .first()
            .cloned()
            .expect("right leaf must contain at least one key");

        let right_leaf = BPlusNode::Leaf {
            keys: right_keys,
            values: right_values,
            next_leaf,
        };

        let right_page_id = self.store.allocate_node(&right_leaf)?;

        let left_leaf = BPlusNode::Leaf {
            keys: left_keys,
            values: left_values,
            next_leaf: Some(right_page_id),
        };

        self.store.write_node(page_id, &left_leaf)?;

        Ok((right_page_id, separator))
    }
    fn split_internal(
        &mut self,
        page_id: PageId,
        keys: Vec<Key>,
        children: Vec<PageId>,
    ) -> Result<(PageId, Key), BPlusTreeError<S::Error>> {
        debug_assert!(!keys.is_empty());
        debug_assert_eq!(children.len(), keys.len() + 1);

        let split_index = keys.len() / 2;

        debug_assert!(split_index > 0);
        debug_assert!(split_index < keys.len());

        let promoted_key = keys[split_index].clone();

        let left_keys = keys[..split_index].to_vec();
        let left_children = children[..=split_index].to_vec();

        let right_keys = keys[split_index + 1..].to_vec();
        let right_children = children[split_index + 1..].to_vec();

        let left_node = BPlusNode::Internal {
            keys: left_keys,
            children: left_children,
        };

        let right_node = BPlusNode::Internal {
            keys: right_keys,
            children: right_children,
        };

        left_node.validate().map_err(BPlusTreeError::Node)?;

        right_node.validate().map_err(BPlusTreeError::Node)?;

        let left_encoded_len = left_node.encoded_len().map_err(BPlusTreeError::Node)?;

        let right_encoded_len = right_node.encoded_len().map_err(BPlusTreeError::Node)?;

        if left_encoded_len > PAGE_SIZE || right_encoded_len > PAGE_SIZE {
            return Err(BPlusTreeError::InvalidTreeState(
                "internal split produced a node larger than PAGE_SIZE",
            ));
        }

        let right_page_id = self.store.allocate_node(&right_node)?;

        self.store.write_node(page_id, &left_node)?;

        Ok((right_page_id, promoted_key))
    }
    pub fn insert(
        &mut self,
        key: Key,
        record_id: RecordId,
    ) -> Result<(), BPlusTreeError<S::Error>> {
        let (ancestors, page_id, node) = self.find_leaf_path(&key)?;

        let BPlusNode::Leaf {
            mut keys,
            mut values,
            next_leaf,
        } = node
        else {
            return Err(BPlusTreeError::InvalidTreeState(
                "find_leaf_path returned an internal node",
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
            keys: keys.clone(),
            values: values.clone(),
            next_leaf,
        };

        let encoded_len = updated_leaf.encoded_len().map_err(BPlusTreeError::Node)?;

        // Leaf still fits. No split required.
        if encoded_len <= PAGE_SIZE {
            self.store.write_node(page_id, &updated_leaf)?;
            return Ok(());
        }

        // Split the overflowing leaf.
        let (mut right_page_id, mut separator) =
            self.split_leaf(page_id, keys, values, next_leaf)?;

        // The page that was split becomes the left side of
        // the split being propagated upward.
        let mut left_page_id = page_id;

        // Process ancestors from the leaf's parent toward the root.
        for &parent_page_id in ancestors.iter().rev() {
            let node = self.store.read_node(parent_page_id)?;

            let BPlusNode::Internal {
                mut keys,
                mut children,
            } = node
            else {
                return Err(BPlusTreeError::InvalidTreeState(
                    "ancestor page is not an internal node",
                ));
            };

            debug_assert_eq!(children.len(), keys.len() + 1);

            let Some(child_index) = children
                .iter()
                .position(|&child_page_id| child_page_id == left_page_id)
            else {
                return Err(BPlusTreeError::InvalidTreeState(
                    "parent does not contain the split child",
                ));
            };

            // Insert the separator produced by the child split.
            keys.insert(child_index, separator.clone());
            children.insert(child_index + 1, right_page_id);

            debug_assert_eq!(children.len(), keys.len() + 1);

            let updated_parent = BPlusNode::Internal {
                keys: keys.clone(),
                children: children.clone(),
            };

            updated_parent.validate().map_err(BPlusTreeError::Node)?;

            let parent_encoded_len = updated_parent.encoded_len().map_err(BPlusTreeError::Node)?;

            // Parent still fits. We are finished propagating.
            if parent_encoded_len <= PAGE_SIZE {
                self.store.write_node(parent_page_id, &updated_parent)?;
                return Ok(());
            }

            // Parent itself overflowed.
            //
            // Split it and promote a new separator to its parent.
            let (new_right_page_id, promoted_separator) =
                self.split_internal(parent_page_id, keys, children)?;

            left_page_id = parent_page_id;
            right_page_id = new_right_page_id;
            separator = promoted_separator;
        }

        // We reached the root and its split propagated beyond the old root.
        //
        // The current left_page_id is the old root and right_page_id
        // is the newly allocated right half.
        let new_root = BPlusNode::Internal {
            keys: vec![separator],
            children: vec![left_page_id, right_page_id],
        };

        new_root.validate().map_err(BPlusTreeError::Node)?;

        let new_root_page_id = self.store.allocate_node(&new_root)?;

        self.root_page_id = new_root_page_id;

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
    use crate::BPlusNode;
    use crate::error::BPlusTreeError;
    use crate::key::Key;
    use crate::node_store::NodeStore;
    use crate::page_id::PageId;
    use crate::record_id::RecordId;
    use crate::slotted_page::PAGE_SIZE;
    use crate::storage::memory::MemoryNodeStore;
    use crate::tree::BPlusTree;

    fn key(value: &str) -> Key {
        Key::new(value.as_bytes().to_vec())
    }

    fn record(page_id: u64, slot_id: u16) -> RecordId {
        RecordId::new(PageId::new(page_id), slot_id)
    }
    fn fixed_key(i: usize) -> Key {
        key(&format!("{i:08}-{}", "x".repeat(8)))
    }

    fn record_id(i: usize) -> RecordId {
        RecordId::new(PageId::new(i as u64 + 1_000_000), 0)
    }

    fn assert_node_valid<S: NodeStore>(tree: &BPlusTree<S>, page_id: PageId)
    where
        S::Error: std::fmt::Debug,
    {
        let node = tree.store.read_node(page_id).unwrap();

        assert!(
            node.validate().is_ok(),
            "invalid node at page {:?}",
            page_id
        );

        assert!(
            node.encoded_len().unwrap() <= PAGE_SIZE,
            "node exceeds PAGE_SIZE at page {:?}",
            page_id
        );

        match node {
            BPlusNode::Leaf { keys, values, .. } => {
                assert_eq!(keys.len(), values.len());

                assert!(
                    keys.windows(2).all(|pair| pair[0] < pair[1]),
                    "leaf keys are not strictly sorted"
                );
            }

            BPlusNode::Internal { keys, children } => {
                assert_eq!(children.len(), keys.len() + 1);

                assert!(
                    keys.windows(2).all(|pair| pair[0] < pair[1]),
                    "internal keys are not strictly sorted"
                );

                for child_page_id in children {
                    assert_node_valid(tree, child_page_id);
                }
            }
        }
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
    #[test]
    fn find_leaf_path_root_leaf() {
        let store = MemoryNodeStore::new();
        let tree = BPlusTree::new(store).unwrap();

        let search_key = key("0001");

        let (ancestors, leaf_page_id, node) = tree.find_leaf_path(&search_key).unwrap();

        assert!(ancestors.is_empty());
        assert_eq!(leaf_page_id, tree.root_page_id);
        assert!(matches!(node, BPlusNode::Leaf { .. }));
    }

    #[test]
    fn find_leaf_path_one_internal_level() {
        let mut store = MemoryNodeStore::new();

        let left_leaf = BPlusNode::Leaf {
            keys: vec![key("0001"), key("0002")],
            values: vec![record(1, 1), record(1, 2)],
            next_leaf: None,
        };

        let right_leaf = BPlusNode::Leaf {
            keys: vec![key("0010"), key("0020")],
            values: vec![record(2, 1), record(2, 2)],
            next_leaf: None,
        };

        let left_page_id = store.allocate_node(&left_leaf).unwrap();
        let right_page_id = store.allocate_node(&right_leaf).unwrap();

        let root = BPlusNode::Internal {
            keys: vec![key("0010")],
            children: vec![left_page_id, right_page_id],
        };

        let root_page_id = store.allocate_node(&root).unwrap();

        let tree = BPlusTree::from_root(store, root_page_id);

        let search_key = key("0002");

        let (ancestors, leaf_page_id, node) = tree.find_leaf_path(&search_key).unwrap();

        assert_eq!(ancestors, vec![root_page_id]);
        assert_eq!(leaf_page_id, left_page_id);
        assert!(matches!(node, BPlusNode::Leaf { .. }));
    }

    #[test]
    fn find_leaf_path_routes_to_right_child() {
        let mut store = MemoryNodeStore::new();

        let left_leaf = BPlusNode::Leaf {
            keys: vec![key("0001")],
            values: vec![record(1, 1)],
            next_leaf: None,
        };

        let right_leaf = BPlusNode::Leaf {
            keys: vec![key("0010")],
            values: vec![record(2, 1)],
            next_leaf: None,
        };

        let left_page_id = store.allocate_node(&left_leaf).unwrap();
        let right_page_id = store.allocate_node(&right_leaf).unwrap();

        let root = BPlusNode::Internal {
            keys: vec![key("0010")],
            children: vec![left_page_id, right_page_id],
        };

        let root_page_id = store.allocate_node(&root).unwrap();

        let tree = BPlusTree::from_root(store, root_page_id);

        let search_key = key("0020");

        let (ancestors, leaf_page_id, node) = tree.find_leaf_path(&search_key).unwrap();

        assert_eq!(ancestors, vec![root_page_id]);
        assert_eq!(leaf_page_id, right_page_id);
        assert!(matches!(node, BPlusNode::Leaf { .. }));
    }

    #[test]
    fn find_leaf_path_separator_equality_routes_right() {
        let mut store = MemoryNodeStore::new();

        let left_leaf = BPlusNode::Leaf {
            keys: vec![key("0001")],
            values: vec![record(1, 1)],
            next_leaf: None,
        };

        let right_leaf = BPlusNode::Leaf {
            keys: vec![key("0010"), key("0020")],
            values: vec![record(2, 1), record(2, 2)],
            next_leaf: None,
        };

        let left_page_id = store.allocate_node(&left_leaf).unwrap();
        let right_page_id = store.allocate_node(&right_leaf).unwrap();

        let root = BPlusNode::Internal {
            keys: vec![key("0010")],
            children: vec![left_page_id, right_page_id],
        };

        let root_page_id = store.allocate_node(&root).unwrap();

        let tree = BPlusTree::from_root(store, root_page_id);

        let search_key = key("0010");

        let (ancestors, leaf_page_id, node) = tree.find_leaf_path(&search_key).unwrap();

        assert_eq!(ancestors, vec![root_page_id]);
        assert_eq!(leaf_page_id, right_page_id);
        assert!(matches!(node, BPlusNode::Leaf { .. }));
    }

    #[test]
    fn find_leaf_path_multiple_internal_levels() {
        let mut store = MemoryNodeStore::new();

        let l1 = BPlusNode::Leaf {
            keys: vec![key("0001")],
            values: vec![record(1, 1)],
            next_leaf: None,
        };

        let l2 = BPlusNode::Leaf {
            keys: vec![key("0010")],
            values: vec![record(2, 1)],
            next_leaf: None,
        };

        let l3 = BPlusNode::Leaf {
            keys: vec![key("0020")],
            values: vec![record(3, 1)],
            next_leaf: None,
        };

        let l4 = BPlusNode::Leaf {
            keys: vec![key("0030")],
            values: vec![record(4, 1)],
            next_leaf: None,
        };

        let l1_page_id = store.allocate_node(&l1).unwrap();
        let l2_page_id = store.allocate_node(&l2).unwrap();
        let l3_page_id = store.allocate_node(&l3).unwrap();
        let l4_page_id = store.allocate_node(&l4).unwrap();

        let n1 = BPlusNode::Internal {
            keys: vec![key("0010")],
            children: vec![l1_page_id, l2_page_id],
        };

        let n1_page_id = store.allocate_node(&n1).unwrap();

        let n2 = BPlusNode::Internal {
            keys: vec![key("0030")],
            children: vec![l3_page_id, l4_page_id],
        };

        let n2_page_id = store.allocate_node(&n2).unwrap();

        let root = BPlusNode::Internal {
            keys: vec![key("0020")],
            children: vec![n1_page_id, n2_page_id],
        };

        let root_page_id = store.allocate_node(&root).unwrap();

        let tree = BPlusTree::from_root(store, root_page_id);

        let search_key = key("0015");

        let (ancestors, leaf_page_id, node) = tree.find_leaf_path(&search_key).unwrap();

        assert_eq!(ancestors, vec![root_page_id, n1_page_id]);
        assert_eq!(leaf_page_id, l2_page_id);
        assert!(matches!(node, BPlusNode::Leaf { .. }));
    }
    #[test]
    fn split_leaf_preserves_all_entries() {
        let mut store = MemoryNodeStore::new();

        let leaf = BPlusNode::Leaf {
            keys: vec![
                key("0001"),
                key("0002"),
                key("0003"),
                key("0004"),
                key("0005"),
                key("0006"),
            ],
            values: vec![
                record(1, 1),
                record(1, 2),
                record(1, 3),
                record(1, 4),
                record(1, 5),
                record(1, 6),
            ],
            next_leaf: None,
        };

        let leaf_page_id = store.allocate_node(&leaf).unwrap();

        let mut tree = BPlusTree::from_root(store, leaf_page_id);

        let (right_page_id, separator) = tree
            .split_leaf(
                leaf_page_id,
                vec![
                    key("0001"),
                    key("0002"),
                    key("0003"),
                    key("0004"),
                    key("0005"),
                    key("0006"),
                ],
                vec![
                    record(1, 1),
                    record(1, 2),
                    record(1, 3),
                    record(1, 4),
                    record(1, 5),
                    record(1, 6),
                ],
                None,
            )
            .unwrap();

        assert_eq!(separator, key("0004"));

        let (_, left_node) = tree.find_leaf(&key("0001")).unwrap();

        let right_node = tree.store.read_node(right_page_id).unwrap();

        match left_node {
            BPlusNode::Leaf {
                keys,
                values,
                next_leaf,
            } => {
                assert_eq!(keys, vec![key("0001"), key("0002"), key("0003")]);

                assert_eq!(values, vec![record(1, 1), record(1, 2), record(1, 3),]);

                assert_eq!(next_leaf, Some(right_page_id));
            }

            BPlusNode::Internal { .. } => {
                panic!("expected left leaf");
            }
        }

        match right_node {
            BPlusNode::Leaf {
                keys,
                values,
                next_leaf,
            } => {
                assert_eq!(keys, vec![key("0004"), key("0005"), key("0006")]);

                assert_eq!(values, vec![record(1, 4), record(1, 5), record(1, 6),]);

                assert_eq!(next_leaf, None);
            }

            BPlusNode::Internal { .. } => {
                panic!("expected right leaf");
            }
        }
    }
    #[test]
    fn split_leaf_preserves_key_value_alignment() {
        let mut store = MemoryNodeStore::new();

        let leaf = BPlusNode::Leaf {
            keys: vec![key("0001"), key("0002"), key("0003"), key("0004")],
            values: vec![record(10, 1), record(10, 2), record(10, 3), record(10, 4)],
            next_leaf: None,
        };

        let leaf_page_id = store.allocate_node(&leaf).unwrap();

        let mut tree = BPlusTree::from_root(store, leaf_page_id);

        let (right_page_id, separator) = tree
            .split_leaf(
                leaf_page_id,
                vec![key("0001"), key("0002"), key("0003"), key("0004")],
                vec![record(10, 1), record(10, 2), record(10, 3), record(10, 4)],
                None,
            )
            .unwrap();

        assert_eq!(separator, key("0003"));

        let (_, left_node) = tree.find_leaf(&key("0001")).unwrap();
        let right_node = tree.store.read_node(right_page_id).unwrap();

        match left_node {
            BPlusNode::Leaf { keys, values, .. } => {
                assert_eq!(keys, vec![key("0001"), key("0002")]);

                assert_eq!(values, vec![record(10, 1), record(10, 2)]);
            }

            BPlusNode::Internal { .. } => {
                panic!("expected left leaf");
            }
        }

        match right_node {
            BPlusNode::Leaf { keys, values, .. } => {
                assert_eq!(keys, vec![key("0003"), key("0004")]);

                assert_eq!(values, vec![record(10, 3), record(10, 4)]);
            }

            BPlusNode::Internal { .. } => {
                panic!("expected right leaf");
            }
        }
    }
    #[test]
    fn split_leaf_preserves_existing_next_leaf() {
        let mut store = MemoryNodeStore::new();

        let next_leaf = BPlusNode::Leaf {
            keys: vec![key("0010")],
            values: vec![record(2, 10)],
            next_leaf: None,
        };

        let next_page_id = store.allocate_node(&next_leaf).unwrap();

        let leaf = BPlusNode::Leaf {
            keys: vec![key("0001"), key("0002"), key("0003"), key("0004")],
            values: vec![record(1, 1), record(1, 2), record(1, 3), record(1, 4)],
            next_leaf: Some(next_page_id),
        };

        let leaf_page_id = store.allocate_node(&leaf).unwrap();

        let mut tree = BPlusTree::from_root(store, leaf_page_id);

        let (right_page_id, separator) = tree
            .split_leaf(
                leaf_page_id,
                vec![key("0001"), key("0002"), key("0003"), key("0004")],
                vec![record(1, 1), record(1, 2), record(1, 3), record(1, 4)],
                Some(next_page_id),
            )
            .unwrap();

        assert_eq!(separator, key("0003"));

        let left_node = tree.store.read_node(leaf_page_id).unwrap();
        let right_node = tree.store.read_node(right_page_id).unwrap();

        match left_node {
            BPlusNode::Leaf { next_leaf, .. } => {
                assert_eq!(next_leaf, Some(right_page_id));
            }

            BPlusNode::Internal { .. } => {
                panic!("expected left leaf");
            }
        }

        match right_node {
            BPlusNode::Leaf { next_leaf, .. } => {
                assert_eq!(next_leaf, Some(next_page_id));
            }

            BPlusNode::Internal { .. } => {
                panic!("expected right leaf");
            }
        }
    }
    #[test]
    fn split_leaf_rightmost_sets_right_next_to_none() {
        let mut store = MemoryNodeStore::new();

        let leaf = BPlusNode::Leaf {
            keys: vec![key("0001"), key("0002"), key("0003"), key("0004")],
            values: vec![record(1, 1), record(1, 2), record(1, 3), record(1, 4)],
            next_leaf: None,
        };

        let leaf_page_id = store.allocate_node(&leaf).unwrap();

        let mut tree = BPlusTree::from_root(store, leaf_page_id);

        let (right_page_id, separator) = tree
            .split_leaf(
                leaf_page_id,
                vec![key("0001"), key("0002"), key("0003"), key("0004")],
                vec![record(1, 1), record(1, 2), record(1, 3), record(1, 4)],
                None,
            )
            .unwrap();

        assert_eq!(separator, key("0003"));

        let left_node = tree.store.read_node(leaf_page_id).unwrap();
        let right_node = tree.store.read_node(right_page_id).unwrap();

        match left_node {
            BPlusNode::Leaf { next_leaf, .. } => {
                assert_eq!(next_leaf, Some(right_page_id));
            }

            BPlusNode::Internal { .. } => {
                panic!("expected left leaf");
            }
        }

        match right_node {
            BPlusNode::Leaf { next_leaf, .. } => {
                assert_eq!(next_leaf, None);
            }

            BPlusNode::Internal { .. } => {
                panic!("expected right leaf");
            }
        }
    }
    #[test]
    fn split_leaf_separator_is_first_right_key() {
        let mut store = MemoryNodeStore::new();

        let leaf = BPlusNode::Leaf {
            keys: vec![
                key("0010"),
                key("0020"),
                key("0030"),
                key("0040"),
                key("0050"),
                key("0060"),
            ],
            values: vec![
                record(1, 10),
                record(1, 20),
                record(1, 30),
                record(1, 40),
                record(1, 50),
                record(1, 60),
            ],
            next_leaf: None,
        };

        let leaf_page_id = store.allocate_node(&leaf).unwrap();

        let mut tree = BPlusTree::from_root(store, leaf_page_id);

        let (right_page_id, separator) = tree
            .split_leaf(
                leaf_page_id,
                vec![
                    key("0010"),
                    key("0020"),
                    key("0030"),
                    key("0040"),
                    key("0050"),
                    key("0060"),
                ],
                vec![
                    record(1, 10),
                    record(1, 20),
                    record(1, 30),
                    record(1, 40),
                    record(1, 50),
                    record(1, 60),
                ],
                None,
            )
            .unwrap();

        let right_node = tree.store.read_node(right_page_id).unwrap();

        match right_node {
            BPlusNode::Leaf { keys, .. } => {
                assert_eq!(separator, keys[0]);
                assert_eq!(separator, key("0040"));
            }

            BPlusNode::Internal { .. } => {
                panic!("expected right leaf");
            }
        }
    }
    #[test]
    fn split_leaf_keeps_original_page_as_left_leaf() {
        let mut store = MemoryNodeStore::new();

        let leaf = BPlusNode::Leaf {
            keys: vec![key("0001"), key("0002"), key("0003"), key("0004")],
            values: vec![record(1, 1), record(1, 2), record(1, 3), record(1, 4)],
            next_leaf: None,
        };

        let leaf_page_id = store.allocate_node(&leaf).unwrap();

        let mut tree = BPlusTree::from_root(store, leaf_page_id);

        let (right_page_id, _) = tree
            .split_leaf(
                leaf_page_id,
                vec![key("0001"), key("0002"), key("0003"), key("0004")],
                vec![record(1, 1), record(1, 2), record(1, 3), record(1, 4)],
                None,
            )
            .unwrap();

        assert_ne!(leaf_page_id, right_page_id);

        let left_node = tree.store.read_node(leaf_page_id).unwrap();

        match left_node {
            BPlusNode::Leaf { keys, .. } => {
                assert_eq!(keys, vec![key("0001"), key("0002")]);
            }

            BPlusNode::Internal { .. } => {
                panic!("original page must remain the left leaf");
            }
        }
    }
    #[test]
    fn split_leaf_preserves_sorted_order() {
        let mut store = MemoryNodeStore::new();

        let leaf = BPlusNode::Leaf {
            keys: vec![
                key("0010"),
                key("0020"),
                key("0030"),
                key("0040"),
                key("0050"),
                key("0060"),
            ],
            values: vec![
                record(1, 10),
                record(1, 20),
                record(1, 30),
                record(1, 40),
                record(1, 50),
                record(1, 60),
            ],
            next_leaf: None,
        };

        let leaf_page_id = store.allocate_node(&leaf).unwrap();

        let mut tree = BPlusTree::from_root(store, leaf_page_id);

        let (right_page_id, _) = tree
            .split_leaf(
                leaf_page_id,
                vec![
                    key("0010"),
                    key("0020"),
                    key("0030"),
                    key("0040"),
                    key("0050"),
                    key("0060"),
                ],
                vec![
                    record(1, 10),
                    record(1, 20),
                    record(1, 30),
                    record(1, 40),
                    record(1, 50),
                    record(1, 60),
                ],
                None,
            )
            .unwrap();

        let left_node = tree.store.read_node(leaf_page_id).unwrap();
        let right_node = tree.store.read_node(right_page_id).unwrap();

        let (left_keys, right_keys) = match (left_node, right_node) {
            (
                BPlusNode::Leaf {
                    keys: left_keys, ..
                },
                BPlusNode::Leaf {
                    keys: right_keys, ..
                },
            ) => (left_keys, right_keys),

            _ => panic!("expected two leaf nodes"),
        };

        assert!(left_keys.windows(2).all(|w| w[0] < w[1]));
        assert!(right_keys.windows(2).all(|w| w[0] < w[1]));

        assert!(left_keys.last().unwrap() < right_keys.first().unwrap());
    }

    #[test]
    fn insert_splits_root_leaf() {
        let store = MemoryNodeStore::new();
        let mut tree = BPlusTree::new(store).unwrap();

        let mut inserted = Vec::new();

        for i in 0..1000u16 {
            let current_key = key(&format!("{i:04}"));
            let current_record = record(1, i);

            match tree.insert(current_key.clone(), current_record) {
                Ok(()) => {
                    inserted.push((current_key, current_record));
                }
                Err(BPlusTreeError::LeafFull) => {
                    panic!("leaf split should prevent LeafFull");
                }
                Err(error) => {
                    panic!("unexpected insertion error: {error}");
                }
            }

            if i > 0 {
                let node = tree.store.read_node(tree.root_page_id).unwrap();

                if matches!(node, BPlusNode::Internal { .. }) {
                    break;
                }
            }
        }

        let root = tree.store.read_node(tree.root_page_id).unwrap();

        match root {
            BPlusNode::Internal { keys, children } => {
                assert_eq!(keys.len(), 1);
                assert_eq!(children.len(), 2);
            }

            BPlusNode::Leaf { .. } => {
                panic!("root should have become internal after leaf split");
            }
        }
    }
    #[test]
    fn insert_root_split_preserves_all_entries() {
        let store = MemoryNodeStore::new();
        let mut tree = BPlusTree::new(store).unwrap();

        let mut expected = Vec::new();

        for i in 0..1000u16 {
            let current_key = key(&format!("{i:04}"));
            let current_record = record(1, i);

            tree.insert(current_key.clone(), current_record).unwrap();

            expected.push((current_key, current_record));

            let root = tree.store.read_node(tree.root_page_id).unwrap();

            if matches!(root, BPlusNode::Internal { .. }) {
                break;
            }
        }

        let actual = tree.scan().unwrap();

        assert_eq!(actual, expected);
    }
    #[test]
    fn insert_root_split_updates_root_page_id() {
        let store = MemoryNodeStore::new();
        let mut tree = BPlusTree::new(store).unwrap();

        let original_root_page_id = tree.root_page_id;

        for i in 0..1000u16 {
            tree.insert(key(&format!("{i:04}")), record(1, i)).unwrap();

            let root = tree.store.read_node(tree.root_page_id).unwrap();

            if matches!(root, BPlusNode::Internal { .. }) {
                break;
            }
        }

        assert_ne!(tree.root_page_id, original_root_page_id);
    }
    #[test]
    fn insert_root_split_keeps_old_root_as_left_leaf() {
        let store = MemoryNodeStore::new();
        let mut tree = BPlusTree::new(store).unwrap();

        let old_root_page_id = tree.root_page_id;

        for i in 0..1000u16 {
            tree.insert(key(&format!("{i:04}")), record(1, i)).unwrap();

            let root = tree.store.read_node(tree.root_page_id).unwrap();

            if let BPlusNode::Internal { .. } = root {
                break;
            }
        }

        let root = tree.store.read_node(tree.root_page_id).unwrap();

        let left_page_id = match root {
            BPlusNode::Internal { children, .. } => children[0],
            BPlusNode::Leaf { .. } => {
                panic!("root should be internal");
            }
        };

        assert_eq!(left_page_id, old_root_page_id);

        let left = tree.store.read_node(left_page_id).unwrap();

        assert!(matches!(left, BPlusNode::Leaf { .. }));
    }
    #[test]
    fn insert_root_split_creates_two_leaf_children() {
        let store = MemoryNodeStore::new();
        let mut tree = BPlusTree::new(store).unwrap();

        for i in 0..1000u16 {
            tree.insert(key(&format!("{i:04}")), record(1, i)).unwrap();

            let root = tree.store.read_node(tree.root_page_id).unwrap();

            if matches!(root, BPlusNode::Internal { .. }) {
                break;
            }
        }

        let root = tree.store.read_node(tree.root_page_id).unwrap();

        let children = match root {
            BPlusNode::Internal { children, .. } => children,

            BPlusNode::Leaf { .. } => {
                panic!("root should be internal");
            }
        };

        assert_eq!(children.len(), 2);

        for child_page_id in children {
            let child = tree.store.read_node(child_page_id).unwrap();

            assert!(matches!(child, BPlusNode::Leaf { .. }));
        }
    }
    #[test]
    fn insert_root_split_separator_is_first_right_key() {
        let store = MemoryNodeStore::new();
        let mut tree = BPlusTree::new(store).unwrap();

        for i in 0..1000u16 {
            tree.insert(key(&format!("{i:04}")), record(1, i)).unwrap();

            let root = tree.store.read_node(tree.root_page_id).unwrap();

            if matches!(root, BPlusNode::Internal { .. }) {
                break;
            }
        }

        let root = tree.store.read_node(tree.root_page_id).unwrap();

        let (separator, right_page_id) = match root {
            BPlusNode::Internal { keys, children } => {
                assert_eq!(keys.len(), 1);
                assert_eq!(children.len(), 2);

                (keys[0].clone(), children[1])
            }

            BPlusNode::Leaf { .. } => {
                panic!("root should be internal");
            }
        };

        let right = tree.store.read_node(right_page_id).unwrap();

        match right {
            BPlusNode::Leaf { keys, .. } => {
                assert!(!keys.is_empty());
                assert_eq!(separator, keys[0]);
            }

            BPlusNode::Internal { .. } => {
                panic!("right child should be a leaf");
            }
        }
    }
    #[test]
    fn insert_root_split_preserves_leaf_chain() {
        let store = MemoryNodeStore::new();
        let mut tree = BPlusTree::new(store).unwrap();

        for i in 0..1000u16 {
            tree.insert(key(&format!("{i:04}")), record(1, i)).unwrap();

            let root = tree.store.read_node(tree.root_page_id).unwrap();

            if matches!(root, BPlusNode::Internal { .. }) {
                break;
            }
        }

        let root = tree.store.read_node(tree.root_page_id).unwrap();

        let children = match root {
            BPlusNode::Internal { children, .. } => children,

            BPlusNode::Leaf { .. } => {
                panic!("root should be internal");
            }
        };

        let left = tree.store.read_node(children[0]).unwrap();

        let right_page_id = match left {
            BPlusNode::Leaf { next_leaf, .. } => {
                next_leaf.expect("left leaf should point to right leaf")
            }

            BPlusNode::Internal { .. } => {
                panic!("left child should be leaf");
            }
        };

        assert_eq!(right_page_id, children[1]);

        let right = tree.store.read_node(right_page_id).unwrap();

        match right {
            BPlusNode::Leaf { next_leaf, .. } => {
                assert_eq!(next_leaf, None);
            }

            BPlusNode::Internal { .. } => {
                panic!("right child should be leaf");
            }
        }
    }
    #[test]
    fn insert_root_split_searches_both_leaves() {
        let store = MemoryNodeStore::new();
        let mut tree = BPlusTree::new(store).unwrap();

        let mut inserted = Vec::new();

        for i in 0..1000u16 {
            let current_key = key(&format!("{i:04}"));
            let current_record = record(1, i);

            tree.insert(current_key.clone(), current_record).unwrap();

            inserted.push((current_key, current_record));

            let root = tree.store.read_node(tree.root_page_id).unwrap();

            if matches!(root, BPlusNode::Internal { .. }) {
                break;
            }
        }

        assert!(inserted.len() >= 2);

        for (inserted_key, expected_record) in inserted {
            let result = tree.search(&inserted_key).unwrap();

            assert_eq!(result, Some(expected_record));
        }
    }
    #[test]
    fn insert_root_split_scan_is_sorted() {
        let store = MemoryNodeStore::new();
        let mut tree = BPlusTree::new(store).unwrap();

        for i in 0..1000u16 {
            tree.insert(key(&format!("{i:04}")), record(1, i)).unwrap();

            let root = tree.store.read_node(tree.root_page_id).unwrap();

            if matches!(root, BPlusNode::Internal { .. }) {
                break;
            }
        }

        let entries = tree.scan().unwrap();

        assert!(!entries.is_empty());

        assert!(entries.windows(2).all(|window| window[0].0 < window[1].0));
    }
    #[test]
    fn insert_root_split_separator_equality_searches_right_leaf() {
        let store = MemoryNodeStore::new();
        let mut tree = BPlusTree::new(store).unwrap();

        for i in 0..1000u16 {
            tree.insert(key(&format!("{i:04}")), record(1, i)).unwrap();

            let root = tree.store.read_node(tree.root_page_id).unwrap();

            if matches!(root, BPlusNode::Internal { .. }) {
                break;
            }
        }

        let root = tree.store.read_node(tree.root_page_id).unwrap();

        let separator = match root {
            BPlusNode::Internal { keys, .. } => keys[0].clone(),

            BPlusNode::Leaf { .. } => {
                panic!("root should be internal");
            }
        };

        let result = tree.search(&separator).unwrap();

        assert!(result.is_some());

        let (_, leaf) = tree.find_leaf(&separator).unwrap();

        match leaf {
            BPlusNode::Leaf { keys, .. } => {
                assert!(
                    keys.binary_search(&separator).is_ok(),
                    "separator equality must route to right leaf"
                );
            }

            BPlusNode::Internal { .. } => {
                panic!("find_leaf must return leaf");
            }
        }
    }
    #[test]
    fn split_internal_promotes_middle_key() {
        let store = MemoryNodeStore::new();
        let mut tree = BPlusTree::new(store).unwrap();

        let keys = vec![key("a"), key("b"), key("c"), key("d"), key("e")];

        let children = vec![
            PageId::new(1),
            PageId::new(2),
            PageId::new(3),
            PageId::new(4),
            PageId::new(5),
            PageId::new(6),
        ];

        let node = BPlusNode::Internal {
            keys: keys.clone(),
            children: children.clone(),
        };

        let page_id = tree.store.allocate_node(&node).unwrap();

        let (right_page_id, promoted_key) = tree.split_internal(page_id, keys, children).unwrap();

        assert_eq!(promoted_key, key("c"));
        assert_ne!(right_page_id, page_id);

        let left = tree.store.read_node(page_id).unwrap();
        let right = tree.store.read_node(right_page_id).unwrap();

        assert_eq!(
            left,
            BPlusNode::Internal {
                keys: vec![key("a"), key("b")],
                children: vec![PageId::new(1), PageId::new(2), PageId::new(3),],
            }
        );

        assert_eq!(
            right,
            BPlusNode::Internal {
                keys: vec![key("d"), key("e")],
                children: vec![PageId::new(4), PageId::new(5), PageId::new(6),],
            }
        );
    }

    #[test]
    fn split_internal_promoted_key_is_removed_from_children() {
        let store = MemoryNodeStore::new();
        let mut tree = BPlusTree::new(store).unwrap();

        let keys = vec![key("a"), key("b"), key("c"), key("d"), key("e")];

        let children = vec![
            PageId::new(1),
            PageId::new(2),
            PageId::new(3),
            PageId::new(4),
            PageId::new(5),
            PageId::new(6),
        ];

        let node = BPlusNode::Internal {
            keys: keys.clone(),
            children: children.clone(),
        };

        let page_id = tree.store.allocate_node(&node).unwrap();

        let (right_page_id, promoted_key) = tree.split_internal(page_id, keys, children).unwrap();

        let left = tree.store.read_node(page_id).unwrap();
        let right = tree.store.read_node(right_page_id).unwrap();

        let BPlusNode::Internal {
            keys: left_keys, ..
        } = left
        else {
            panic!("left node must be internal");
        };

        let BPlusNode::Internal {
            keys: right_keys, ..
        } = right
        else {
            panic!("right node must be internal");
        };

        assert!(!left_keys.contains(&promoted_key));
        assert!(!right_keys.contains(&promoted_key));
    }

    #[test]
    fn split_internal_preserves_all_keys() {
        let store = MemoryNodeStore::new();
        let mut tree = BPlusTree::new(store).unwrap();

        let keys = vec![key("a"), key("b"), key("c"), key("d"), key("e")];

        let children = vec![
            PageId::new(1),
            PageId::new(2),
            PageId::new(3),
            PageId::new(4),
            PageId::new(5),
            PageId::new(6),
        ];

        let node = BPlusNode::Internal {
            keys: keys.clone(),
            children: children.clone(),
        };

        let page_id = tree.store.allocate_node(&node).unwrap();

        let (right_page_id, promoted_key) = tree
            .split_internal(page_id, keys.clone(), children)
            .unwrap();

        let left = tree.store.read_node(page_id).unwrap();
        let right = tree.store.read_node(right_page_id).unwrap();

        let BPlusNode::Internal {
            keys: left_keys, ..
        } = left
        else {
            panic!("left node must be internal");
        };

        let BPlusNode::Internal {
            keys: right_keys, ..
        } = right
        else {
            panic!("right node must be internal");
        };

        let mut reconstructed_keys = left_keys;
        reconstructed_keys.push(promoted_key);
        reconstructed_keys.extend(right_keys);

        assert_eq!(reconstructed_keys, keys);
    }

    #[test]
    fn split_internal_preserves_all_children() {
        let store = MemoryNodeStore::new();
        let mut tree = BPlusTree::new(store).unwrap();

        let keys = vec![key("a"), key("b"), key("c"), key("d"), key("e")];

        let children = vec![
            PageId::new(1),
            PageId::new(2),
            PageId::new(3),
            PageId::new(4),
            PageId::new(5),
            PageId::new(6),
        ];

        let node = BPlusNode::Internal {
            keys: keys.clone(),
            children: children.clone(),
        };

        let page_id = tree.store.allocate_node(&node).unwrap();

        let (right_page_id, _) = tree
            .split_internal(page_id, keys, children.clone())
            .unwrap();

        let left = tree.store.read_node(page_id).unwrap();
        let right = tree.store.read_node(right_page_id).unwrap();

        let BPlusNode::Internal {
            children: left_children,
            ..
        } = left
        else {
            panic!("left node must be internal");
        };

        let BPlusNode::Internal {
            children: right_children,
            ..
        } = right
        else {
            panic!("right node must be internal");
        };

        let mut reconstructed_children = left_children;
        reconstructed_children.extend(right_children);

        assert_eq!(reconstructed_children, children);
    }

    #[test]
    fn split_internal_preserves_internal_cardinality() {
        let store = MemoryNodeStore::new();
        let mut tree = BPlusTree::new(store).unwrap();

        let keys = vec![key("a"), key("b"), key("c"), key("d"), key("e")];

        let children = vec![
            PageId::new(1),
            PageId::new(2),
            PageId::new(3),
            PageId::new(4),
            PageId::new(5),
            PageId::new(6),
        ];

        let node = BPlusNode::Internal {
            keys: keys.clone(),
            children: children.clone(),
        };

        let page_id = tree.store.allocate_node(&node).unwrap();

        let (right_page_id, _) = tree.split_internal(page_id, keys, children).unwrap();

        let left = tree.store.read_node(page_id).unwrap();
        let right = tree.store.read_node(right_page_id).unwrap();

        let BPlusNode::Internal {
            keys: left_keys,
            children: left_children,
        } = left
        else {
            panic!("left node must be internal");
        };

        let BPlusNode::Internal {
            keys: right_keys,
            children: right_children,
        } = right
        else {
            panic!("right node must be internal");
        };

        assert_eq!(left_children.len(), left_keys.len() + 1);
        assert_eq!(right_children.len(), right_keys.len() + 1);
    }

    #[test]
    fn split_internal_preserves_sorted_order() {
        let store = MemoryNodeStore::new();
        let mut tree = BPlusTree::new(store).unwrap();

        let keys = vec![
            key("a"),
            key("b"),
            key("c"),
            key("d"),
            key("e"),
            key("f"),
            key("g"),
        ];

        let children = (1..=8).map(PageId::new).collect::<Vec<_>>();

        let node = BPlusNode::Internal {
            keys: keys.clone(),
            children: children.clone(),
        };

        let page_id = tree.store.allocate_node(&node).unwrap();

        let (right_page_id, promoted_key) = tree.split_internal(page_id, keys, children).unwrap();

        let left = tree.store.read_node(page_id).unwrap();
        let right = tree.store.read_node(right_page_id).unwrap();

        let BPlusNode::Internal {
            keys: left_keys, ..
        } = left
        else {
            panic!("left node must be internal");
        };

        let BPlusNode::Internal {
            keys: right_keys, ..
        } = right
        else {
            panic!("right node must be internal");
        };

        assert!(left_keys.windows(2).all(|window| window[0] < window[1]));
        assert!(right_keys.windows(2).all(|window| window[0] < window[1]));

        assert!(left_keys.last().is_none_or(|key| key < &promoted_key));
        assert!(right_keys.first().is_none_or(|key| &promoted_key < key));
    }

    #[test]
    fn split_internal_original_page_becomes_left_node() {
        let store = MemoryNodeStore::new();
        let mut tree = BPlusTree::new(store).unwrap();

        let keys = vec![key("a"), key("b"), key("c"), key("d"), key("e")];

        let children = vec![
            PageId::new(1),
            PageId::new(2),
            PageId::new(3),
            PageId::new(4),
            PageId::new(5),
            PageId::new(6),
        ];

        let node = BPlusNode::Internal {
            keys: keys.clone(),
            children: children.clone(),
        };

        let page_id = tree.store.allocate_node(&node).unwrap();

        let (right_page_id, _) = tree.split_internal(page_id, keys, children).unwrap();

        assert_ne!(page_id, right_page_id);

        let left = tree.store.read_node(page_id).unwrap();

        assert_eq!(
            left,
            BPlusNode::Internal {
                keys: vec![key("a"), key("b")],
                children: vec![PageId::new(1), PageId::new(2), PageId::new(3),],
            }
        );
    }

    #[test]
    fn split_internal_allocates_right_node() {
        let store = MemoryNodeStore::new();
        let mut tree = BPlusTree::new(store).unwrap();

        let keys = vec![key("a"), key("b"), key("c"), key("d"), key("e")];

        let children = vec![
            PageId::new(1),
            PageId::new(2),
            PageId::new(3),
            PageId::new(4),
            PageId::new(5),
            PageId::new(6),
        ];

        let node = BPlusNode::Internal {
            keys: keys.clone(),
            children: children.clone(),
        };

        let page_id = tree.store.allocate_node(&node).unwrap();

        let (right_page_id, _) = tree.split_internal(page_id, keys, children).unwrap();

        assert_ne!(page_id, right_page_id);

        let right = tree.store.read_node(right_page_id).unwrap();

        assert_eq!(
            right,
            BPlusNode::Internal {
                keys: vec![key("d"), key("e")],
                children: vec![PageId::new(4), PageId::new(5), PageId::new(6),],
            }
        );
    }

    #[test]
    fn split_internal_resulting_nodes_validate() {
        let store = MemoryNodeStore::new();
        let mut tree = BPlusTree::new(store).unwrap();

        let keys = vec![key("a"), key("b"), key("c"), key("d"), key("e")];

        let children = vec![
            PageId::new(1),
            PageId::new(2),
            PageId::new(3),
            PageId::new(4),
            PageId::new(5),
            PageId::new(6),
        ];

        let node = BPlusNode::Internal {
            keys: keys.clone(),
            children: children.clone(),
        };

        let page_id = tree.store.allocate_node(&node).unwrap();

        let (right_page_id, _) = tree.split_internal(page_id, keys, children).unwrap();

        let left = tree.store.read_node(page_id).unwrap();
        let right = tree.store.read_node(right_page_id).unwrap();

        assert!(left.validate().is_ok());
        assert!(right.validate().is_ok());
    }

    #[test]
    fn split_internal_resulting_nodes_fit_page_size() {
        let store = MemoryNodeStore::new();
        let mut tree = BPlusTree::new(store).unwrap();

        let keys: Vec<Key> = (0..100).map(|i| key(&format!("{i:04}"))).collect();

        let children: Vec<PageId> = (0..=100).map(PageId::new).collect();

        let node = BPlusNode::Internal {
            keys: keys.clone(),
            children: children.clone(),
        };

        let page_id = tree.store.allocate_node(&node).unwrap();

        let (right_page_id, _) = tree.split_internal(page_id, keys, children).unwrap();

        let left = tree.store.read_node(page_id).unwrap();
        let right = tree.store.read_node(right_page_id).unwrap();

        assert!(left.encoded_len().unwrap() <= PAGE_SIZE);
        assert!(right.encoded_len().unwrap() <= PAGE_SIZE);
    }

    #[test]
    fn split_internal_with_even_key_count() {
        let store = MemoryNodeStore::new();
        let mut tree = BPlusTree::new(store).unwrap();

        let keys = vec![key("a"), key("b"), key("c"), key("d")];

        let children = vec![
            PageId::new(1),
            PageId::new(2),
            PageId::new(3),
            PageId::new(4),
            PageId::new(5),
        ];

        let node = BPlusNode::Internal {
            keys: keys.clone(),
            children: children.clone(),
        };

        let page_id = tree.store.allocate_node(&node).unwrap();

        let (right_page_id, promoted_key) = tree.split_internal(page_id, keys, children).unwrap();

        assert_eq!(promoted_key, key("c"));

        let left = tree.store.read_node(page_id).unwrap();
        let right = tree.store.read_node(right_page_id).unwrap();

        assert_eq!(
            left,
            BPlusNode::Internal {
                keys: vec![key("a"), key("b")],
                children: vec![PageId::new(1), PageId::new(2), PageId::new(3),],
            }
        );

        assert_eq!(
            right,
            BPlusNode::Internal {
                keys: vec![key("d")],
                children: vec![PageId::new(4), PageId::new(5),],
            }
        );
    }
    #[test]
    fn insert_after_leaf_full_splits_leaf() {
        let store = MemoryNodeStore::new();
        let mut tree = BPlusTree::new(store).unwrap();

        let mut inserted = Vec::new();

        for i in 0..10_000 {
            let current_key = key(&format!("{i:05}"));
            let record_id = RecordId::new(PageId::new(i as u64 + 1), 0);

            match tree.insert(current_key.clone(), record_id) {
                Ok(()) => {
                    inserted.push((current_key, record_id));
                }
                Err(BPlusTreeError::LeafFull) => {
                    panic!("insert returned LeafFull after leaf splitting was implemented");
                }
                Err(error) => panic!("unexpected insert error: {error:?}"),
            }

            if tree.scan().unwrap().len() > 100 {
                break;
            }
        }

        assert!(inserted.len() > 1);

        for (key, record_id) in &inserted {
            assert_eq!(tree.search(key).unwrap(), Some(*record_id));
        }

        let entries = tree.scan().unwrap();

        assert_eq!(entries, inserted);
    }

    #[test]
    fn leaf_split_preserves_existing_entries_and_new_entry() {
        let store = MemoryNodeStore::new();
        let mut tree = BPlusTree::new(store).unwrap();

        let mut inserted = Vec::new();

        for i in 0..10_000 {
            let current_key = key(&format!("{i:05}"));
            let record_id = RecordId::new(PageId::new(i as u64 + 1), 0);

            tree.insert(current_key.clone(), record_id).unwrap();
            inserted.push((current_key, record_id));

            if tree.scan().unwrap().len() > 100 {
                break;
            }
        }

        let entries = tree.scan().unwrap();

        assert_eq!(entries, inserted);

        for (key, record_id) in inserted {
            assert_eq!(tree.search(&key).unwrap(), Some(record_id));
        }
    }
    #[test]
    fn insert_leaf_split_updates_existing_parent() {
        let store = MemoryNodeStore::new();
        let mut tree = BPlusTree::new(store).unwrap();

        let mut inserted = Vec::new();

        for i in 0..10_000u64 {
            let current_key = key(&format!("{i:05}"));
            let record_id = RecordId::new(PageId::new(i + 1), 0);

            tree.insert(current_key.clone(), record_id).unwrap();
            inserted.push((current_key, record_id));

            let entries = tree.scan().unwrap();

            if entries.len() > 100 {
                break;
            }
        }

        assert!(inserted.len() > 1);

        let entries = tree.scan().unwrap();

        assert_eq!(entries, inserted);

        for (current_key, record_id) in inserted {
            assert_eq!(tree.search(&current_key).unwrap(), Some(record_id));
        }
    }

    #[test]
    fn insert_leaf_split_creates_internal_root() {
        let store = MemoryNodeStore::new();
        let mut tree = BPlusTree::new(store).unwrap();

        let original_root = tree.root_page_id;

        let mut inserted = Vec::new();

        for i in 0..10_000u64 {
            let current_key = key(&format!("{i:05}"));
            let record_id = RecordId::new(PageId::new(i + 1), 0);

            tree.insert(current_key.clone(), record_id).unwrap();
            inserted.push((current_key, record_id));

            let root = tree.store.read_node(tree.root_page_id).unwrap();

            if matches!(root, BPlusNode::Internal { .. }) {
                break;
            }
        }

        assert_ne!(tree.root_page_id, original_root);

        let root = tree.store.read_node(tree.root_page_id).unwrap();

        let BPlusNode::Internal { keys, children } = root else {
            panic!("root should be internal after leaf split");
        };

        assert_eq!(children.len(), keys.len() + 1);
        assert!(!keys.is_empty());

        assert_eq!(tree.scan().unwrap(), inserted);
    }

    #[test]
    fn insert_after_root_leaf_split_searches_both_sides() {
        let store = MemoryNodeStore::new();
        let mut tree = BPlusTree::new(store).unwrap();

        let mut inserted = Vec::new();

        for i in 0..10_000u64 {
            let current_key = key(&format!("{i:05}"));
            let record_id = RecordId::new(PageId::new(i + 1), 0);

            tree.insert(current_key.clone(), record_id).unwrap();
            inserted.push((current_key, record_id));

            if matches!(
                tree.store.read_node(tree.root_page_id).unwrap(),
                BPlusNode::Internal { .. }
            ) {
                break;
            }
        }

        assert!(inserted.len() > 2);

        let first = &inserted[0];
        let middle = &inserted[inserted.len() / 2];
        let last = &inserted[inserted.len() - 1];

        assert_eq!(tree.search(&first.0).unwrap(), Some(first.1));
        assert_eq!(tree.search(&middle.0).unwrap(), Some(middle.1));
        assert_eq!(tree.search(&last.0).unwrap(), Some(last.1));
    }

    #[test]
    fn insert_preserves_sorted_scan_after_multiple_leaf_splits() {
        let store = MemoryNodeStore::new();
        let mut tree = BPlusTree::new(store).unwrap();

        let mut expected = Vec::new();

        for i in 0..1_000u64 {
            let current_key = key(&format!("{i:05}"));
            let record_id = RecordId::new(PageId::new(i + 1), 0);

            tree.insert(current_key.clone(), record_id).unwrap();

            expected.push((current_key, record_id));
        }

        let actual = tree.scan().unwrap();

        assert_eq!(actual, expected);
    }

    #[test]
    fn insert_preserves_leaf_chain_after_multiple_splits() {
        let store = MemoryNodeStore::new();
        let mut tree = BPlusTree::new(store).unwrap();

        for i in 0..1_000u64 {
            let current_key = key(&format!("{i:05}"));
            let record_id = RecordId::new(PageId::new(i + 1), 0);

            tree.insert(current_key, record_id).unwrap();
        }

        let mut page_id = tree.root_page_id;

        loop {
            let node = tree.store.read_node(page_id).unwrap();

            match node {
                BPlusNode::Internal { children, .. } => {
                    page_id = children[0];
                }
                BPlusNode::Leaf { .. } => break,
            }
        }

        let mut previous_key: Option<Key> = None;
        let mut count = 0usize;

        loop {
            let node = tree.store.read_node(page_id).unwrap();

            let BPlusNode::Leaf {
                keys,
                values,
                next_leaf,
            } = node
            else {
                panic!("leaf chain contains an internal node");
            };

            assert_eq!(keys.len(), values.len());

            for current_key in &keys {
                if let Some(previous_key) = &previous_key {
                    assert!(previous_key < current_key);
                }

                previous_key = Some(current_key.clone());
                count += 1;
            }

            match next_leaf {
                Some(next_page_id) => {
                    page_id = next_page_id;
                }
                None => break,
            }
        }

        assert_eq!(count, 1_000);
    }

    #[test]
    fn insert_multiple_splits_preserves_every_record() {
        let store = MemoryNodeStore::new();
        let mut tree = BPlusTree::new(store).unwrap();

        let total = 2_000u64;

        let mut expected = Vec::new();

        for i in 0..total {
            let current_key = key(&format!("{i:05}"));
            let record_id = RecordId::new(PageId::new(i + 1), (i % u16::MAX as u64) as u16);

            tree.insert(current_key.clone(), record_id).unwrap();

            expected.push((current_key, record_id));
        }

        assert_eq!(tree.scan().unwrap(), expected);

        for (current_key, record_id) in expected {
            assert_eq!(tree.search(&current_key).unwrap(), Some(record_id));
        }
    }

    #[test]
    fn insert_duplicate_after_multiple_splits_is_rejected() {
        let store = MemoryNodeStore::new();
        let mut tree = BPlusTree::new(store).unwrap();

        for i in 0..1_000u64 {
            let current_key = key(&format!("{i:05}"));
            let record_id = RecordId::new(PageId::new(i + 1), 0);

            tree.insert(current_key, record_id).unwrap();
        }

        let duplicate_key = key("00500");

        let result = tree.insert(
            duplicate_key.clone(),
            RecordId::new(PageId::new(99_999), 99),
        );

        assert!(matches!(result, Err(BPlusTreeError::DuplicateKey)));

        assert_eq!(
            tree.search(&duplicate_key).unwrap(),
            Some(RecordId::new(PageId::new(501), 0))
        );
    }

    #[test]
    fn insert_creates_multiple_internal_children() {
        let store = MemoryNodeStore::new();
        let mut tree = BPlusTree::new(store).unwrap();

        for i in 0..2_000u64 {
            let current_key = key(&format!("{i:05}"));
            let record_id = RecordId::new(PageId::new(i + 1), 0);

            tree.insert(current_key, record_id).unwrap();
        }

        let root = tree.store.read_node(tree.root_page_id).unwrap();

        let BPlusNode::Internal { keys, children } = root else {
            panic!("expected internal root");
        };

        assert!(!keys.is_empty());
        assert!(children.len() > 2);
        assert_eq!(children.len(), keys.len() + 1);
    }

    #[test]
    fn insert_internal_nodes_preserve_invariants() {
        let store = MemoryNodeStore::new();
        let mut tree = BPlusTree::new(store).unwrap();

        for i in 0..2_000u64 {
            let current_key = key(&format!("{i:05}"));
            let record_id = RecordId::new(PageId::new(i + 1), 0);

            tree.insert(current_key, record_id).unwrap();
        }

        fn verify_node<S: NodeStore>(tree: &BPlusTree<S>, page_id: PageId)
        where
            S::Error: std::fmt::Debug,
        {
            let node = tree.store.read_node(page_id).unwrap();

            node.validate().unwrap();

            match node {
                BPlusNode::Internal { children, .. } => {
                    for child_page_id in children {
                        verify_node(tree, child_page_id);
                    }
                }

                BPlusNode::Leaf { keys, values, .. } => {
                    assert_eq!(keys.len(), values.len());
                }
            }
        }

        verify_node(&tree, tree.root_page_id);
    }

    #[test]
    fn insert_multiple_internal_splits_preserves_search() {
        let store = MemoryNodeStore::new();
        let mut tree = BPlusTree::new(store).unwrap();

        let total = 10_000u64;

        for i in 0..total {
            let current_key = key(&format!("{i:05}"));
            let record_id = RecordId::new(PageId::new(i + 1), 0);

            tree.insert(current_key, record_id).unwrap();
        }

        for i in 0..total {
            let current_key = key(&format!("{i:05}"));
            let expected = RecordId::new(PageId::new(i + 1), 0);

            assert_eq!(tree.search(&current_key).unwrap(), Some(expected));
        }
    }

    #[test]
    fn insert_multiple_internal_splits_preserves_scan_order() {
        let store = MemoryNodeStore::new();
        let mut tree = BPlusTree::new(store).unwrap();

        let total = 10_000u64;

        for i in 0..total {
            let current_key = key(&format!("{i:05}"));
            let record_id = RecordId::new(PageId::new(i + 1), 0);

            tree.insert(current_key, record_id).unwrap();
        }

        let entries = tree.scan().unwrap();

        assert_eq!(entries.len(), total as usize);

        for window in entries.windows(2) {
            assert!(window[0].0 < window[1].0);
        }
    }

    #[test]
    fn insert_multiple_internal_splits_root_is_internal() {
        let store = MemoryNodeStore::new();
        let mut tree = BPlusTree::new(store).unwrap();

        for i in 0..10_000u64 {
            let current_key = key(&format!("{i:05}"));
            let record_id = RecordId::new(PageId::new(i + 1), 0);

            tree.insert(current_key, record_id).unwrap();
        }

        let root = tree.store.read_node(tree.root_page_id).unwrap();

        assert!(matches!(root, BPlusNode::Internal { .. }));
    }

    #[test]
    fn insert_separator_equality_routes_to_right_subtree() {
        let store = MemoryNodeStore::new();
        let mut tree = BPlusTree::new(store).unwrap();

        for i in 0..2_000u64 {
            let current_key = key(&format!("{i:05}"));
            let record_id = RecordId::new(PageId::new(i + 1), 0);

            tree.insert(current_key, record_id).unwrap();
        }

        let root = tree.store.read_node(tree.root_page_id).unwrap();

        let BPlusNode::Internal { keys, .. } = root else {
            panic!("expected internal root");
        };

        assert!(!keys.is_empty());

        for separator in keys {
            let result = tree.search(&separator).unwrap();

            assert!(result.is_some());
        }
    }

    #[test]
    fn insert_large_tree_contains_exactly_all_records() {
        let store = MemoryNodeStore::new();
        let mut tree = BPlusTree::new(store).unwrap();

        let total = 5_000u64;

        for i in 0..total {
            let current_key = key(&format!("{i:05}"));
            let record_id = RecordId::new(PageId::new(i + 1), 0);

            tree.insert(current_key, record_id).unwrap();
        }

        let entries = tree.scan().unwrap();

        assert_eq!(entries.len(), total as usize);

        for (index, (current_key, record_id)) in entries.iter().enumerate() {
            assert_eq!(current_key, &key(&format!("{index:05}")));
            assert_eq!(*record_id, RecordId::new(PageId::new(index as u64 + 1), 0));
        }
    }
    #[test]
    fn insert_large_tree_preserves_node_invariants() {
        let store = MemoryNodeStore::new();
        let mut tree = BPlusTree::new(store).unwrap();

        let key_count = 20_000;

        for i in 0..key_count {
            tree.insert(fixed_key(i), record_id(i)).unwrap();
        }

        assert_node_valid(&tree, tree.root_page_id);
    }
    #[test]
    fn insert_multiple_splits_preserve_search() {
        let store = MemoryNodeStore::new();
        let mut tree = BPlusTree::new(store).unwrap();

        let key_count = 20_000;

        for i in 0..key_count {
            tree.insert(fixed_key(i), record_id(i)).unwrap();
        }

        let checkpoints = [0, 1, 10, 100, 1_000, 5_000, 10_000, 15_000, 19_000, 19_999];

        for &i in &checkpoints {
            assert_eq!(tree.search(&fixed_key(i)).unwrap(), Some(record_id(i)));
        }
    }
    #[test]
    fn insert_multiple_splits_preserve_scan_order() {
        let store = MemoryNodeStore::new();
        let mut tree = BPlusTree::new(store).unwrap();

        let key_count = 20_000;

        for i in 0..key_count {
            tree.insert(fixed_key(i), record_id(i)).unwrap();
        }

        let entries = tree.scan().unwrap();

        assert_eq!(entries.len(), key_count);

        for (i, (actual_key, actual_record)) in entries.iter().enumerate() {
            assert_eq!(actual_key, &fixed_key(i));

            assert_eq!(*actual_record, record_id(i));
        }
    }
    #[test]
    fn insert_multiple_splits_preserve_leaf_chain() {
        let store = MemoryNodeStore::new();
        let mut tree = BPlusTree::new(store).unwrap();

        let key_count = 20_000;

        for i in 0..key_count {
            tree.insert(fixed_key(i), record_id(i)).unwrap();
        }

        /*
         * Find the left-most leaf.
         */
        let mut page_id = tree.root_page_id;

        loop {
            let node = tree.store.read_node(page_id).unwrap();

            match node {
                BPlusNode::Internal { children, .. } => {
                    page_id = children[0];
                }

                BPlusNode::Leaf { .. } => {
                    break;
                }
            }
        }

        /*
         * Follow next_leaf until the end.
         */
        let mut previous_key: Option<Key> = None;
        let mut record_count = 0usize;

        loop {
            let node = tree.store.read_node(page_id).unwrap();

            let BPlusNode::Leaf {
                keys,
                values,
                next_leaf,
            } = node
            else {
                panic!("leaf chain pointed to a non-leaf node");
            };

            assert_eq!(keys.len(), values.len());

            for current_key in &keys {
                if let Some(previous) = &previous_key {
                    assert!(previous < current_key, "leaf chain is not strictly sorted");
                }

                previous_key = Some(current_key.clone());

                record_count += 1;
            }

            match next_leaf {
                Some(next_page_id) => {
                    page_id = next_page_id;
                }

                None => break,
            }
        }

        assert_eq!(record_count, key_count);
    }
    #[test]
    fn duplicate_key_is_rejected_after_multiple_splits() {
        let store = MemoryNodeStore::new();
        let mut tree = BPlusTree::new(store).unwrap();

        let key_count = 20_000;

        for i in 0..key_count {
            tree.insert(fixed_key(i), record_id(i)).unwrap();
        }

        let duplicate_key = fixed_key(10_000);

        let result = tree.insert(
            duplicate_key.clone(),
            RecordId::new(PageId::new(999_999), 0),
        );

        assert!(matches!(result, Err(BPlusTreeError::DuplicateKey)));

        assert_eq!(
            tree.search(&duplicate_key).unwrap(),
            Some(record_id(10_000))
        );
    }
    #[test]
    fn missing_keys_remain_absent_after_multiple_splits() {
        let store = MemoryNodeStore::new();
        let mut tree = BPlusTree::new(store).unwrap();

        let key_count = 20_000;

        for i in 0..key_count {
            tree.insert(fixed_key(i), record_id(i)).unwrap();
        }

        let missing_keys = [
            key("00000000-yyyyyyyy"),
            key("00000001-yyyyyyyy"),
            key("00005000-yyyyyyyy"),
            key("00010000-yyyyyyyy"),
            key("99999999-yyyyyyyy"),
        ];

        for missing_key in &missing_keys {
            assert_eq!(tree.search(missing_key).unwrap(), None);
        }
    }
    #[test]
    fn root_remains_valid_after_multiple_splits() {
        let store = MemoryNodeStore::new();
        let mut tree = BPlusTree::new(store).unwrap();

        let key_count = 20_000;

        for i in 0..key_count {
            tree.insert(fixed_key(i), record_id(i)).unwrap();
        }

        let root = tree.store.read_node(tree.root_page_id).unwrap();

        assert!(root.validate().is_ok());

        assert!(root.encoded_len().unwrap() <= PAGE_SIZE);

        match root {
            BPlusNode::Leaf { keys, values, .. } => {
                assert_eq!(keys.len(), values.len());
            }

            BPlusNode::Internal { keys, children } => {
                assert_eq!(children.len(), keys.len() + 1);
            }
        }
    }
    #[test]
    fn large_tree_contains_exactly_all_records() {
        let store = MemoryNodeStore::new();
        let mut tree = BPlusTree::new(store).unwrap();

        let key_count = 20_000;

        for i in 0..key_count {
            tree.insert(fixed_key(i), record_id(i)).unwrap();
        }

        let entries = tree.scan().unwrap();

        assert_eq!(entries.len(), key_count);

        for i in 0..key_count {
            assert_eq!(tree.search(&fixed_key(i)).unwrap(), Some(record_id(i)));
        }
    }
    #[test]
    fn shuffled_insertions_preserve_tree_correctness() {
        let store = MemoryNodeStore::new();
        let mut tree = BPlusTree::new(store).unwrap();

        let key_count = 10_000;

        /*
         * Deterministic permutation.
         *
         * This avoids depending on an external RNG.
         */
        let mut order = Vec::with_capacity(key_count);

        for i in 0..key_count {
            order.push(i);
        }

        order.sort_by_key(|i| (i.wrapping_mul(7_919)) % key_count);

        for &i in &order {
            tree.insert(fixed_key(i), record_id(i)).unwrap();
        }

        for i in 0..key_count {
            assert_eq!(tree.search(&fixed_key(i)).unwrap(), Some(record_id(i)));
        }

        let entries = tree.scan().unwrap();

        assert_eq!(entries.len(), key_count);

        for (i, (key_value, record)) in entries.iter().enumerate() {
            assert_eq!(key_value, &fixed_key(i));

            assert_eq!(*record, record_id(i));
        }

        assert_node_valid(&tree, tree.root_page_id);
    }
}
