use parking_lot::RwLock;

use crate::BPlusNode;
use crate::error::BPlusTreeError;
use crate::key::Key;
use crate::node_store::NodeStore;
use crate::page_id::PageId;
use crate::record_id::RecordId;
use crate::slotted_page::PAGE_SIZE;

pub struct BPlusTree<S: NodeStore> {
    store: S,
    root_page_id: RwLock<PageId>,
    insert_latch: RwLock<()>,
}

impl<S: NodeStore> BPlusTree<S> {
    pub fn new(store: S) -> Result<Self, BPlusTreeError<S::Error>> {
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
            root_page_id: RwLock::new(allocated_root_id),
            insert_latch: RwLock::new(()),
        })
    }

    pub fn scan(&self) -> Result<Vec<(Key, RecordId)>, BPlusTreeError<S::Error>> {
        let mut page_id = *self.root_page_id.read();

        // Descend to the leftmost leaf.
        loop {
            let node = self.store.read_node(page_id)?;
            let node = node.read();

            match &*node {
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
            let node = node.read();

            match &*node {
                BPlusNode::Leaf {
                    keys,
                    values,
                    next_leaf,
                } => {
                    entries.extend(keys.clone().into_iter().zip(values.clone()));

                    match next_leaf {
                        Some(next_page_id) => {
                            page_id = *next_page_id;
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
        let mut page_id = *self.root_page_id.read();
        let mut page = self.store.read_node(page_id)?;

        loop {
            let node = page.read();
            match &*node {
                BPlusNode::Internal { keys, children } => {
                    debug_assert_eq!(children.len(), keys.len() + 1);

                    let child_index = keys.partition_point(|separator| separator <= key);
                    let child_page_id = children[child_index];
                    let child_page = self.store.read_node(child_page_id)?;
                    drop(node);

                    page_id = child_page_id;
                    page = child_page;
                }

                leaf @ BPlusNode::Leaf { .. } => {
                    return Ok((page_id, leaf.clone()));
                }
            }
        }
    }
    fn find_leaf_path(
        &self,
        key: &Key,
    ) -> Result<(Vec<PageId>, PageId, BPlusNode), BPlusTreeError<S::Error>> {
        let mut page_id = *self.root_page_id.read();
        let mut ancestors = Vec::new();

        loop {
            let node = self.store.read_node(page_id)?;
            let node = node.read();

            match &*node {
                BPlusNode::Internal { keys, children } => {
                    debug_assert_eq!(children.len(), keys.len() + 1);

                    ancestors.push(page_id);

                    let child_index = keys.partition_point(|separator| separator <= key);

                    page_id = children[child_index];
                }

                leaf @ BPlusNode::Leaf { .. } => {
                    return Ok((ancestors, page_id, leaf.clone()));
                }
            }
        }
    }
    fn split_leaf(
        &self,
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

        let _lock = self.store.write_node(page_id, &left_leaf)?;

        Ok((right_page_id, separator))
    }
    fn split_internal(
        &self,
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

        let _lock = self.store.write_node(page_id, &left_node)?;

        Ok((right_page_id, promoted_key))
    }

    pub fn insert(&self, key: Key, record_id: RecordId) -> Result<(), BPlusTreeError<S::Error>> {
        let _guard = self.insert_latch.write();
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
            let _lock = self.store.write_node(page_id, &updated_leaf)?;
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
            let (mut keys, mut children) = {
                let node = self.store.read_node(parent_page_id)?;
                let node = node.read();

                let BPlusNode::Internal {
                    keys: existing_keys,
                    children: existing_children,
                } = &*node
                else {
                    return Err(BPlusTreeError::InvalidTreeState(
                        "parent page is not an internal node",
                    ));
                };

                (existing_keys.clone(), existing_children.clone())
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
                let _lock = self.store.write_node(parent_page_id, &updated_parent)?;
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

        *self.root_page_id.write() = new_root_page_id;

        Ok(())
    }

    fn is_underflowed(&self, node: &BPlusNode) -> bool {
        match node {
            BPlusNode::Leaf { keys, .. } => keys.is_empty(),

            BPlusNode::Internal { keys, .. } => keys.is_empty(),
        }
    }

    fn try_borrow_leaf_from_left(
        &self,
        page_id: PageId,
        left_page_id: PageId,
        parent_page_id: PageId,
        child_index: usize,
    ) -> Result<bool, BPlusTreeError<S::Error>> {
        let left_page = self.store.read_node(left_page_id)?;
        let left_guard = left_page.read();

        let BPlusNode::Leaf {
            keys: left_keys,
            values: left_values,
            ..
        } = &*left_guard
        else {
            return Err(BPlusTreeError::InvalidTreeState(
                "left sibling is not a leaf",
            ));
        };

        if left_keys.len() <= 1 {
            return Ok(false);
        }

        let borrowed_key = left_keys
            .last()
            .cloned()
            .ok_or(BPlusTreeError::InvalidTreeState(
                "left sibling unexpectedly empty",
            ))?;

        let borrowed_value = *left_values.last().ok_or(BPlusTreeError::InvalidTreeState(
            "left sibling has no value",
        ))?;

        let mut new_left_keys = left_keys.clone();
        let mut new_left_values = left_values.clone();

        new_left_keys.pop();
        new_left_values.pop();

        drop(left_guard);

        let leaf_page = self.store.read_node(page_id)?;
        let leaf_guard = leaf_page.read();

        let BPlusNode::Leaf {
            keys,
            values,
            next_leaf,
        } = &*leaf_guard
        else {
            return Err(BPlusTreeError::InvalidTreeState(
                "underflowed node is not a leaf",
            ));
        };

        let mut new_keys = keys.clone();
        let mut new_values = values.clone();

        new_keys.insert(0, borrowed_key.clone());
        new_values.insert(0, borrowed_value);

        let new_leaf = BPlusNode::Leaf {
            keys: new_keys,
            values: new_values,
            next_leaf: *next_leaf,
        };

        let new_left = BPlusNode::Leaf {
            keys: new_left_keys,
            values: new_left_values,
            next_leaf: match &*leaf_guard {
                BPlusNode::Leaf { .. } => Some(page_id),
                _ => unreachable!(),
            },
        };

        drop(leaf_guard);

        new_left.validate().map_err(BPlusTreeError::Node)?;
        new_leaf.validate().map_err(BPlusTreeError::Node)?;

        {
            let page = self.store.read_node(left_page_id)?;
            let mut guard = page.write();
            *guard = new_left;
        }

        {
            let page = self.store.read_node(page_id)?;
            let mut guard = page.write();
            *guard = new_leaf;
        }

        /*
         * The first key of the right child is now the
         * new parent separator.
         */
        let page = self.store.read_node(page_id)?;
        let guard = page.read();

        let first_key = match &*guard {
            BPlusNode::Leaf { keys, .. } => keys.first().cloned(),
            _ => None,
        };

        let Some(first_key) = first_key else {
            return Err(BPlusTreeError::InvalidTreeState("borrowed leaf has no key"));
        };

        drop(guard);

        let parent = self.store.read_node(parent_page_id)?;
        let mut parent_guard = parent.write();

        let BPlusNode::Internal { keys, .. } = &mut *parent_guard else {
            return Err(BPlusTreeError::InvalidTreeState(
                "leaf parent is not internal",
            ));
        };

        keys[child_index - 1] = first_key;

        Ok(true)
    }

    fn refresh_parent_separator(
        &self,
        ancestors: &[PageId],
        child_page_id: PageId,
    ) -> Result<(), BPlusTreeError<S::Error>> {
        let Some(&parent_page_id) = ancestors.last() else {
            return Ok(());
        };

        let parent_page = self.store.read_node(parent_page_id)?;
        let parent_guard = parent_page.read();

        let BPlusNode::Internal { keys: _, children } = &*parent_guard else {
            return Err(BPlusTreeError::InvalidTreeState(
                "parent page is not an internal node",
            ));
        };

        let Some(child_index) = children
            .iter()
            .position(|&page_id| page_id == child_page_id)
        else {
            return Err(BPlusTreeError::InvalidTreeState(
                "parent does not contain child page",
            ));
        };

        /*
         * The first child has no separator in the parent.
         */
        if child_index == 0 {
            return Ok(());
        }

        let child = self.store.read_node(child_page_id)?;
        let child_guard = child.read();

        let first_key = match &*child_guard {
            BPlusNode::Leaf { keys, .. } | BPlusNode::Internal { keys, .. } => keys
                .first()
                .cloned()
                .ok_or(BPlusTreeError::InvalidTreeState(
                    "non-root child has no key",
                ))?,
        };

        drop(child_guard);
        drop(parent_guard);

        let parent_page = self.store.read_node(parent_page_id)?;
        let mut parent_guard = parent_page.write();

        let BPlusNode::Internal { keys, .. } = &mut *parent_guard else {
            return Err(BPlusTreeError::InvalidTreeState(
                "parent page is not an internal node",
            ));
        };

        keys[child_index - 1] = first_key;

        Ok(())
    }
    fn try_borrow_leaf_from_right(
        &self,
        page_id: PageId,
        right_page_id: PageId,
        parent_page_id: PageId,
        child_index: usize,
    ) -> Result<bool, BPlusTreeError<S::Error>> {
        let right_page = self.store.read_node(right_page_id)?;
        let right_guard = right_page.read();

        let BPlusNode::Leaf {
            keys: right_keys,
            values: right_values,
            next_leaf,
        } = &*right_guard
        else {
            return Err(BPlusTreeError::InvalidTreeState(
                "right sibling is not a leaf",
            ));
        };

        if right_keys.len() <= 1 {
            return Ok(false);
        }

        let borrowed_key = right_keys[0].clone();
        let borrowed_value = right_values[0];

        let mut new_right_keys = right_keys.clone();
        let mut new_right_values = right_values.clone();

        new_right_keys.remove(0);
        new_right_values.remove(0);

        let right_next_leaf = *next_leaf;

        drop(right_guard);

        let leaf_page = self.store.read_node(page_id)?;
        let leaf_guard = leaf_page.read();

        let BPlusNode::Leaf {
            keys,
            values,
            next_leaf,
        } = &*leaf_guard
        else {
            return Err(BPlusTreeError::InvalidTreeState(
                "underflowed node is not a leaf",
            ));
        };

        let mut new_keys = keys.clone();
        let mut new_values = values.clone();

        new_keys.push(borrowed_key);
        new_values.push(borrowed_value);

        let new_leaf = BPlusNode::Leaf {
            keys: new_keys,
            values: new_values,
            next_leaf: *next_leaf,
        };

        let new_right = BPlusNode::Leaf {
            keys: new_right_keys,
            values: new_right_values,
            next_leaf: right_next_leaf,
        };

        drop(leaf_guard);

        new_leaf.validate().map_err(BPlusTreeError::Node)?;
        new_right.validate().map_err(BPlusTreeError::Node)?;

        {
            let page = self.store.read_node(page_id)?;
            let mut guard = page.write();
            *guard = new_leaf;
        }

        {
            let page = self.store.read_node(right_page_id)?;
            let mut guard = page.write();
            *guard = new_right;
        }

        let right_page = self.store.read_node(right_page_id)?;
        let right_guard = right_page.read();

        let new_separator = match &*right_guard {
            BPlusNode::Leaf { keys, .. } => keys.first().cloned(),
            _ => None,
        };

        let Some(new_separator) = new_separator else {
            return Err(BPlusTreeError::InvalidTreeState(
                "right sibling became empty after borrowing",
            ));
        };

        drop(right_guard);

        let parent = self.store.read_node(parent_page_id)?;
        let mut parent_guard = parent.write();

        let BPlusNode::Internal { keys, .. } = &mut *parent_guard else {
            return Err(BPlusTreeError::InvalidTreeState(
                "leaf parent is not internal",
            ));
        };

        keys[child_index] = new_separator;

        Ok(true)
    }
    fn ensure_page_fits(&self, node: &BPlusNode) -> Result<(), BPlusTreeError<S::Error>> {
        let encoded_len = node.encoded_len().map_err(BPlusTreeError::Node)?;

        if encoded_len > PAGE_SIZE {
            return Err(BPlusTreeError::InvalidTreeState("node exceeds PAGE_SIZE"));
        }

        Ok(())
    }
    fn merge_leaf_into_left(
        &self,
        ancestors: &[PageId],
        parent_page_id: PageId,
        child_index: usize,
        left_page_id: PageId,
        page_id: PageId,
    ) -> Result<(), BPlusTreeError<S::Error>> {
        let left_page = self.store.read_node(left_page_id)?;
        let left_guard = left_page.read();

        let BPlusNode::Leaf {
            keys: left_keys,
            values: left_values,
            ..
        } = &*left_guard
        else {
            return Err(BPlusTreeError::InvalidTreeState(
                "left sibling is not a leaf",
            ));
        };

        let left_keys = left_keys.clone();
        let left_values = left_values.clone();

        drop(left_guard);

        let page = self.store.read_node(page_id)?;
        let page_guard = page.read();

        let BPlusNode::Leaf {
            keys,
            values,
            next_leaf,
        } = &*page_guard
        else {
            return Err(BPlusTreeError::InvalidTreeState(
                "node being merged is not a leaf",
            ));
        };

        let mut merged_keys = left_keys;
        let mut merged_values = left_values;

        merged_keys.extend(keys.clone());
        merged_values.extend(values.clone());

        let merged = BPlusNode::Leaf {
            keys: merged_keys,
            values: merged_values,
            next_leaf: *next_leaf,
        };

        drop(page_guard);

        self.ensure_page_fits(&merged)?;

        {
            let page = self.store.read_node(left_page_id)?;
            let mut guard = page.write();
            *guard = merged;
        }

        self.remove_child_from_parent(ancestors, parent_page_id, child_index)
    }
    fn merge_leaf_with_right(
        &self,
        ancestors: &[PageId],
        parent_page_id: PageId,
        child_index: usize,
        page_id: PageId,
        right_page_id: PageId,
    ) -> Result<(), BPlusTreeError<S::Error>> {
        let page = self.store.read_node(page_id)?;
        let page_guard = page.read();

        let BPlusNode::Leaf { keys, values, .. } = &*page_guard else {
            return Err(BPlusTreeError::InvalidTreeState(
                "node being merged is not a leaf",
            ));
        };

        let mut merged_keys = keys.clone();
        let mut merged_values = values.clone();

        drop(page_guard);

        let right_page = self.store.read_node(right_page_id)?;
        let right_guard = right_page.read();

        let BPlusNode::Leaf {
            keys: right_keys,
            values: right_values,
            next_leaf,
        } = &*right_guard
        else {
            return Err(BPlusTreeError::InvalidTreeState(
                "right sibling is not a leaf",
            ));
        };

        merged_keys.extend(right_keys.clone());
        merged_values.extend(right_values.clone());

        let merged = BPlusNode::Leaf {
            keys: merged_keys,
            values: merged_values,
            next_leaf: *next_leaf,
        };

        drop(right_guard);

        self.ensure_page_fits(&merged)?;

        {
            let page = self.store.read_node(page_id)?;
            let mut guard = page.write();
            *guard = merged;
        }

        self.remove_child_from_parent(ancestors, parent_page_id, child_index + 1)
    }
    fn remove_child_from_parent(
        &self,
        ancestors: &[PageId],
        parent_page_id: PageId,
        child_index: usize,
    ) -> Result<(), BPlusTreeError<S::Error>> {
        let parent_page = self.store.read_node(parent_page_id)?;
        let parent_guard = parent_page.read();

        let BPlusNode::Internal { keys, children } = &*parent_guard else {
            return Err(BPlusTreeError::InvalidTreeState(
                "parent is not an internal node",
            ));
        };

        let mut new_keys = keys.clone();
        let mut new_children = children.clone();

        drop(parent_guard);

        /*
         * Removing child i removes:
         *
         * separator i-1 when i > 0
         *
         * otherwise separator 0
         */
        let separator_index = if child_index == 0 { 0 } else { child_index - 1 };

        if new_children.len() <= 1 {
            return Err(BPlusTreeError::InvalidTreeState(
                "parent cannot remove its final child",
            ));
        }

        new_children.remove(child_index);

        if !new_keys.is_empty() {
            new_keys.remove(separator_index);
        }

        let updated_parent = BPlusNode::Internal {
            keys: new_keys,
            children: new_children,
        };

        updated_parent.validate().map_err(BPlusTreeError::Node)?;

        /*
         * Root contraction.
         */
        if ancestors.len() == 1 {
            let is_single_child = match &updated_parent {
                BPlusNode::Internal { children, .. } => children.len() == 1,
                _ => false,
            };

            if is_single_child {
                let new_root = match &updated_parent {
                    BPlusNode::Internal { children, .. } => children[0],
                    _ => unreachable!(),
                };

                *self.root_page_id.write() = new_root;

                return Ok(());
            }
        }

        self.ensure_page_fits(&updated_parent)?;

        {
            let page = self.store.read_node(parent_page_id)?;
            let mut guard = page.write();
            *guard = updated_parent.clone();
        }

        /*
         * Parent still has at least one key.
         */
        if !self.is_underflowed(&updated_parent) {
            return Ok(());
        }

        /*
         * Parent itself underflowed.
         *
         * Continue upward.
         */
        self.repair_internal_underflow(&ancestors[..ancestors.len() - 1], parent_page_id)
    }
    fn repair_internal_underflow(
        &self,
        ancestors: &[PageId],
        page_id: PageId,
    ) -> Result<(), BPlusTreeError<S::Error>> {
        let Some(&parent_page_id) = ancestors.last() else {
            return Err(BPlusTreeError::InvalidTreeState(
                "underflowed internal node has no parent",
            ));
        };

        let parent_page = self.store.read_node(parent_page_id)?;
        let parent_guard = parent_page.read();

        let BPlusNode::Internal {
            keys: parent_keys,
            children: parent_children,
        } = &*parent_guard
        else {
            return Err(BPlusTreeError::InvalidTreeState(
                "internal node parent is not an internal node",
            ));
        };

        let Some(child_index) = parent_children.iter().position(|&child| child == page_id) else {
            return Err(BPlusTreeError::InvalidTreeState(
                "parent does not contain underflowed child",
            ));
        };

        let left_page_id = child_index
            .checked_sub(1)
            .and_then(|index| parent_children.get(index))
            .copied();

        let right_page_id = parent_children.get(child_index + 1).copied();

        /*
         * The separator immediately to the left of this child.
         *
         * parent_keys[i] separates children[i] and children[i + 1].
         */
        let separator_from_left = child_index
            .checked_sub(1)
            .and_then(|index| parent_keys.get(index))
            .cloned();

        /*
         * The separator immediately to the right of this child.
         */
        let separator_from_right = parent_keys.get(child_index).cloned();

        drop(parent_guard);

        // ------------------------------------------------------------
        // 1. Try borrowing from the left sibling.
        // ------------------------------------------------------------

        if let (Some(left_page_id), Some(parent_separator)) =
            (left_page_id, separator_from_left.clone())
            && self.try_borrow_internal_from_left(
                page_id,
                left_page_id,
                parent_page_id,
                parent_separator,
                child_index,
            )?
        {
            return Ok(());
        }

        // ------------------------------------------------------------
        // 2. Try borrowing from the right sibling.
        // ------------------------------------------------------------

        if let (Some(right_page_id), Some(parent_separator)) =
            (right_page_id, separator_from_right.clone())
            && self.try_borrow_internal_from_right(
                page_id,
                right_page_id,
                parent_page_id,
                parent_separator,
                child_index,
            )?
        {
            return Ok(());
        }

        // ------------------------------------------------------------
        // 3. Neither sibling can lend.
        //
        // Prefer merging into the left sibling.
        // ------------------------------------------------------------

        if let (Some(left_page_id), Some(parent_separator)) =
            (left_page_id, separator_from_left.clone())
        {
            return self.merge_internal_into_left(
                ancestors,
                parent_page_id,
                child_index,
                left_page_id,
                page_id,
                parent_separator,
            );
        }

        // ------------------------------------------------------------
        // 4. No left sibling, so merge with the right sibling.
        // ------------------------------------------------------------

        if let (Some(right_page_id), Some(parent_separator)) =
            (right_page_id, separator_from_right.clone())
        {
            return self.merge_internal_with_right(
                ancestors,
                parent_page_id,
                child_index,
                page_id,
                right_page_id,
                parent_separator,
            );
        }

        Err(BPlusTreeError::InvalidTreeState(
            "underflowed internal node has no sibling",
        ))
    }
    fn merge_internal_with_right(
        &self,
        ancestors: &[PageId],
        parent_page_id: PageId,
        child_index: usize,
        page_id: PageId,
        right_page_id: PageId,
        parent_separator: Key,
    ) -> Result<(), BPlusTreeError<S::Error>> {
        let page = self.store.read_node(page_id)?;
        let page_guard = page.read();

        let BPlusNode::Internal { keys, children } = &*page_guard else {
            return Err(BPlusTreeError::InvalidTreeState(
                "node being merged is not internal",
            ));
        };

        let mut merged_keys = keys.clone();
        let mut merged_children = children.clone();

        drop(page_guard);

        let right_page = self.store.read_node(right_page_id)?;
        let right_guard = right_page.read();

        let BPlusNode::Internal {
            keys: right_keys,
            children: right_children,
        } = &*right_guard
        else {
            return Err(BPlusTreeError::InvalidTreeState(
                "right sibling is not internal",
            ));
        };

        merged_keys.push(parent_separator);
        merged_keys.extend(right_keys.clone());

        merged_children.extend(right_children.clone());

        drop(right_guard);

        let merged = BPlusNode::Internal {
            keys: merged_keys,
            children: merged_children,
        };

        merged.validate().map_err(BPlusTreeError::Node)?;

        self.ensure_page_fits(&merged)?;

        {
            let page = self.store.read_node(page_id)?;
            let mut guard = page.write();
            *guard = merged;
        }

        /*
         * Remove the right child from the parent.
         */
        self.remove_child_from_parent(ancestors, parent_page_id, child_index + 1)
    }
    fn try_borrow_internal_from_left(
        &self,
        page_id: PageId,
        left_page_id: PageId,
        parent_page_id: PageId,
        parent_separator: Key,
        child_index: usize,
    ) -> Result<bool, BPlusTreeError<S::Error>> {
        let left_page = self.store.read_node(left_page_id)?;
        let left_guard = left_page.read();

        let BPlusNode::Internal {
            keys: left_keys,
            children: left_children,
        } = &*left_guard
        else {
            return Err(BPlusTreeError::InvalidTreeState(
                "left sibling is not an internal node",
            ));
        };

        /*
         * The left sibling must retain at least one key.
         */
        if left_keys.len() <= 1 {
            return Ok(false);
        }

        let borrowed_key = left_keys
            .last()
            .cloned()
            .ok_or(BPlusTreeError::InvalidTreeState(
                "left internal sibling has no key",
            ))?;

        let borrowed_child =
            left_children
                .last()
                .copied()
                .ok_or(BPlusTreeError::InvalidTreeState(
                    "left internal sibling has no child",
                ))?;

        let mut new_left_keys = left_keys.clone();
        let mut new_left_children = left_children.clone();

        new_left_keys.pop();
        new_left_children.pop();

        drop(left_guard);

        let child_page = self.store.read_node(page_id)?;
        let child_guard = child_page.read();

        let BPlusNode::Internal { keys, children } = &*child_guard else {
            return Err(BPlusTreeError::InvalidTreeState(
                "underflowed node is not internal",
            ));
        };

        let mut new_keys = keys.clone();
        let mut new_children = children.clone();

        /*
         * Parent separator moves DOWN into the underflowed node.
         */
        new_keys.insert(0, parent_separator);

        /*
         * The borrowed child becomes the first child.
         */
        new_children.insert(0, borrowed_child);

        let new_left = BPlusNode::Internal {
            keys: new_left_keys,
            children: new_left_children,
        };

        let new_child = BPlusNode::Internal {
            keys: new_keys,
            children: new_children,
        };

        drop(child_guard);

        new_left.validate().map_err(BPlusTreeError::Node)?;

        new_child.validate().map_err(BPlusTreeError::Node)?;

        self.ensure_page_fits(&new_left)?;
        self.ensure_page_fits(&new_child)?;

        /*
         * The key that moved upward becomes the new parent separator.
         */
        let new_parent_separator = borrowed_key;

        {
            let page = self.store.read_node(left_page_id)?;
            let mut guard = page.write();
            *guard = new_left;
        }

        {
            let page = self.store.read_node(page_id)?;
            let mut guard = page.write();
            *guard = new_child;
        }

        {
            let page = self.store.read_node(parent_page_id)?;
            let mut guard = page.write();

            let BPlusNode::Internal { keys, .. } = &mut *guard else {
                return Err(BPlusTreeError::InvalidTreeState("parent is not internal"));
            };

            keys[child_index - 1] = new_parent_separator;
        }

        Ok(true)
    }
    fn try_borrow_internal_from_right(
        &self,
        page_id: PageId,
        right_page_id: PageId,
        parent_page_id: PageId,
        parent_separator: Key,
        child_index: usize,
    ) -> Result<bool, BPlusTreeError<S::Error>> {
        let right_page = self.store.read_node(right_page_id)?;
        let right_guard = right_page.read();

        let BPlusNode::Internal {
            keys: right_keys,
            children: right_children,
        } = &*right_guard
        else {
            return Err(BPlusTreeError::InvalidTreeState(
                "right sibling is not an internal node",
            ));
        };

        /*
         * The right sibling must retain at least one key.
         */
        if right_keys.len() <= 1 {
            return Ok(false);
        }

        let borrowed_key = right_keys
            .first()
            .cloned()
            .ok_or(BPlusTreeError::InvalidTreeState(
                "right internal sibling has no key",
            ))?;

        let borrowed_child =
            right_children
                .first()
                .copied()
                .ok_or(BPlusTreeError::InvalidTreeState(
                    "right internal sibling has no child",
                ))?;

        let mut new_right_keys = right_keys.clone();
        let mut new_right_children = right_children.clone();

        new_right_keys.remove(0);
        new_right_children.remove(0);

        drop(right_guard);

        let child_page = self.store.read_node(page_id)?;
        let child_guard = child_page.read();

        let BPlusNode::Internal { keys, children } = &*child_guard else {
            return Err(BPlusTreeError::InvalidTreeState(
                "underflowed node is not internal",
            ));
        };

        let mut new_keys = keys.clone();
        let mut new_children = children.clone();

        /*
         * Parent separator moves DOWN into the underflowed node.
         */
        new_keys.push(parent_separator);

        /*
         * Borrowed child becomes the last child.
         */
        new_children.push(borrowed_child);

        let new_child = BPlusNode::Internal {
            keys: new_keys,
            children: new_children,
        };

        let new_right = BPlusNode::Internal {
            keys: new_right_keys,
            children: new_right_children,
        };

        drop(child_guard);

        new_child.validate().map_err(BPlusTreeError::Node)?;

        new_right.validate().map_err(BPlusTreeError::Node)?;

        self.ensure_page_fits(&new_child)?;
        self.ensure_page_fits(&new_right)?;

        /*
         * First key of the right sibling becomes
         * the new parent separator.
         */
        let new_parent_separator = borrowed_key;

        {
            let page = self.store.read_node(page_id)?;
            let mut guard = page.write();
            *guard = new_child;
        }

        {
            let page = self.store.read_node(right_page_id)?;
            let mut guard = page.write();
            *guard = new_right;
        }

        {
            let page = self.store.read_node(parent_page_id)?;
            let mut guard = page.write();

            let BPlusNode::Internal { keys, .. } = &mut *guard else {
                return Err(BPlusTreeError::InvalidTreeState("parent is not internal"));
            };

            keys[child_index] = new_parent_separator;
        }

        Ok(true)
    }
    fn merge_internal_into_left(
        &self,
        ancestors: &[PageId],
        parent_page_id: PageId,
        child_index: usize,
        left_page_id: PageId,
        page_id: PageId,
        parent_separator: Key,
    ) -> Result<(), BPlusTreeError<S::Error>> {
        let left_page = self.store.read_node(left_page_id)?;
        let left_guard = left_page.read();

        let BPlusNode::Internal {
            keys: left_keys,
            children: left_children,
        } = &*left_guard
        else {
            return Err(BPlusTreeError::InvalidTreeState(
                "left sibling is not internal",
            ));
        };

        let mut merged_keys = left_keys.clone();
        let mut merged_children = left_children.clone();

        drop(left_guard);

        let page = self.store.read_node(page_id)?;
        let page_guard = page.read();

        let BPlusNode::Internal { keys, children } = &*page_guard else {
            return Err(BPlusTreeError::InvalidTreeState(
                "node being merged is not internal",
            ));
        };

        /*
         * Parent separator is pulled down between
         * the two sets of keys.
         */
        merged_keys.push(parent_separator);
        merged_keys.extend(keys.clone());

        merged_children.extend(children.clone());

        drop(page_guard);

        let merged = BPlusNode::Internal {
            keys: merged_keys,
            children: merged_children,
        };

        merged.validate().map_err(BPlusTreeError::Node)?;

        self.ensure_page_fits(&merged)?;

        {
            let page = self.store.read_node(left_page_id)?;
            let mut guard = page.write();
            *guard = merged;
        }

        /*
         * Remove the underflowed child from its parent.
         */
        self.remove_child_from_parent(ancestors, parent_page_id, child_index)
    }
    fn repair_leaf_underflow(
        &self,
        ancestors: &[PageId],
        page_id: PageId,
        _leaf: BPlusNode,
    ) -> Result<(), BPlusTreeError<S::Error>> {
        let Some(&parent_page_id) = ancestors.last() else {
            return Err(BPlusTreeError::InvalidTreeState(
                "non-root leaf has no parent",
            ));
        };

        let parent_page = self.store.read_node(parent_page_id)?;
        let parent_guard = parent_page.read();

        let BPlusNode::Internal {
            keys: parent_keys,
            children,
        } = &*parent_guard
        else {
            return Err(BPlusTreeError::InvalidTreeState(
                "leaf parent is not internal",
            ));
        };

        let Some(child_index) = children.iter().position(|&child| child == page_id) else {
            return Err(BPlusTreeError::InvalidTreeState(
                "parent does not contain underflowed leaf",
            ));
        };

        let left_page_id = child_index
            .checked_sub(1)
            .and_then(|index| children.get(index))
            .copied();

        let right_page_id = children.get(child_index + 1).copied();

        let _parent_separator = if child_index > 0 {
            Some(parent_keys[child_index - 1].clone())
        } else {
            None
        };

        drop(parent_guard);

        /*
         * Prefer borrowing from the left sibling.
         */
        if let Some(left_page_id) = left_page_id
            && self.try_borrow_leaf_from_left(page_id, left_page_id, parent_page_id, child_index)?
        {
            return Ok(());
        }

        /*
         * Then try the right sibling.
         */
        if let Some(right_page_id) = right_page_id
            && self.try_borrow_leaf_from_right(
                page_id,
                right_page_id,
                parent_page_id,
                child_index,
            )?
        {
            return Ok(());
        }

        /*
         * Neither sibling can lend.
         * Merge with the left when possible, otherwise merge right.
         */
        if let Some(left_page_id) = left_page_id {
            return self.merge_leaf_into_left(
                ancestors,
                parent_page_id,
                child_index,
                left_page_id,
                page_id,
            );
        }

        if let Some(right_page_id) = right_page_id {
            return self.merge_leaf_with_right(
                ancestors,
                parent_page_id,
                child_index,
                page_id,
                right_page_id,
            );
        }

        Err(BPlusTreeError::InvalidTreeState(
            "underflowed leaf has no sibling",
        ))
    }

    pub fn delete(&self, key: &Key) -> Result<Option<RecordId>, BPlusTreeError<S::Error>> {
        let _delete_guard = self.insert_latch.write();

        let (ancestors, page_id, node) = self.find_leaf_path(key)?;

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

        let Ok(index) = keys.binary_search(key) else {
            return Ok(None);
        };

        let removed_value = values[index];

        keys.remove(index);
        values.remove(index);

        let updated_leaf = BPlusNode::Leaf {
            keys,
            values,
            next_leaf,
        };

        /*
         * Root leaf may legally become empty.
         */
        if ancestors.is_empty() {
            let page = self.store.read_node(page_id)?;
            let mut guard = page.write();
            *guard = updated_leaf;

            return Ok(Some(removed_value));
        }

        /*
         * A non-root leaf with at least one key is not underflowed.
         */
        if !self.is_underflowed(&updated_leaf) {
            let page = self.store.read_node(page_id)?;
            let mut guard = page.write();
            *guard = updated_leaf;

            self.refresh_parent_separator(&ancestors, page_id)?;

            return Ok(Some(removed_value));
        }

        /*
         * Leaf underflow requires redistribution or merge.
         */
        self.repair_leaf_underflow(&ancestors, page_id, updated_leaf)?;

        Ok(Some(removed_value))
    }
}

#[cfg(test)]
impl<S: NodeStore> BPlusTree<S> {
    fn from_root(store: S, root_page_id: PageId) -> Self {
        Self {
            store,
            root_page_id: RwLock::new(root_page_id),
            insert_latch: RwLock::new(()),
        }
    }
    fn is_safe_for_insert(
        &self,
        node: &BPlusNode,
        additional_len: usize,
    ) -> Result<bool, BPlusTreeError<S::Error>> {
        let encoded_len = node.encoded_len().map_err(BPlusTreeError::Node)?;

        Ok(encoded_len
            .checked_add(additional_len)
            .is_some_and(|new_len| new_len <= PAGE_SIZE))
    }
}

#[cfg(test)]
pub mod tree_tests {
    use std::sync::Arc;

    use parking_lot::{Mutex, RwLock};

    use crate::BPlusNode;
    use crate::error::BPlusTreeError;
    use crate::key::Key;
    use crate::node_store::NodeStore;
    use crate::page_id::PageId;
    use crate::record_id::RecordId;
    use crate::slotted_page::{PAGE_SIZE, SlotId};
    use crate::storage::memory::{MemoryNodeStore, MemoryNodeStoreError};
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
        let node = node.read();

        assert!(node.validate().is_ok(), "invalid node at page {page_id:?}");

        assert!(
            node.encoded_len().unwrap() <= PAGE_SIZE,
            "node exceeds PAGE_SIZE at page {page_id:?}",
        );

        match &*node {
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
                    assert_node_valid(tree, *child_page_id);
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
        let store = MemoryNodeStore::new();

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
        let store = MemoryNodeStore::new();

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
        let store = MemoryNodeStore::new();

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

        let lock = store.write_node(left_leaf_id, &left_leaf).unwrap();

        drop(lock);
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
        let store = MemoryNodeStore::new();

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
        let store = MemoryNodeStore::new();

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
        let store = MemoryNodeStore::new();

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
        let store = MemoryNodeStore::new();

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
        let store = MemoryNodeStore::new();

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
        let store = MemoryNodeStore::new();

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
        let store = MemoryNodeStore::new();

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
        let store = MemoryNodeStore::new();

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
        let store = MemoryNodeStore::new();

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
        let store = MemoryNodeStore::new();

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
        let store = MemoryNodeStore::new();

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
        let store = MemoryNodeStore::new();

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
        let store = MemoryNodeStore::new();

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
        let store = MemoryNodeStore::new();

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
        let store = MemoryNodeStore::new();

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
        let store = MemoryNodeStore::new();

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
        let store = MemoryNodeStore::new();

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
        let store = MemoryNodeStore::new();

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
        let tree = BPlusTree::new(store).unwrap();

        let key = key("a");
        let record_id = record(1, 0);

        tree.insert(key.clone(), record_id).unwrap();

        assert_eq!(tree.search(&key).unwrap(), Some(record_id));

        let result = tree.scan().unwrap();

        assert_eq!(result, vec![(key, record_id)]);
    }

    #[test]
    fn insert_at_beginning() {
        let store = MemoryNodeStore::new();

        let leaf = BPlusNode::Leaf {
            keys: vec![key("b"), key("c")],
            values: vec![record(1, 1), record(1, 2)],
            next_leaf: None,
        };

        let root_id = store.allocate_node(&leaf).unwrap();

        let tree = BPlusTree::from_root(store, root_id);

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
        let store = MemoryNodeStore::new();

        let leaf = BPlusNode::Leaf {
            keys: vec![key("a"), key("c")],
            values: vec![record(1, 0), record(1, 2)],
            next_leaf: None,
        };

        let root_id = store.allocate_node(&leaf).unwrap();

        let tree = BPlusTree::from_root(store, root_id);

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
        let store = MemoryNodeStore::new();

        let leaf = BPlusNode::Leaf {
            keys: vec![key("a"), key("b")],
            values: vec![record(1, 0), record(1, 1)],
            next_leaf: None,
        };

        let root_id = store.allocate_node(&leaf).unwrap();

        let tree = BPlusTree::from_root(store, root_id);

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
        let tree = BPlusTree::new(store).unwrap();

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
        let tree = BPlusTree::new(store).unwrap();

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
        let tree = BPlusTree::new(store).unwrap();

        tree.insert(key("a"), record(1, 0)).unwrap();

        let result = tree.insert(key("a"), record(2, 0));

        assert!(matches!(result, Err(BPlusTreeError::DuplicateKey)));
    }

    #[test]
    fn duplicate_insert_does_not_modify_existing_value() {
        let store = MemoryNodeStore::new();
        let tree = BPlusTree::new(store).unwrap();

        tree.insert(key("a"), record(1, 0)).unwrap();

        let result = tree.insert(key("a"), record(2, 0));

        assert!(matches!(result, Err(BPlusTreeError::DuplicateKey)));

        assert_eq!(tree.search(&key("a")).unwrap(), Some(record(1, 0)));
    }

    #[test]
    fn duplicate_insert_does_not_modify_tree() {
        let store = MemoryNodeStore::new();
        let tree = BPlusTree::new(store).unwrap();

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
        let tree = BPlusTree::new(store).unwrap();

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
        let tree = BPlusTree::new(store).unwrap();

        tree.insert(key("hello"), record(42, 7)).unwrap();

        assert_eq!(tree.search(&key("hello")).unwrap(), Some(record(42, 7)));
    }

    #[test]
    fn insert_then_scan_returns_inserted_entries() {
        let store = MemoryNodeStore::new();
        let tree = BPlusTree::new(store).unwrap();

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
        let store = MemoryNodeStore::new();

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

        let tree = BPlusTree::from_root(store, root_id);

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
        assert_eq!(leaf_page_id, *tree.root_page_id.read());
        assert!(matches!(node, BPlusNode::Leaf { .. }));
    }

    #[test]
    fn find_leaf_path_one_internal_level() {
        let store = MemoryNodeStore::new();

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
        let store = MemoryNodeStore::new();

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
        let store = MemoryNodeStore::new();

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
        let store = MemoryNodeStore::new();

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
        let store = MemoryNodeStore::new();

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

        let tree = BPlusTree::from_root(store, leaf_page_id);

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
        let right_node = right_node.read();

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

        match &*right_node {
            BPlusNode::Leaf {
                keys,
                values,
                next_leaf,
            } => {
                assert_eq!(keys, &vec![key("0004"), key("0005"), key("0006")]);

                assert_eq!(values, &vec![record(1, 4), record(1, 5), record(1, 6),]);

                assert_eq!(next_leaf, &None);
            }

            BPlusNode::Internal { .. } => {
                panic!("expected right leaf");
            }
        }
    }
    #[test]
    fn split_leaf_rightmost_sets_right_next_to_none() {
        let store = MemoryNodeStore::new();

        let leaf = BPlusNode::Leaf {
            keys: vec![key("0001"), key("0002"), key("0003"), key("0004")],
            values: vec![record(1, 1), record(1, 2), record(1, 3), record(1, 4)],
            next_leaf: None,
        };

        let leaf_page_id = store.allocate_node(&leaf).unwrap();

        let tree = BPlusTree::from_root(store, leaf_page_id);

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

        let left_node = left_node.read();
        let right_node = right_node.read();

        match &*left_node {
            BPlusNode::Leaf { next_leaf, .. } => {
                assert_eq!(next_leaf, &Some(right_page_id));
            }

            BPlusNode::Internal { .. } => {
                panic!("expected left leaf");
            }
        }

        match &*right_node {
            BPlusNode::Leaf { next_leaf, .. } => {
                assert_eq!(next_leaf, &None);
            }

            BPlusNode::Internal { .. } => {
                panic!("expected right leaf");
            }
        }
    }
    #[test]
    fn split_leaf_separator_is_first_right_key() {
        let store = MemoryNodeStore::new();

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

        let tree = BPlusTree::from_root(store, leaf_page_id);

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
        let right_node = right_node.read();

        match &*right_node {
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
        let store = MemoryNodeStore::new();

        let leaf = BPlusNode::Leaf {
            keys: vec![key("0001"), key("0002"), key("0003"), key("0004")],
            values: vec![record(1, 1), record(1, 2), record(1, 3), record(1, 4)],
            next_leaf: None,
        };

        let leaf_page_id = store.allocate_node(&leaf).unwrap();

        let tree = BPlusTree::from_root(store, leaf_page_id);

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
        let left_node = left_node.read();

        match &*left_node {
            BPlusNode::Leaf { keys, .. } => {
                assert_eq!(keys, &vec![key("0001"), key("0002")]);
            }

            BPlusNode::Internal { .. } => {
                panic!("original page must remain the left leaf");
            }
        }
    }
    #[test]
    fn split_leaf_preserves_sorted_order() {
        let store = MemoryNodeStore::new();

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

        let tree = BPlusTree::from_root(store, leaf_page_id);

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
        let left_node = left_node.read();
        let right_node = tree.store.read_node(right_page_id).unwrap();
        let right_node = right_node.read();

        let (left_keys, right_keys) = match (&*left_node, &*right_node) {
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
        let tree = BPlusTree::new(store).unwrap();

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
                let node = tree.store.read_node(*tree.root_page_id.read()).unwrap();

                let node = node.read();
                if matches!(&*node, BPlusNode::Internal { .. }) {
                    break;
                }
            }
        }

        let root = tree.store.read_node(*tree.root_page_id.read()).unwrap();

        let root = root.read();
        match &*root {
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
        let tree = BPlusTree::new(store).unwrap();

        let mut expected = Vec::new();

        for i in 0..1000u16 {
            let current_key = key(&format!("{i:04}"));
            let current_record = record(1, i);

            tree.insert(current_key.clone(), current_record).unwrap();

            expected.push((current_key, current_record));

            let root = tree.store.read_node(*tree.root_page_id.read()).unwrap();
            let root = root.read();

            if matches!(&*root, BPlusNode::Internal { .. }) {
                break;
            }
        }

        let actual = tree.scan().unwrap();

        assert_eq!(actual, expected);
    }
    #[test]
    fn insert_root_split_updates_root_page_id() {
        let store = MemoryNodeStore::new();
        let tree = BPlusTree::new(store).unwrap();

        let original_root_page_id = *tree.root_page_id.read();

        for i in 0..1000u16 {
            tree.insert(key(&format!("{i:04}")), record(1, i)).unwrap();

            let root = tree.store.read_node(*tree.root_page_id.read()).unwrap();

            let root = root.read();
            if matches!(&*root, BPlusNode::Internal { .. }) {
                break;
            }
        }

        assert_ne!(*tree.root_page_id.read(), original_root_page_id);
    }
    #[test]
    fn insert_root_split_keeps_old_root_as_left_leaf() {
        let store = MemoryNodeStore::new();
        let tree = BPlusTree::new(store).unwrap();

        let old_root_page_id = *tree.root_page_id.read();

        for i in 0..1000u16 {
            tree.insert(key(&format!("{i:04}")), record(1, i)).unwrap();

            let root = tree.store.read_node(*tree.root_page_id.read()).unwrap();

            let root = root.read();
            if let BPlusNode::Internal { .. } = &*root {
                break;
            }
        }

        let root = tree.store.read_node(*tree.root_page_id.read()).unwrap();

        let root = root.read();
        let left_page_id = match &*root {
            BPlusNode::Internal { children, .. } => children[0],
            BPlusNode::Leaf { .. } => {
                panic!("root should be internal");
            }
        };

        assert_eq!(left_page_id, old_root_page_id);

        let left = tree.store.read_node(left_page_id).unwrap();
        let left = left.read();

        assert!(matches!(&*left, BPlusNode::Leaf { .. }));
    }
    #[test]
    fn insert_root_split_creates_two_leaf_children() {
        let store = MemoryNodeStore::new();
        let tree = BPlusTree::new(store).unwrap();

        for i in 0..1000u16 {
            tree.insert(key(&format!("{i:04}")), record(1, i)).unwrap();

            let root = tree.store.read_node(*tree.root_page_id.read()).unwrap();
            let root = root.read();
            if matches!(&*root, BPlusNode::Internal { .. }) {
                break;
            }
        }

        let root = tree.store.read_node(*tree.root_page_id.read()).unwrap();

        let root = root.read();
        let children = match &*root {
            BPlusNode::Internal { children, .. } => children,

            BPlusNode::Leaf { .. } => {
                panic!("root should be internal");
            }
        };

        assert_eq!(children.len(), 2);

        for child_page_id in children {
            let child = tree.store.read_node(*child_page_id).unwrap();
            let child = child.read();
            assert!(matches!(&*child, BPlusNode::Leaf { .. }));
        }
    }
    #[test]
    fn insert_root_split_separator_is_first_right_key() {
        let store = MemoryNodeStore::new();
        let tree = BPlusTree::new(store).unwrap();

        for i in 0..1000u16 {
            tree.insert(key(&format!("{i:04}")), record(1, i)).unwrap();

            let root = tree.store.read_node(*tree.root_page_id.read()).unwrap();

            let root = root.read();
            if matches!(&*root, BPlusNode::Internal { .. }) {
                break;
            }
        }

        let root = tree.store.read_node(*tree.root_page_id.read()).unwrap();

        let root = root.read();
        let (separator, right_page_id) = match &*root {
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

        let right = right.read();
        match &*right {
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
        let tree = BPlusTree::new(store).unwrap();

        for i in 0..1000u16 {
            tree.insert(key(&format!("{i:04}")), record(1, i)).unwrap();

            let root = tree.store.read_node(*tree.root_page_id.read()).unwrap();

            let root = root.read();
            if matches!(&*root, BPlusNode::Internal { .. }) {
                break;
            }
        }

        let root = tree.store.read_node(*tree.root_page_id.read()).unwrap();

        let root = root.read();
        let children = match &*root {
            BPlusNode::Internal { children, .. } => children,

            BPlusNode::Leaf { .. } => {
                panic!("root should be internal");
            }
        };

        let left = tree.store.read_node(children[0]).unwrap();

        let left = left.read();
        let right_page_id = match &*left {
            BPlusNode::Leaf { next_leaf, .. } => {
                next_leaf.expect("left leaf should point to right leaf")
            }

            BPlusNode::Internal { .. } => {
                panic!("left child should be leaf");
            }
        };

        assert_eq!(right_page_id, children[1]);

        let right = tree.store.read_node(right_page_id).unwrap();
        let right = right.read();
        match &*right {
            BPlusNode::Leaf { next_leaf, .. } => {
                assert_eq!(next_leaf, &None);
            }

            BPlusNode::Internal { .. } => {
                panic!("right child should be leaf");
            }
        }
    }
    #[test]
    fn insert_root_split_searches_both_leaves() {
        let store = MemoryNodeStore::new();
        let tree = BPlusTree::new(store).unwrap();

        let mut inserted = Vec::new();

        for i in 0..1000u16 {
            let current_key = key(&format!("{i:04}"));
            let current_record = record(1, i);

            tree.insert(current_key.clone(), current_record).unwrap();

            inserted.push((current_key, current_record));

            let root = tree.store.read_node(*tree.root_page_id.read()).unwrap();
            let root = root.read();
            if matches!(&*root, BPlusNode::Internal { .. }) {
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
        let tree = BPlusTree::new(store).unwrap();

        for i in 0..1000u16 {
            tree.insert(key(&format!("{i:04}")), record(1, i)).unwrap();

            let root = tree.store.read_node(*tree.root_page_id.read()).unwrap();

            let root = root.read();
            if matches!(&*root, BPlusNode::Internal { .. }) {
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
        let tree = BPlusTree::new(store).unwrap();

        for i in 0..1000u16 {
            tree.insert(key(&format!("{i:04}")), record(1, i)).unwrap();

            let root = tree.store.read_node(*tree.root_page_id.read()).unwrap();

            let root = root.read();
            if matches!(&*root, BPlusNode::Internal { .. }) {
                break;
            }
        }

        let root = tree.store.read_node(*tree.root_page_id.read()).unwrap();

        let root = root.read();
        let separator = match &*root {
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
        let tree = BPlusTree::new(store).unwrap();

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

        let left = left.read();
        let right = right.read();
        assert_eq!(
            &*left,
            &BPlusNode::Internal {
                keys: vec![key("a"), key("b")],
                children: vec![PageId::new(1), PageId::new(2), PageId::new(3),],
            }
        );

        assert_eq!(
            &*right,
            &BPlusNode::Internal {
                keys: vec![key("d"), key("e")],
                children: vec![PageId::new(4), PageId::new(5), PageId::new(6),],
            }
        );
    }

    #[test]
    fn split_internal_promoted_key_is_removed_from_children() {
        let store = MemoryNodeStore::new();
        let tree = BPlusTree::new(store).unwrap();

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

        let left = left.read();
        let right = right.read();
        let BPlusNode::Internal {
            keys: left_keys, ..
        } = &*left
        else {
            panic!("left node must be internal");
        };

        let BPlusNode::Internal {
            keys: right_keys, ..
        } = &*right
        else {
            panic!("right node must be internal");
        };

        assert!(!left_keys.contains(&promoted_key));
        assert!(!right_keys.contains(&promoted_key));
    }

    #[test]
    fn split_internal_preserves_all_keys() {
        let store = MemoryNodeStore::new();
        let tree = BPlusTree::new(store).unwrap();

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

        let left = left.read();
        let right = right.read();
        let BPlusNode::Internal {
            keys: left_keys, ..
        } = &*left
        else {
            panic!("left node must be internal");
        };

        let BPlusNode::Internal {
            keys: right_keys, ..
        } = &*right
        else {
            panic!("right node must be internal");
        };

        let mut reconstructed_keys = left_keys.clone();
        reconstructed_keys.push(promoted_key);
        reconstructed_keys.extend(right_keys.clone());

        assert_eq!(reconstructed_keys, keys);
    }

    #[test]
    fn split_internal_preserves_all_children() {
        let store = MemoryNodeStore::new();
        let tree = BPlusTree::new(store).unwrap();

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

        let left = left.read();
        let right = right.read();
        let BPlusNode::Internal {
            children: left_children,
            ..
        } = &*left
        else {
            panic!("left node must be internal");
        };

        let BPlusNode::Internal {
            children: right_children,
            ..
        } = &*right
        else {
            panic!("right node must be internal");
        };

        let mut reconstructed_children = left_children.clone();
        reconstructed_children.extend(right_children);

        assert_eq!(reconstructed_children, children);
    }

    #[test]
    fn split_internal_preserves_internal_cardinality() {
        let store = MemoryNodeStore::new();
        let tree = BPlusTree::new(store).unwrap();

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

        let left = left.read();
        let right = right.read();
        let BPlusNode::Internal {
            keys: left_keys,
            children: left_children,
        } = &*left
        else {
            panic!("left node must be internal");
        };

        let BPlusNode::Internal {
            keys: right_keys,
            children: right_children,
        } = &*right
        else {
            panic!("right node must be internal");
        };

        assert_eq!(left_children.len(), left_keys.len() + 1);
        assert_eq!(right_children.len(), right_keys.len() + 1);
    }

    #[test]
    fn split_internal_preserves_sorted_order() {
        let store = MemoryNodeStore::new();
        let tree = BPlusTree::new(store).unwrap();

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

        let left = left.read();
        let right = right.read();
        let BPlusNode::Internal {
            keys: left_keys, ..
        } = &*left
        else {
            panic!("left node must be internal");
        };

        let BPlusNode::Internal {
            keys: right_keys, ..
        } = &*right
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
        let tree = BPlusTree::new(store).unwrap();

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

        let left = left.read();

        assert_eq!(
            &*left,
            &BPlusNode::Internal {
                keys: vec![key("a"), key("b")],
                children: vec![PageId::new(1), PageId::new(2), PageId::new(3),],
            }
        );
    }

    #[test]
    fn split_internal_allocates_right_node() {
        let store = MemoryNodeStore::new();
        let tree = BPlusTree::new(store).unwrap();

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

        let right = right.read();
        assert_eq!(
            &*right,
            &BPlusNode::Internal {
                keys: vec![key("d"), key("e")],
                children: vec![PageId::new(4), PageId::new(5), PageId::new(6),],
            }
        );
    }

    #[test]
    fn split_internal_resulting_nodes_validate() {
        let store = MemoryNodeStore::new();
        let tree = BPlusTree::new(store).unwrap();

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

        let left = left.read();
        let right = right.read();
        assert!(left.validate().is_ok());
        assert!(right.validate().is_ok());
    }

    #[test]
    fn split_internal_resulting_nodes_fit_page_size() {
        let store = MemoryNodeStore::new();
        let tree = BPlusTree::new(store).unwrap();

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

        let left = left.read();
        let right = right.read();
        assert!(left.encoded_len().unwrap() <= PAGE_SIZE);
        assert!(right.encoded_len().unwrap() <= PAGE_SIZE);
    }

    #[test]
    fn split_internal_with_even_key_count() {
        let store = MemoryNodeStore::new();
        let tree = BPlusTree::new(store).unwrap();

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

        let left = left.read();
        let right = right.read();
        assert_eq!(
            &*left,
            &BPlusNode::Internal {
                keys: vec![key("a"), key("b")],
                children: vec![PageId::new(1), PageId::new(2), PageId::new(3),],
            }
        );

        assert_eq!(
            &*right,
            &BPlusNode::Internal {
                keys: vec![key("d")],
                children: vec![PageId::new(4), PageId::new(5),],
            }
        );
    }
    #[test]
    fn insert_after_leaf_full_splits_leaf() {
        let store = MemoryNodeStore::new();
        let tree = BPlusTree::new(store).unwrap();

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
        let tree = BPlusTree::new(store).unwrap();

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
        let tree = BPlusTree::new(store).unwrap();

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
        let tree = BPlusTree::new(store).unwrap();

        let original_root = *tree.root_page_id.read();

        let mut inserted = Vec::new();

        for i in 0..10_000u64 {
            let current_key = key(&format!("{i:05}"));
            let record_id = RecordId::new(PageId::new(i + 1), 0);

            tree.insert(current_key.clone(), record_id).unwrap();
            inserted.push((current_key, record_id));

            let root = tree.store.read_node(*tree.root_page_id.read()).unwrap();

            let root = root.read();
            if matches!(&*root, BPlusNode::Internal { .. }) {
                break;
            }
        }

        assert_ne!(*tree.root_page_id.read(), original_root);

        let root = tree.store.read_node(*tree.root_page_id.read()).unwrap();

        let root = root.read();
        let BPlusNode::Internal { keys, children } = &*root else {
            panic!("root should be internal after leaf split");
        };

        assert_eq!(children.len(), keys.len() + 1);
        assert!(!keys.is_empty());

        assert_eq!(tree.scan().unwrap(), inserted);
    }

    #[test]
    fn insert_after_root_leaf_split_searches_both_sides() {
        let store = MemoryNodeStore::new();
        let tree = BPlusTree::new(store).unwrap();

        let mut inserted = Vec::new();

        for i in 0..10_000u64 {
            let current_key = key(&format!("{i:05}"));
            let record_id = RecordId::new(PageId::new(i + 1), 0);

            tree.insert(current_key.clone(), record_id).unwrap();
            inserted.push((current_key, record_id));
            let node = tree.store.read_node(*tree.root_page_id.read()).unwrap();
            let node = node.read();
            if matches!(&*node, BPlusNode::Internal { .. }) {
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
        let tree = BPlusTree::new(store).unwrap();

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
        let tree = BPlusTree::new(store).unwrap();

        for i in 0..1_000u64 {
            let current_key = key(&format!("{i:05}"));
            let record_id = RecordId::new(PageId::new(i + 1), 0);

            tree.insert(current_key, record_id).unwrap();
        }

        let mut page_id = *tree.root_page_id.read();

        loop {
            let node = tree.store.read_node(page_id).unwrap();

            let node = node.read();
            match &*node {
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

            let node = node.read();
            let BPlusNode::Leaf {
                keys,
                values,
                next_leaf,
            } = &*node
            else {
                panic!("leaf chain contains an internal node");
            };

            assert_eq!(keys.len(), values.len());

            for current_key in keys {
                if let Some(previous_key) = &previous_key {
                    assert!(previous_key < current_key);
                }

                previous_key = Some(current_key.clone());
                count += 1;
            }

            match next_leaf {
                Some(next_page_id) => {
                    page_id = *next_page_id;
                }
                None => break,
            }
        }

        assert_eq!(count, 1_000);
    }

    #[test]
    fn insert_multiple_splits_preserves_every_record() {
        let store = MemoryNodeStore::new();
        let tree = BPlusTree::new(store).unwrap();

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
        let tree = BPlusTree::new(store).unwrap();

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
        let tree = BPlusTree::new(store).unwrap();

        for i in 0..2_000u64 {
            let current_key = key(&format!("{i:05}"));
            let record_id = RecordId::new(PageId::new(i + 1), 0);

            tree.insert(current_key, record_id).unwrap();
        }

        let root = tree.store.read_node(*tree.root_page_id.read()).unwrap();

        let root = root.read();
        let BPlusNode::Internal { keys, children } = &*root else {
            panic!("expected internal root");
        };

        assert!(!keys.is_empty());
        assert!(children.len() > 2);
        assert_eq!(children.len(), keys.len() + 1);
    }

    #[test]
    fn insert_internal_nodes_preserve_invariants() {
        let store = MemoryNodeStore::new();
        let tree = BPlusTree::new(store).unwrap();

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
            let node = node.read();
            node.validate().unwrap();

            match &*node {
                BPlusNode::Internal { children, .. } => {
                    for child_page_id in children {
                        verify_node(tree, *child_page_id);
                    }
                }

                BPlusNode::Leaf { keys, values, .. } => {
                    assert_eq!(keys.len(), values.len());
                }
            }
        }

        verify_node(&tree, *tree.root_page_id.read());
    }

    #[test]
    fn insert_multiple_internal_splits_preserves_search() {
        let store = MemoryNodeStore::new();
        let tree = BPlusTree::new(store).unwrap();

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
        let tree = BPlusTree::new(store).unwrap();

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
        let tree = BPlusTree::new(store).unwrap();

        for i in 0..10_000u64 {
            let current_key = key(&format!("{i:05}"));
            let record_id = RecordId::new(PageId::new(i + 1), 0);

            tree.insert(current_key, record_id).unwrap();
        }

        let root = tree.store.read_node(*tree.root_page_id.read()).unwrap();

        let root = root.read();
        assert!(matches!(&*root, BPlusNode::Internal { .. }));
    }

    #[test]
    fn insert_separator_equality_routes_to_right_subtree() {
        let store = MemoryNodeStore::new();
        let tree = BPlusTree::new(store).unwrap();

        for i in 0..2_000u64 {
            let current_key = key(&format!("{i:05}"));
            let record_id = RecordId::new(PageId::new(i + 1), 0);

            tree.insert(current_key, record_id).unwrap();
        }

        let root = tree.store.read_node(*tree.root_page_id.read()).unwrap();

        let root = root.read();
        let BPlusNode::Internal { keys, .. } = &*root else {
            panic!("expected internal root");
        };

        assert!(!keys.is_empty());

        for separator in keys {
            let result = tree.search(separator).unwrap();

            assert!(result.is_some());
        }
    }

    #[test]
    fn insert_large_tree_contains_exactly_all_records() {
        let store = MemoryNodeStore::new();
        let tree = BPlusTree::new(store).unwrap();

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
        let tree = BPlusTree::new(store).unwrap();

        let key_count = 20_000;

        for i in 0..key_count {
            tree.insert(fixed_key(i), record_id(i)).unwrap();
        }

        assert_node_valid(&tree, *tree.root_page_id.read());
    }
    #[test]
    fn insert_multiple_splits_preserve_search() {
        let store = MemoryNodeStore::new();
        let tree = BPlusTree::new(store).unwrap();

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
        let tree = BPlusTree::new(store).unwrap();

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
        let tree = BPlusTree::new(store).unwrap();

        let key_count = 20_000;

        for i in 0..key_count {
            tree.insert(fixed_key(i), record_id(i)).unwrap();
        }

        /*
         * Find the left-most leaf.
         */
        let mut page_id = *tree.root_page_id.read();

        loop {
            let node = tree.store.read_node(page_id).unwrap();

            let node = node.read();
            match &*node {
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

            let node = node.read();
            let BPlusNode::Leaf {
                keys,
                values,
                next_leaf,
            } = &*node
            else {
                panic!("leaf chain pointed to a non-leaf node");
            };

            assert_eq!(keys.len(), values.len());

            for current_key in keys {
                if let Some(previous) = &previous_key {
                    assert!(previous < current_key, "leaf chain is not strictly sorted");
                }

                previous_key = Some(current_key.clone());

                record_count += 1;
            }

            match next_leaf {
                Some(next_page_id) => {
                    page_id = *next_page_id;
                }

                None => break,
            }
        }

        assert_eq!(record_count, key_count);
    }
    #[test]
    fn duplicate_key_is_rejected_after_multiple_splits() {
        let store = MemoryNodeStore::new();
        let tree = BPlusTree::new(store).unwrap();

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
        let tree = BPlusTree::new(store).unwrap();

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
        let tree = BPlusTree::new(store).unwrap();

        let key_count = 20_000;

        for i in 0..key_count {
            tree.insert(fixed_key(i), record_id(i)).unwrap();
        }

        let root = tree.store.read_node(*tree.root_page_id.read()).unwrap();
        let root = root.read();
        assert!(root.validate().is_ok());

        assert!(root.encoded_len().unwrap() <= PAGE_SIZE);

        match &*root {
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
        let tree = BPlusTree::new(store).unwrap();

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
        let tree = BPlusTree::new(store).unwrap();

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

        assert_node_valid(&tree, *tree.root_page_id.read());
    }
    #[test]
    fn split_leaf_results_fit_page_size() {
        let store = MemoryNodeStore::new();
        let tree = BPlusTree::new(store).unwrap();

        let mut keys = Vec::new();
        let mut values = Vec::new();

        for i in 0..10_000u64 {
            let candidate_key = key(&format!("{i:08}-{}", "x".repeat(20)));

            keys.push(candidate_key);
            values.push(RecordId::new(PageId::new(i + 1), 0));

            let candidate = BPlusNode::Leaf {
                keys: keys.clone(),
                values: values.clone(),
                next_leaf: None,
            };

            if candidate.encoded_len().unwrap() > PAGE_SIZE {
                keys.pop();
                values.pop();
                break;
            }
        }

        /*
         * Make the candidate overflow.
         */
        keys.push(key(&format!("{:08}-{}", keys.len(), "y".repeat(20))));

        values.push(RecordId::new(PageId::new(999_999), 0));

        let original = BPlusNode::Leaf {
            keys: keys.clone(),
            values: values.clone(),
            next_leaf: None,
        };

        let page_id = tree.store.allocate_node(&original).unwrap();

        let (right_page_id, separator) = tree.split_leaf(page_id, keys, values, None).unwrap();

        let left = tree.store.read_node(page_id).unwrap();
        let right = tree.store.read_node(right_page_id).unwrap();
        let left = left.read();
        let right = right.read();

        assert!(left.encoded_len().unwrap() <= PAGE_SIZE);
        assert!(right.encoded_len().unwrap() <= PAGE_SIZE);

        assert!(left.validate().is_ok());
        assert!(right.validate().is_ok());

        let BPlusNode::Leaf {
            keys: right_keys, ..
        } = &*right
        else {
            panic!("right split node must be a leaf");
        };

        assert_eq!(separator, right_keys[0]);
    }
    #[test]
    fn split_leaf_preserves_every_key_and_value_exactly() {
        let store = MemoryNodeStore::new();
        let tree = BPlusTree::new(store).unwrap();

        let keys: Vec<Key> = (0..100).map(|i| key(&format!("{i:04}"))).collect();

        let values: Vec<RecordId> = (0..100)
            .map(|i| RecordId::new(PageId::new(i + 1), 0))
            .collect();

        let original = BPlusNode::Leaf {
            keys: keys.clone(),
            values: values.clone(),
            next_leaf: None,
        };

        let page_id = tree.store.allocate_node(&original).unwrap();

        let (right_page_id, separator) = tree
            .split_leaf(page_id, keys.clone(), values.clone(), None)
            .unwrap();

        let left = tree.store.read_node(page_id).unwrap();
        let right = tree.store.read_node(right_page_id).unwrap();

        let left = left.read();
        let right = right.read();
        let BPlusNode::Leaf {
            keys: left_keys,
            values: left_values,
            next_leaf: left_next,
        } = &*left
        else {
            panic!("left node must be leaf");
        };

        let BPlusNode::Leaf {
            keys: right_keys,
            values: right_values,
            next_leaf: right_next,
        } = &*right
        else {
            panic!("right node must be leaf");
        };

        let mut combined_keys = left_keys.clone();
        combined_keys.extend(right_keys.clone());

        let mut combined_values = left_values.clone();
        combined_values.extend(right_values.clone());

        assert_eq!(combined_keys, keys);
        assert_eq!(combined_values, values);

        assert_eq!(separator, right_keys[0]);

        assert_eq!(left_next, &Some(right_page_id));

        assert_eq!(right_next, &None);
    }
    #[test]
    fn split_leaf_preserves_key_value_alignment() {
        let store = MemoryNodeStore::new();
        let tree = BPlusTree::new(store).unwrap();

        let keys: Vec<Key> = (0..101).map(|i| key(&format!("{i:04}"))).collect();

        let values: Vec<RecordId> = (0..101)
            .map(|i| RecordId::new(PageId::new(100_000 + i), i as u16))
            .collect();

        let original = BPlusNode::Leaf {
            keys: keys.clone(),
            values: values.clone(),
            next_leaf: None,
        };

        let page_id = tree.store.allocate_node(&original).unwrap();

        let (right_page_id, _) = tree.split_leaf(page_id, keys, values, None).unwrap();

        for page in [page_id, right_page_id] {
            let node = tree.store.read_node(page).unwrap();

            let node = node.read();
            let BPlusNode::Leaf { keys, values, .. } = &*node else {
                panic!("expected leaf");
            };

            assert_eq!(keys.len(), values.len());

            for (key_value, record_id) in keys.iter().zip(values.iter()) {
                let key_number: usize = String::from_utf8(key_value.as_bytes().to_vec())
                    .unwrap()
                    .parse()
                    .unwrap();

                assert_eq!(
                    record_id.page_id(),
                    PageId::new(100_000 + key_number as u64)
                );

                assert_eq!(record_id.slot_id(), key_number as u16);
            }
        }
    }
    #[test]
    fn split_internal_preserves_every_child_exactly() {
        let store = MemoryNodeStore::new();
        let tree = BPlusTree::new(store).unwrap();

        let keys: Vec<Key> = (0..101).map(|i| key(&format!("{i:04}"))).collect();

        let children: Vec<PageId> = (0..=101).map(|i| PageId::new(i + 10_000)).collect();

        let original = BPlusNode::Internal {
            keys: keys.clone(),
            children: children.clone(),
        };

        let page_id = tree.store.allocate_node(&original).unwrap();

        let (right_page_id, promoted) = tree
            .split_internal(page_id, keys.clone(), children.clone())
            .unwrap();

        let left = tree.store.read_node(page_id).unwrap();
        let right = tree.store.read_node(right_page_id).unwrap();

        let left = left.read();
        let right = right.read();
        let BPlusNode::Internal {
            keys: left_keys,
            children: left_children,
        } = &*left
        else {
            panic!("left node must be internal");
        };

        let BPlusNode::Internal {
            keys: right_keys,
            children: right_children,
        } = &*right
        else {
            panic!("right node must be internal");
        };

        /*
         * The promoted separator belongs to neither child.
         */
        assert!(!left_keys.contains(&promoted));

        assert!(!right_keys.contains(&promoted));

        /*
         * Every original child must still exist exactly once.
         */
        let mut combined_children = left_children.clone();

        combined_children.extend(right_children.clone());

        assert_eq!(combined_children, children);
    }
    #[test]
    fn split_internal_preserves_every_non_promoted_key() {
        let store = MemoryNodeStore::new();
        let tree = BPlusTree::new(store).unwrap();

        let keys: Vec<Key> = (0..101).map(|i| key(&format!("{i:04}"))).collect();

        let children: Vec<PageId> = (0..=101).map(|i| PageId::new(i + 20_000)).collect();

        let original = BPlusNode::Internal {
            keys: keys.clone(),
            children,
        };

        let page_id = tree.store.allocate_node(&original).unwrap();

        let (right_page_id, promoted) = tree
            .split_internal(
                page_id,
                keys.clone(),
                (0..=101).map(|i| PageId::new(i + 20_000)).collect(),
            )
            .unwrap();

        let left = tree.store.read_node(page_id).unwrap();
        let right = tree.store.read_node(right_page_id).unwrap();

        let left = left.read();
        let right = right.read();
        let BPlusNode::Internal {
            keys: left_keys, ..
        } = &*left
        else {
            panic!("expected internal");
        };

        let BPlusNode::Internal {
            keys: right_keys, ..
        } = &*right
        else {
            panic!("expected internal");
        };

        let mut combined = left_keys.clone();

        combined.extend(right_keys.clone());

        let expected: Vec<Key> = keys
            .into_iter()
            .filter(|key_value| key_value != &promoted)
            .collect();

        assert_eq!(combined, expected);
    }
    #[test]
    fn split_internal_preserves_global_key_order() {
        let store = MemoryNodeStore::new();
        let tree = BPlusTree::new(store).unwrap();

        let keys: Vec<Key> = (0..101).map(|i| key(&format!("{i:04}"))).collect();

        let children: Vec<PageId> = (0..=101).map(|i| PageId::new(i + 30_000)).collect();

        let node = BPlusNode::Internal {
            keys: keys.clone(),
            children: children.clone(),
        };

        let page_id = tree.store.allocate_node(&node).unwrap();

        let (right_page_id, promoted) = tree.split_internal(page_id, keys, children).unwrap();

        let left = tree.store.read_node(page_id).unwrap();
        let right = tree.store.read_node(right_page_id).unwrap();

        let left = left.read();
        let right = right.read();
        let BPlusNode::Internal {
            keys: left_keys, ..
        } = &*left
        else {
            panic!("expected internal");
        };

        let BPlusNode::Internal {
            keys: right_keys, ..
        } = &*right
        else {
            panic!("expected internal");
        };

        assert!(left_keys.windows(2).all(|pair| pair[0] < pair[1]));

        assert!(right_keys.windows(2).all(|pair| pair[0] < pair[1]));

        assert!(
            left_keys
                .last()
                .is_none_or(|key_value| key_value < &promoted)
        );

        assert!(
            right_keys
                .first()
                .is_none_or(|key_value| &promoted < key_value)
        );
    }
    #[test]
    fn split_leaf_preserves_existing_next_leaf() {
        let store = MemoryNodeStore::new();
        let tree = BPlusTree::new(store).unwrap();

        let next_page_id = tree
            .store
            .allocate_node(&BPlusNode::Leaf {
                keys: vec![key("zzzz")],
                values: vec![RecordId::new(PageId::new(99_999), 0)],
                next_leaf: None,
            })
            .unwrap();

        let keys: Vec<Key> = (0..100).map(|i| key(&format!("{i:04}"))).collect();

        let values: Vec<RecordId> = (0..100).map(record_id).collect();

        let original = BPlusNode::Leaf {
            keys: keys.clone(),
            values: values.clone(),
            next_leaf: Some(next_page_id),
        };

        let page_id = tree.store.allocate_node(&original).unwrap();

        let (right_page_id, _) = tree
            .split_leaf(page_id, keys, values, Some(next_page_id))
            .unwrap();

        let left = tree.store.read_node(page_id).unwrap();

        let right = tree.store.read_node(right_page_id).unwrap();

        let left = left.read();
        let right = right.read();
        let BPlusNode::Leaf {
            next_leaf: left_next,
            ..
        } = &*left
        else {
            panic!("expected leaf");
        };

        let BPlusNode::Leaf {
            next_leaf: right_next,
            ..
        } = &*right
        else {
            panic!("expected leaf");
        };

        assert_eq!(left_next, &Some(right_page_id));

        assert_eq!(right_next, &Some(next_page_id));
    }
    #[test]
    fn duplicate_keys_are_rejected_at_all_positions_after_splits() {
        let store = MemoryNodeStore::new();
        let tree = BPlusTree::new(store).unwrap();

        let key_count = 10_000;

        for i in 0..key_count {
            tree.insert(fixed_key(i), record_id(i)).unwrap();
        }

        let duplicate_indices = [0, key_count / 2, key_count - 1];

        for &i in &duplicate_indices {
            let result = tree.insert(fixed_key(i), RecordId::new(PageId::new(999_999), 999));

            assert!(
                matches!(result, Err(BPlusTreeError::DuplicateKey)),
                "duplicate key {i} was not rejected"
            );

            assert_eq!(tree.search(&fixed_key(i)).unwrap(), Some(record_id(i)));
        }
    }
    #[test]
    fn separator_equality_routes_to_right_leaf_after_split() {
        let store = MemoryNodeStore::new();
        let tree = BPlusTree::new(store).unwrap();

        let count = 1_000;

        for i in 0..count {
            tree.insert(fixed_key(i), record_id(i)).unwrap();
        }

        let entries = tree.scan().unwrap();

        assert_eq!(entries.len(), count);

        for i in 0..count {
            assert_eq!(tree.search(&fixed_key(i)).unwrap(), Some(record_id(i)));
        }
    }
    #[test]
    fn separator_equality_routes_correctly_after_internal_splits() {
        let store = MemoryNodeStore::new();
        let tree = BPlusTree::new(store).unwrap();

        let key_count = 20_000;

        for i in 0..key_count {
            tree.insert(fixed_key(i), record_id(i)).unwrap();
        }

        /*
         * Check many likely separator positions.
         */
        let positions = [
            100, 500, 1_000, 2_500, 5_000, 7_500, 10_000, 12_500, 15_000, 17_500, 19_000,
        ];

        for &i in &positions {
            assert_eq!(tree.search(&fixed_key(i)).unwrap(), Some(record_id(i)));
        }
    }
    #[test]
    fn interleaved_insertions_preserve_sorted_scan() {
        let store = MemoryNodeStore::new();
        let tree = BPlusTree::new(store).unwrap();

        for i in (0..10_000).step_by(2) {
            tree.insert(fixed_key(i), record_id(i)).unwrap();
        }

        for i in (1..10_000).step_by(2) {
            tree.insert(fixed_key(i), record_id(i)).unwrap();
        }

        let entries = tree.scan().unwrap();

        assert_eq!(entries.len(), 10_000);

        for (i, (actual_key, actual_record)) in entries.iter().enumerate() {
            assert_eq!(actual_key, &fixed_key(i));

            assert_eq!(*actual_record, record_id(i));
        }
    }
    #[test]
    fn duplicate_insert_does_not_modify_tree_after_splits() {
        let store = MemoryNodeStore::new();
        let tree = BPlusTree::new(store).unwrap();

        let key_count = 10_000;

        for i in 0..key_count {
            tree.insert(fixed_key(i), record_id(i)).unwrap();
        }

        let before = tree.scan().unwrap();

        let duplicate_key = fixed_key(5_000);

        let result = tree.insert(duplicate_key, RecordId::new(PageId::new(777_777), 777));

        assert!(matches!(result, Err(BPlusTreeError::DuplicateKey)));

        let after = tree.scan().unwrap();

        assert_eq!(before, after);
    }
    #[test]
    fn insert_before_existing_separator_preserves_routing() {
        let store = MemoryNodeStore::new();
        let tree = BPlusTree::new(store).unwrap();

        /*
         * First insert all even keys.
         *
         * key 0 -> record_id(0)
         * key 2 -> record_id(2)
         * key 4 -> record_id(4)
         * ...
         */
        for i in 0..10_000usize {
            let key_number = i * 2;

            tree.insert(fixed_key(key_number), record_id(key_number))
                .unwrap();
        }

        /*
         * Now insert all odd keys between the existing keys.
         *
         * key 1 -> record_id(1)
         * key 3 -> record_id(3)
         * key 5 -> record_id(5)
         * ...
         */
        for i in 0..10_000usize {
            let key_number = i * 2 + 1;

            tree.insert(fixed_key(key_number), record_id(key_number))
                .unwrap();
        }

        /*
         * The final key space is:
         *
         * 0, 1, 2, 3, 4, 5, ..., 19_999
         *
         * Every key must map to its corresponding record.
         */
        for key_number in 0..20_000usize {
            assert_eq!(
                tree.search(&fixed_key(key_number)).unwrap(),
                Some(record_id(key_number)),
                "search failed for key {key_number}",
            );
        }

        /*
         * Scan must contain every key exactly once and remain
         * globally sorted.
         */
        let entries = tree.scan().unwrap();

        assert_eq!(entries.len(), 20_000);

        for (index, (actual_key, actual_record)) in entries.iter().enumerate() {
            assert_eq!(
                actual_key,
                &fixed_key(index),
                "unexpected key at scan position {index}",
            );

            assert_eq!(
                *actual_record,
                record_id(index),
                "unexpected record at scan position {index}",
            );
        }

        /*
         * Final structural validation.
         */
        assert_node_valid(&tree, *tree.root_page_id.read());
    }
    #[test]
    fn randomized_insertion_order_preserves_tree_correctness() {
        let store = MemoryNodeStore::new();
        let tree = BPlusTree::new(store).unwrap();

        let key_count = 20_000;

        /*
         * Generate a deterministic permutation.
         *
         * The multiplication produces a different insertion order
         * while remaining completely reproducible.
         */
        let mut order: Vec<usize> = (0..key_count).collect();

        order.sort_by_key(|value| value.wrapping_mul(7_919) % key_count);

        /*
         * Insert according to the permutation.
         */
        for &i in &order {
            tree.insert(fixed_key(i), record_id(i)).unwrap();
        }

        /*
         * Every key must be searchable.
         */
        for i in 0..key_count {
            assert_eq!(
                tree.search(&fixed_key(i)).unwrap(),
                Some(record_id(i)),
                "missing key {i}",
            );
        }

        /*
         * Scan must return keys in sorted order regardless
         * of insertion order.
         */
        let entries = tree.scan().unwrap();

        assert_eq!(entries.len(), key_count);

        for (i, (actual_key, actual_record)) in entries.iter().enumerate() {
            assert_eq!(actual_key, &fixed_key(i), "wrong key at scan position {i}");

            assert_eq!(
                *actual_record,
                record_id(i),
                "wrong record at scan position {i}",
            );
        }

        /*
         * Validate the complete reachable tree.
         */
        assert_node_valid(&tree, *tree.root_page_id.read());
    }
    #[test]
    fn reverse_order_insertion_preserves_tree_correctness() {
        let store = MemoryNodeStore::new();
        let tree = BPlusTree::new(store).unwrap();

        let key_count = 10_000;

        for i in (0..key_count).rev() {
            tree.insert(fixed_key(i), record_id(i)).unwrap();
        }

        for i in 0..key_count {
            assert_eq!(tree.search(&fixed_key(i)).unwrap(), Some(record_id(i)),);
        }

        let entries = tree.scan().unwrap();

        assert_eq!(entries.len(), key_count);

        for (i, (actual_key, actual_record)) in entries.iter().enumerate() {
            assert_eq!(actual_key, &fixed_key(i));

            assert_eq!(*actual_record, record_id(i));
        }

        assert_node_valid(&tree, *tree.root_page_id.read());
    }
    #[test]
    fn alternating_insertion_order_preserves_tree_correctness() {
        let store = MemoryNodeStore::new();
        let tree = BPlusTree::new(store).unwrap();

        let key_count = 10_000;

        let mut order = Vec::with_capacity(key_count);

        let mut left = 0usize;
        let mut right = key_count - 1;

        while left <= right {
            order.push(left);

            if left == right {
                break;
            }

            order.push(right);

            left += 1;
            right -= 1;
        }

        for &i in &order {
            tree.insert(fixed_key(i), record_id(i)).unwrap();
        }

        let entries = tree.scan().unwrap();

        assert_eq!(entries.len(), key_count);

        for (i, (actual_key, actual_record)) in entries.iter().enumerate() {
            assert_eq!(actual_key, &fixed_key(i));

            assert_eq!(*actual_record, record_id(i));
        }

        assert_node_valid(&tree, *tree.root_page_id.read());
    }
    #[test]
    fn randomized_insertion_duplicate_detection() {
        let store = MemoryNodeStore::new();
        let tree = BPlusTree::new(store).unwrap();

        let key_count = 10_000;

        let mut order: Vec<usize> = (0..key_count).collect();

        order.sort_by_key(|value| value.wrapping_mul(7_919) % key_count);

        for &i in &order {
            tree.insert(fixed_key(i), record_id(i)).unwrap();
        }

        /*
         * Try duplicates in positions spread across the tree.
         */
        for i in [0, 1, 127, 1_000, 2_500, 5_000, 7_500, 9_999] {
            let result = tree.insert(fixed_key(i), RecordId::new(PageId::new(999_999), 999));

            assert!(
                matches!(result, Err(BPlusTreeError::DuplicateKey)),
                "duplicate key {i} was accepted"
            );

            assert_eq!(tree.search(&fixed_key(i)).unwrap(), Some(record_id(i)));
        }
    }

    // Custom Failing test implementation.
    #[derive(Debug)]
    struct FailingNodeStore {
        inner: MemoryNodeStore,
        fail_writes_after: Mutex<Option<usize>>,
        fail_allocations_after: Mutex<Option<usize>>,
    }
    impl NodeStore for FailingNodeStore {
        type Error = MemoryNodeStoreError;

        fn read_node(&self, page_id: PageId) -> Result<Arc<RwLock<BPlusNode>>, Self::Error> {
            self.inner.read_node(page_id)
        }

        fn write_node(
            &self,
            page_id: PageId,
            node: &BPlusNode,
        ) -> Result<Arc<RwLock<BPlusNode>>, Self::Error> {
            if let Some(remaining) = self.fail_writes_after.lock().as_mut() {
                if *remaining == 0 {
                    return Err(MemoryNodeStoreError::InvalidNode);
                }

                *remaining -= 1;
            }

            self.inner.write_node(page_id, node)
        }

        fn allocate_node(&self, node: &BPlusNode) -> Result<PageId, Self::Error> {
            if let Some(remaining) = self.fail_allocations_after.lock().as_mut() {
                if *remaining == 0 {
                    return Err(MemoryNodeStoreError::InvalidNode);
                }

                *remaining -= 1;
            }

            self.inner.allocate_node(node)
        }
    }
    fn tree_snapshot<S: NodeStore>(tree: &BPlusTree<S>) -> Vec<(Key, RecordId)>
    where
        S::Error: std::fmt::Debug,
    {
        tree.scan().unwrap()
    }
    #[test]
    fn failing_node_store_allows_normal_operations() {
        let store = FailingNodeStore {
            inner: MemoryNodeStore::new(),
            fail_writes_after: Mutex::new(None),
            fail_allocations_after: Mutex::new(None),
        };

        let tree = BPlusTree::new(store).unwrap();

        tree.insert(fixed_key(1), record_id(1)).unwrap();

        assert_eq!(tree.search(&fixed_key(1)).unwrap(), Some(record_id(1)));
    }
    #[test]
    fn insert_propagates_allocation_failure() {
        let store = FailingNodeStore {
            inner: MemoryNodeStore::new(),
            fail_writes_after: Mutex::new(None),
            fail_allocations_after: Mutex::new(Some(0)),
        };

        let tree = BPlusTree::new(store);

        assert!(tree.is_err());
    }
    #[test]
    fn insert_propagates_write_failure() {
        let store = FailingNodeStore {
            inner: MemoryNodeStore::new(),
            fail_writes_after: Mutex::new(None),
            fail_allocations_after: Mutex::new(None),
        };

        let mut tree = BPlusTree::new(store).unwrap();

        for i in 0..100 {
            tree.insert(fixed_key(i), record_id(i)).unwrap();
        }

        tree.store.fail_writes_after = Mutex::new(Some(0));

        let result = tree.insert(fixed_key(101), record_id(101));

        assert!(result.is_err());
    }
    #[test]
    fn failed_leaf_split_is_detectable() {
        let store = FailingNodeStore {
            inner: MemoryNodeStore::new(),
            fail_writes_after: Mutex::new(None),
            fail_allocations_after: Mutex::new(None),
        };

        let mut tree = BPlusTree::new(store).unwrap();

        for i in 0..10_000 {
            tree.insert(fixed_key(i), record_id(i)).unwrap();
        }

        let before = tree_snapshot(&tree);

        /*
         * Force the next write to fail.
         */
        tree.store.fail_writes_after = Mutex::new(Some(0));

        let result = tree.insert(fixed_key(10_001), record_id(10_001));

        assert!(result.is_err());

        /*
         * We deliberately don't assert equality yet.
         *
         * This test establishes whether the current implementation
         * leaves the tree unchanged or partially modified.
         */
        let after = tree_snapshot(&tree);

        println!("before entries: {}", before.len());

        println!("after entries: {}", after.len());
    }
    #[test]
    fn failed_parent_update_is_detectable() {
        let store = FailingNodeStore {
            inner: MemoryNodeStore::new(),
            fail_writes_after: Mutex::new(None),
            fail_allocations_after: Mutex::new(None),
        };

        let mut tree = BPlusTree::new(store).unwrap();

        /*
         * Force the first root leaf split.
         */
        for i in 0..5_000 {
            tree.insert(fixed_key(i), record_id(i)).unwrap();
        }

        let before = tree_snapshot(&tree);

        /*
         * The next write should fail.
         */
        tree.store.fail_writes_after = Mutex::new(Some(0));

        let result = tree.insert(fixed_key(5_001), record_id(5_001));

        assert!(result.is_err());
        let after = tree_snapshot(&tree);

        assert_eq!(
            before, after,
            "failed parent update left the tree partially modified"
        );

        let after = tree_snapshot(&tree);

        println!("before entries: {}", before.len());

        println!("after entries: {}", after.len());
    }
    #[test]
    fn failed_split_allocation_does_not_modify_original_leaf() {
        let store = FailingNodeStore {
            inner: MemoryNodeStore::new(),
            fail_writes_after: Mutex::new(None),
            fail_allocations_after: Mutex::new(None),
        };

        let mut tree = BPlusTree::new(store).unwrap();

        /*
         * Construct a leaf large enough to represent the
         * pre-split state.
         */
        let mut keys = Vec::new();
        let mut values = Vec::new();

        for i in 0..10_000u64 {
            let candidate_key = key(&format!("{i:08}-{}", "x".repeat(20)));

            let candidate_value = RecordId::new(PageId::new(i + 1), 0);

            keys.push(candidate_key);
            values.push(candidate_value);

            let candidate = BPlusNode::Leaf {
                keys: keys.clone(),
                values: values.clone(),
                next_leaf: None,
            };

            if candidate.encoded_len().unwrap() > PAGE_SIZE {
                keys.pop();
                values.pop();
                break;
            }
        }

        /*
         * Add one more entry so the input represents the
         * overflowing leaf that split_leaf() expects.
         */
        let overflow_index = keys.len() as u64;

        keys.push(key(&format!("{overflow_index:08}-{}", "y".repeat(20))));

        values.push(RecordId::new(PageId::new(100_000), 0));

        /*
         * The caller of split_leaf() must already own an
         * allocated page.
         */
        let original = BPlusNode::Leaf {
            keys: keys.clone(),
            values: values.clone(),
            next_leaf: None,
        };

        /*
         * Make sure the constructed node is valid.
         */
        assert!(original.validate().is_ok());

        /*
         * Store the overflowing leaf before enabling
         * allocation failure.
         */
        let page_id = tree.store.allocate_node(&original).unwrap();

        /*
         * Capture the original state.
         */
        let before = {
            let guard = tree.store.read_node(page_id).unwrap();
            guard.clone()
        };

        let before = before.read();
        /*
         * The next allocation must fail.
         */
        tree.store.fail_allocations_after = Mutex::new(Some(0));

        let result = tree.split_leaf(page_id, keys, values, None);

        assert!(
            result.is_err(),
            "split_leaf should propagate allocation failure"
        );

        /*
         * Allocation happens before the original page is
         * rewritten in split_leaf().
         *
         * Therefore the original leaf must remain unchanged.
         */
        let after = tree.store.read_node(page_id).unwrap();
        let after = after.read();
        assert_eq!(
            &*before, &*after,
            "allocation failure modified the original leaf"
        );
    }
    #[test]
    fn failed_insert_preserves_existing_search_results() {
        let store = FailingNodeStore {
            inner: MemoryNodeStore::new(),
            fail_writes_after: Mutex::new(None),
            fail_allocations_after: Mutex::new(None),
        };

        let mut tree = BPlusTree::new(store).unwrap();

        let key_count = 5_000;

        for i in 0..key_count {
            tree.insert(fixed_key(i), record_id(i)).unwrap();
        }

        tree.store.fail_writes_after = Mutex::new(Some(0));

        let result = tree.insert(fixed_key(key_count + 1), record_id(key_count + 1));

        assert!(result.is_err());

        /*
         * Existing records must remain searchable.
         */
        for i in 0..key_count {
            assert_eq!(
                tree.search(&fixed_key(i)).unwrap(),
                Some(record_id(i)),
                "existing key {i} disappeared after failed insert",
            );
        }
    }
    #[test]
    fn failed_insert_never_reports_success() {
        let store = FailingNodeStore {
            inner: MemoryNodeStore::new(),
            fail_writes_after: Mutex::new(Some(0)),
            fail_allocations_after: Mutex::new(None),
        };

        let tree = BPlusTree::new(store).unwrap();

        let result = tree.insert(fixed_key(1), record_id(1));

        assert!(result.is_err());
    }
    #[test]
    fn failed_split_preserves_tree_snapshot() {
        let store = FailingNodeStore {
            inner: MemoryNodeStore::new(),
            fail_writes_after: Mutex::new(None),
            fail_allocations_after: Mutex::new(None),
        };

        let mut tree = BPlusTree::new(store).unwrap();

        let mut keys = Vec::new();
        let mut values = Vec::new();

        for i in 0..10_000u64 {
            keys.push(key(&format!("{i:08}-{}", "x".repeat(20))));
            values.push(RecordId::new(PageId::new(i + 1), 0));

            let candidate = BPlusNode::Leaf {
                keys: keys.clone(),
                values: values.clone(),
                next_leaf: None,
            };

            if candidate.encoded_len().unwrap() > PAGE_SIZE {
                keys.pop();
                values.pop();
                break;
            }
        }

        // Add one entry that makes the leaf overflow.
        let overflow_index = keys.len() as u64;

        keys.push(key(&format!("{overflow_index:08}-{}", "y".repeat(20))));
        values.push(RecordId::new(PageId::new(100_000), 0));

        let overflowing_leaf = BPlusNode::Leaf {
            keys: keys.clone(),
            values: values.clone(),
            next_leaf: None,
        };

        assert!(
            overflowing_leaf.encoded_len().unwrap() > PAGE_SIZE,
            "test setup must create an overflowing leaf"
        );

        let page_id = tree
            .store
            .allocate_node(&BPlusNode::Leaf {
                keys: keys[..keys.len() - 1].to_vec(),
                values: values[..values.len() - 1].to_vec(),
                next_leaf: None,
            })
            .unwrap();

        let before = {
            let guard = tree.store.read_node(page_id).unwrap();
            guard.clone()
        };
        let before = before.read();

        tree.store.fail_allocations_after = Mutex::new(Some(0));

        let result = tree.split_leaf(page_id, keys, values, None);

        assert!(result.is_err());

        let after = tree.store.read_node(page_id).unwrap();
        let after = after.read();
        assert_eq!(
            &*before, &*after,
            "failed split allocation modified the original leaf"
        );
    }
    #[test]
    fn failed_split_write_preserves_existing_tree() {
        let store = FailingNodeStore {
            inner: MemoryNodeStore::new(),
            fail_writes_after: Mutex::new(None),
            fail_allocations_after: Mutex::new(None),
        };

        let mut tree = BPlusTree::new(store).unwrap();

        for i in 0..5_000usize {
            tree.insert(fixed_key(i), record_id(i)).unwrap();
        }

        let before = tree_snapshot(&tree);

        /*
         * Allow allocation, but fail the next write.
         */
        tree.store.fail_allocations_after = Mutex::new(None);
        tree.store.fail_writes_after = Mutex::new(Some(0));

        let result = tree.insert(fixed_key(5_000), record_id(5_000));

        assert!(result.is_err());

        let after = tree_snapshot(&tree);

        assert_eq!(before, after, "failed split write changed the visible tree");
    }
    #[test]
    fn stress_shuffled_insertions() {
        let store = MemoryNodeStore::new();
        let tree = BPlusTree::new(store).unwrap();

        let count = 50_000usize;

        let mut order: Vec<usize> = (0..count).collect();

        // Deterministic Fisher-Yates shuffle.
        let mut state = 0x1234_5678_9abc_def0u64;

        for i in (1..order.len()).rev() {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;

            let j = (state as usize) % (i + 1);
            order.swap(i, j);
        }

        for &i in &order {
            tree.insert(fixed_key(i), record_id(i)).unwrap();
        }

        assert_eq!(tree.scan().unwrap().len(), count);

        for i in 0..count {
            assert_eq!(tree.search(&fixed_key(i)).unwrap(), Some(record_id(i)));
        }

        assert_node_valid(&tree, *tree.root_page_id.read());
    }
    #[test]
    fn stress_reverse_insertions() {
        let store = MemoryNodeStore::new();
        let tree = BPlusTree::new(store).unwrap();

        let count = 30_000usize;

        for i in (0..count).rev() {
            tree.insert(fixed_key(i), record_id(i)).unwrap();
        }

        let entries = tree.scan().unwrap();

        assert_eq!(entries.len(), count);

        for (i, (key, record_id_value)) in entries.iter().enumerate() {
            assert_eq!(key, &fixed_key(i));
            assert_eq!(*record_id_value, record_id(i));
        }

        assert_node_valid(&tree, *tree.root_page_id.read());
    }
    #[test]
    fn stress_duplicate_insertions() {
        let store = MemoryNodeStore::new();
        let tree = BPlusTree::new(store).unwrap();

        let count = 20_000usize;

        for i in 0..count {
            tree.insert(fixed_key(i), record_id(i)).unwrap();
        }

        let before = tree.scan().unwrap();

        for i in 0..count {
            let result = tree.insert(fixed_key(i), RecordId::new(PageId::new(999_999), 999));

            assert!(
                matches!(result, Err(BPlusTreeError::DuplicateKey)),
                "expected duplicate error for key {i}"
            );
        }

        let after = tree.scan().unwrap();

        assert_eq!(before, after);

        assert_node_valid(&tree, *tree.root_page_id.read());
    }
    #[test]
    fn stress_missing_key_searches() {
        let store = MemoryNodeStore::new();
        let tree = BPlusTree::new(store).unwrap();

        let count = 20_000usize;

        for i in (0..count).step_by(2) {
            tree.insert(fixed_key(i), record_id(i)).unwrap();
        }

        for i in (0..count).step_by(2) {
            assert_eq!(tree.search(&fixed_key(i)).unwrap(), Some(record_id(i)));
        }

        for i in (1..count).step_by(2) {
            assert_eq!(
                tree.search(&fixed_key(i)).unwrap(),
                None,
                "missing key {i} unexpectedly found"
            );
        }

        assert_node_valid(&tree, *tree.root_page_id.read());
    }
    #[test]
    fn stress_scan_preserves_global_order() {
        let store = MemoryNodeStore::new();
        let tree = BPlusTree::new(store).unwrap();

        let count = 40_000usize;

        for i in (0..count).rev() {
            tree.insert(fixed_key(i), record_id(i)).unwrap();
        }

        let entries = tree.scan().unwrap();

        assert_eq!(entries.len(), count);

        for window in entries.windows(2) {
            assert!(
                window[0].0 < window[1].0,
                "scan order is not strictly increasing"
            );
        }

        for (i, (_, record_id_value)) in entries.iter().enumerate() {
            assert_eq!(*record_id_value, record_id(i));
        }

        assert_node_valid(&tree, *tree.root_page_id.read());
    }
    #[test]
    fn stress_leaf_chain_matches_scan() {
        let store = MemoryNodeStore::new();
        let tree = BPlusTree::new(store).unwrap();

        let count = 30_000usize;

        for i in (0..count).rev() {
            tree.insert(fixed_key(i), record_id(i)).unwrap();
        }

        let scanned = tree.scan().unwrap();

        assert_eq!(scanned.len(), count);

        let mut page_id = *tree.root_page_id.read();

        loop {
            let node = tree.store.read_node(page_id).unwrap();
            let node = node.read();
            match &*node {
                BPlusNode::Leaf { .. } => break,

                BPlusNode::Internal { children, .. } => {
                    page_id = children[0];
                }
            }
        }

        let mut chain_entries = Vec::new();

        loop {
            let node = tree.store.read_node(page_id).unwrap();

            let node = node.read();
            let BPlusNode::Leaf {
                keys,
                values,
                next_leaf,
            } = &*node
            else {
                panic!("leaf chain reached an internal node");
            };

            chain_entries.extend(keys.clone().into_iter().zip(values.clone()));

            match next_leaf {
                Some(next) => page_id = *next,
                None => break,
            }
        }

        assert_eq!(chain_entries, scanned);
    }
    #[test]
    fn concurrent_search_reads_are_safe() {
        use std::sync::Arc;
        use std::thread;

        let tree = BPlusTree::new(MemoryNodeStore::new() /* existing constructor args */).unwrap();

        // Insert a deterministic dataset.
        for i in 0..100 {
            let key = Key::new(format!("key-{i:03}").into_bytes());
            let record_id = RecordId::new(PageId::new(i), i as SlotId);

            tree.insert(key, record_id).unwrap();
        }

        let tree = Arc::new(tree);

        let mut handles = Vec::new();

        for _ in 0..8 {
            let tree = Arc::clone(&tree);

            handles.push(thread::spawn(move || {
                for i in 0..100 {
                    let key = Key::new(format!("key-{i:03}").into_bytes());

                    let result = tree.search(&key).unwrap();

                    assert_eq!(result, Some(RecordId::new(PageId::new(i), i as SlotId)));
                }
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }
    }
    #[test]
    fn concurrent_search_same_key_is_safe() {
        use std::sync::Arc;
        use std::thread;

        let tree = BPlusTree::new(MemoryNodeStore::new() /* existing constructor args */).unwrap();

        let key = Key::new(b"key-050".to_vec());
        let expected = RecordId::new(PageId::new(50), 50);
        tree.insert(key.clone(), expected).unwrap();
        let tree = Arc::new(tree);

        let mut handles = Vec::new();

        for _ in 0..16 {
            let tree = Arc::clone(&tree);
            let key = key.clone();

            handles.push(thread::spawn(move || {
                for _ in 0..1_000 {
                    let result = tree.search(&key).unwrap();

                    assert_eq!(result, Some(expected));
                }
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }
    }
    #[test]
    fn test_is_safe_for_insert_when_additional_bytes_fit() {
        let tree = BPlusTree::new(MemoryNodeStore::new() /* existing constructor args */).unwrap();
        let node = BPlusNode::Leaf {
            keys: vec![],
            values: vec![],
            next_leaf: None,
        };

        let encoded_len = node.encoded_len().unwrap();

        let additional_len = PAGE_SIZE - encoded_len;

        assert!(tree.is_safe_for_insert(&node, additional_len).unwrap());
    }
    #[test]
    fn test_is_safe_for_insert_when_additional_bytes_do_not_fit() {
        let tree = BPlusTree::new(MemoryNodeStore::new() /* existing constructor args */).unwrap();
        let node = BPlusNode::Leaf {
            keys: vec![],
            values: vec![],
            next_leaf: None,
        };

        let encoded_len = node.encoded_len().unwrap();

        let additional_len = PAGE_SIZE - encoded_len + 1;

        assert!(!tree.is_safe_for_insert(&node, additional_len).unwrap());
    }
    #[test]
    fn test_is_safe_for_insert_handles_size_overflow() {
        let tree = BPlusTree::new(MemoryNodeStore::new() /* existing constructor args */).unwrap();
        let node = BPlusNode::Leaf {
            keys: vec![],
            values: vec![],
            next_leaf: None,
        };

        assert!(!tree.is_safe_for_insert(&node, usize::MAX).unwrap());
    }
    #[test]
    fn test_is_safe_for_insert_with_zero_additional_bytes() {
        let tree = BPlusTree::new(MemoryNodeStore::new() /* existing constructor args */).unwrap();
        let node = BPlusNode::Leaf {
            keys: vec![],
            values: vec![],
            next_leaf: None,
        };

        assert!(tree.is_safe_for_insert(&node, 0).unwrap());
    }
    #[test]
    fn delete_existing_key_returns_record_id() {
        let tree = BPlusTree::new(MemoryNodeStore::new() /* existing constructor args */).unwrap();
        let record_id = RecordId::new(PageId::new(10), 1);

        tree.insert(Key::new(b"a".to_vec()), record_id).unwrap();

        assert_eq!(
            tree.delete(&Key::new(b"a".to_vec())).unwrap(),
            Some(record_id)
        );

        assert_eq!(tree.search(&Key::new(b"a".to_vec())).unwrap(), None);
    }
    #[test]
    fn delete_missing_key_returns_none() {
        let tree = BPlusTree::new(MemoryNodeStore::new() /* existing constructor args */).unwrap();
        assert_eq!(tree.delete(&Key::new(b"missing".to_vec())).unwrap(), None);
    }
    #[test]
    fn delete_last_key_leaves_empty_root_leaf() {
        let tree = BPlusTree::new(MemoryNodeStore::new() /* existing constructor args */).unwrap();
        let key = Key::new(b"a".to_vec());
        let value = RecordId::new(PageId::new(1), 1);

        tree.insert(key.clone(), value).unwrap();

        assert_eq!(tree.delete(&key).unwrap(), Some(value));

        assert!(tree.scan().unwrap().is_empty());
    }
    #[test]
    fn delete_preserves_remaining_entries() {
        let tree = BPlusTree::new(MemoryNodeStore::new() /* existing constructor args */).unwrap();
        for i in 0..20 {
            tree.insert(
                Key::new(format!("{i:02}").into_bytes()),
                RecordId::new(PageId::new(i), 0),
            )
            .unwrap();
        }

        tree.delete(&Key::new(b"05".to_vec())).unwrap();

        assert_eq!(
            tree.search(&Key::new(b"04".to_vec())).unwrap(),
            Some(RecordId::new(PageId::new(4), 0))
        );

        assert_eq!(tree.search(&Key::new(b"05".to_vec())).unwrap(), None);

        assert_eq!(
            tree.search(&Key::new(b"06".to_vec())).unwrap(),
            Some(RecordId::new(PageId::new(6), 0))
        );
    }
}
