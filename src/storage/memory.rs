use std::collections::BTreeMap;
use std::sync::Arc;

use parking_lot::{Mutex, RwLock};
use thiserror::Error;

use crate::BPlusNode;
use crate::node_store::NodeStore;
use crate::page_id::PageId;

#[derive(Debug)]
pub struct MemoryNodeStore {
    tree: RwLock<BTreeMap<PageId, Arc<RwLock<BPlusNode>>>>,
    next_page_id: Mutex<u64>,
}

#[derive(Debug, Error)]
pub enum MemoryNodeStoreError {
    #[error("Page id not found page_id:{page_id}")]
    PageNotFound { page_id: u64 },
    #[error("Page id not allocated page_id:{page_id}")]
    PageNotAllocated { page_id: u64 },
    #[error("Node validation failded.")]
    InvalidNode,
    #[error("Allocator can't produce another new id")]
    PageIdExhausted,
}

impl MemoryNodeStore {
    pub fn new() -> Self {
        Self {
            tree: RwLock::new(BTreeMap::new()),
            next_page_id: Mutex::new(0),
        }
    }
}

impl Default for MemoryNodeStore {
    fn default() -> Self {
        Self::new()
    }
}

impl NodeStore for MemoryNodeStore {
    type Error = MemoryNodeStoreError;

    fn read_node(&self, page_id: PageId) -> Result<Arc<RwLock<BPlusNode>>, Self::Error> {
        let node = {
            let tree = self.tree.read();
            tree.get(&page_id)
                .cloned()
                .ok_or(MemoryNodeStoreError::PageNotFound {
                    page_id: page_id.get(),
                })?
        };
        Ok(node)
    }

    fn write_node(
        &self,
        page_id: PageId,
        node: &BPlusNode,
    ) -> Result<Arc<RwLock<BPlusNode>>, Self::Error> {
        node.validate()
            .map_err(|_| MemoryNodeStoreError::InvalidNode)?;

        let stored_node = {
            let tree = self.tree.read();

            tree.get(&page_id)
                .cloned()
                .ok_or(MemoryNodeStoreError::PageNotAllocated {
                    page_id: page_id.get(),
                })?
        };
        {
            let mut guard = stored_node.write();
            *guard = node.clone();
        }
        Ok(stored_node)
    }

    fn allocate_node(&self, node: &BPlusNode) -> Result<PageId, Self::Error> {
        node.validate()
            .map_err(|_| MemoryNodeStoreError::InvalidNode)?;
        let next_page_id = self
            .next_page_id
            .lock()
            .checked_add(1)
            .ok_or(MemoryNodeStoreError::PageIdExhausted)?;
        let new_page_id = PageId::new(*self.next_page_id.lock());
        self.tree
            .write()
            .insert(new_page_id, Arc::new(RwLock::new(node.clone())));
        *self.next_page_id.lock() = next_page_id;
        Ok(new_page_id)
    }
}

#[cfg(test)]
pub mod memory_node_store_tests {
    use super::*;
    use crate::key::Key;
    use crate::node_store::NodeStore;
    use crate::page_id::PageId;
    use crate::record_id::RecordId;

    use std::sync::Arc;

    fn leaf_node() -> BPlusNode {
        BPlusNode::Leaf {
            keys: vec![Key::new(b"alpha".to_vec()), Key::new(b"beta".to_vec())],
            values: vec![
                RecordId::new(PageId::new(10), 1),
                RecordId::new(PageId::new(20), 2),
            ],
            next_leaf: None,
        }
    }

    fn leaf_node_with_next() -> BPlusNode {
        BPlusNode::Leaf {
            keys: vec![Key::new(b"gamma".to_vec())],
            values: vec![RecordId::new(PageId::new(30), 3)],
            next_leaf: Some(PageId::new(99)),
        }
    }

    fn internal_node() -> BPlusNode {
        BPlusNode::Internal {
            keys: vec![Key::new(b"middle".to_vec())],
            children: vec![PageId::new(10), PageId::new(20)],
        }
    }

    fn invalid_leaf_node() -> BPlusNode {
        BPlusNode::Leaf {
            keys: vec![Key::new(b"alpha".to_vec()), Key::new(b"beta".to_vec())],
            values: vec![RecordId::new(PageId::new(10), 1)],
            next_leaf: None,
        }
    }

    fn invalid_internal_node() -> BPlusNode {
        BPlusNode::Internal {
            keys: vec![Key::new(b"middle".to_vec())],
            children: vec![PageId::new(10)],
        }
    }

    // -------------------------------------------------------------------------
    // Existing NodeStore contract
    // -------------------------------------------------------------------------

    #[test]
    fn allocate_node_returns_page_id() {
        let store = MemoryNodeStore::new();

        let page_id = store.allocate_node(&leaf_node()).unwrap();

        assert_eq!(page_id, PageId::new(0));
    }

    #[test]
    fn allocate_node_returns_unique_page_ids() {
        let store = MemoryNodeStore::new();

        let first = store.allocate_node(&leaf_node()).unwrap();
        let second = store.allocate_node(&internal_node()).unwrap();
        let third = store.allocate_node(&leaf_node_with_next()).unwrap();

        assert_ne!(first, second);
        assert_ne!(second, third);
        assert_ne!(first, third);
    }

    #[test]
    fn allocate_node_stores_leaf_node() {
        let store = MemoryNodeStore::new();
        let node = leaf_node();

        let page_id = store.allocate_node(&node).unwrap();

        assert_eq!(&*store.read_node(page_id).unwrap().read(), &node);
    }

    #[test]
    fn allocate_node_stores_internal_node() {
        let store = MemoryNodeStore::new();
        let node = internal_node();

        let page_id = store.allocate_node(&node).unwrap();

        assert_eq!(&*store.read_node(page_id).unwrap().read(), &node);
    }

    #[test]
    fn read_node_returns_exact_allocated_node() {
        let store = MemoryNodeStore::new();
        let node = leaf_node();

        let page_id = store.allocate_node(&node).unwrap();
        let read_node = store.read_node(page_id).unwrap();
        let read_node = read_node.read();

        assert_eq!(&*read_node, &node);
    }

    #[test]
    fn read_node_returns_independent_clone() {
        let store = MemoryNodeStore::new();
        let node = leaf_node();

        let page_id = store.allocate_node(&node).unwrap();

        let first_read = store.read_node(page_id).unwrap();
        let first_read = first_read.read();
        let second_read = store.read_node(page_id).unwrap();
        let second_read = second_read.read();

        assert_eq!(&*first_read, &*second_read);
        assert_eq!(&*store.read_node(page_id).unwrap().read(), &node);
    }

    #[test]
    fn read_node_rejects_missing_page() {
        let store = MemoryNodeStore::new();

        assert!(store.read_node(PageId::new(999)).is_err());
    }

    #[test]
    fn write_node_overwrites_existing_node() {
        let store = MemoryNodeStore::new();

        let page_id = store.allocate_node(&leaf_node()).unwrap();
        let replacement = internal_node();

        let _lock = store.write_node(page_id, &replacement).unwrap();
        let read_node = store.read_node(page_id).unwrap();
        let read_node = read_node.read();
        assert_eq!(&*read_node, &replacement);
    }

    #[test]
    fn write_node_rejects_unallocated_page() {
        let store = MemoryNodeStore::new();

        assert!(store.write_node(PageId::new(999), &leaf_node()).is_err());
    }

    #[test]
    fn write_node_rejects_invalid_leaf_without_overwriting() {
        let store = MemoryNodeStore::new();
        let original = leaf_node();

        let page_id = store.allocate_node(&original).unwrap();

        assert!(store.write_node(page_id, &invalid_leaf_node()).is_err());
        let read_node = store.read_node(page_id).unwrap();
        let read_node = read_node.read();

        assert_eq!(&*read_node, &original);
    }

    #[test]
    fn write_node_rejects_invalid_internal_without_overwriting() {
        let store = MemoryNodeStore::new();
        let original = internal_node();

        let page_id = store.allocate_node(&original).unwrap();

        assert!(store.write_node(page_id, &invalid_internal_node()).is_err());
        let read_node = store.read_node(page_id).unwrap();
        let read_node = read_node.read();

        assert_eq!(&*read_node, &original);
    }

    #[test]
    fn allocate_node_rejects_invalid_leaf() {
        let store = MemoryNodeStore::new();

        assert!(store.allocate_node(&invalid_leaf_node()).is_err());
        assert!(store.tree.read().is_empty());
    }

    #[test]
    fn allocate_node_rejects_invalid_internal() {
        let store = MemoryNodeStore::new();

        assert!(store.allocate_node(&invalid_internal_node()).is_err());
        assert!(store.tree.read().is_empty());
    }

    #[test]
    fn failed_allocation_does_not_consume_page_id() {
        let store = MemoryNodeStore::new();

        assert!(store.allocate_node(&invalid_leaf_node()).is_err());

        let page_id = store.allocate_node(&leaf_node()).unwrap();

        assert_eq!(page_id, PageId::new(0));
    }

    #[test]
    fn failed_write_does_not_change_stored_node() {
        let store = MemoryNodeStore::new();
        let original = leaf_node();

        let page_id = store.allocate_node(&original).unwrap();

        assert!(
            store
                .write_node(PageId::new(500), &internal_node())
                .is_err()
        );
        let read_node = store.read_node(page_id).unwrap();
        let read_node = read_node.read();

        assert_eq!(&*read_node, &original);
    }

    #[test]
    fn allocate_node_rejects_page_id_overflow() {
        let store = MemoryNodeStore::new();

        *store.next_page_id.lock() = u64::MAX;

        let result = store.allocate_node(&internal_node());

        assert!(result.is_err());
        assert!(store.tree.read().is_empty());
        assert_eq!(*store.next_page_id.lock(), u64::MAX);
    }

    #[test]
    fn read_node_supports_max_page_id() {
        let store = MemoryNodeStore::new();
        let node = leaf_node();

        store
            .tree
            .write()
            .insert(PageId::new(u64::MAX), Arc::new(RwLock::new(node.clone())));
        let read_node = store.read_node(PageId::new(u64::MAX)).unwrap();
        let read_node = read_node.read();

        assert_eq!(&*read_node, &node);
    }

    #[test]
    fn write_node_supports_max_page_id() {
        let store = MemoryNodeStore::new();

        store
            .tree
            .write()
            .insert(PageId::new(u64::MAX), Arc::new(RwLock::new(leaf_node())));

        let replacement = internal_node();

        let _lock = store
            .write_node(PageId::new(u64::MAX), &replacement)
            .unwrap();
        let read_node = store.read_node(PageId::new(u64::MAX)).unwrap();
        let read_node = read_node.read();

        assert_eq!(&*read_node, &replacement);
    }
    // -------------------------------------------------------------------------
    // Page guard API through the exposed NodeStore methods
    // -------------------------------------------------------------------------

    #[test]
    fn read_node_returns_guard_to_existing_node() {
        let store = MemoryNodeStore::new();
        let node = leaf_node();

        let page_id = store.allocate_node(&node).unwrap();

        let guard = store.read_node(page_id).unwrap();
        let guard = guard.read();

        assert_eq!(&*guard, &node);
    }

    #[test]
    fn write_node_returns_guard_to_existing_node() {
        let store = MemoryNodeStore::new();
        let node = leaf_node();

        let page_id = store.allocate_node(&node).unwrap();

        let guard = store.write_node(page_id, &node).unwrap();
        let guard = guard.read();

        assert_eq!(&*guard, &node);
    }

    #[test]
    fn write_node_guard_can_modify_stored_node() {
        let store = MemoryNodeStore::new();

        let original = leaf_node();
        let replacement = internal_node();

        let page_id = store.allocate_node(&original).unwrap();

        {
            let guard = store.write_node(page_id, &replacement).unwrap();
            let mut guard = guard.write();

            assert_eq!(&*guard, &replacement);

            *guard = leaf_node_with_next();
        }

        let guard = store.read_node(page_id).unwrap();

        let guard = guard.read();

        assert_eq!(&*guard, &leaf_node_with_next());
    }

    #[test]
    fn write_node_modification_is_visible_through_read_node() {
        let store = MemoryNodeStore::new();

        let original = leaf_node();
        let replacement = internal_node();

        let page_id = store.allocate_node(&original).unwrap();

        {
            let guard = store.write_node(page_id, &replacement).unwrap();

            let mut guard = guard.write();
            *guard = replacement.clone();
        }

        let guard = store.read_node(page_id).unwrap();
        let guard = guard.read();

        assert_eq!(&*guard, &replacement);
    }

    #[test]
    fn multiple_readers_can_hold_same_page() {
        let store = MemoryNodeStore::new();
        let node = leaf_node();

        let page_id = store.allocate_node(&node).unwrap();

        let first = store.read_node(page_id).unwrap();
        let first = first.read();
        let second = store.read_node(page_id).unwrap();
        let second = second.read();

        assert_eq!(&*first, &node);
        assert_eq!(&*second, &node);
    }

    #[test]
    fn write_node_preserves_page_lock_identity() {
        let store = MemoryNodeStore::new();

        let original = leaf_node();
        let replacement = internal_node();

        let page_id = store.allocate_node(&original).unwrap();

        let before = store
            .tree
            .read()
            .get(&page_id)
            .expect("allocated page must exist")
            .clone();

        {
            let _guard = store.write_node(page_id, &replacement).unwrap();
        }

        let after = store
            .tree
            .read()
            .get(&page_id)
            .expect("allocated page must exist")
            .clone();

        assert!(
            Arc::ptr_eq(&before, &after),
            "write_node must not replace the page Arc"
        );

        assert_eq!(&*after.read(), &replacement);
    }

    #[test]
    fn different_pages_have_independent_locks() {
        let store = MemoryNodeStore::new();

        let page_a = store.allocate_node(&leaf_node()).unwrap();
        let page_b = store.allocate_node(&internal_node()).unwrap();

        let lock_a = store
            .tree
            .read()
            .get(&page_a)
            .expect("page A must exist")
            .clone();

        let lock_b = store
            .tree
            .read()
            .get(&page_b)
            .expect("page B must exist")
            .clone();

        assert!(
            !Arc::ptr_eq(&lock_a, &lock_b),
            "different pages must have different lock objects"
        );
    }
    #[test]
    fn multiple_reads_observe_same_node() {
        let store = MemoryNodeStore::new();
        let node = leaf_node();

        let page_id = store.allocate_node(&node).unwrap();

        let first_read = store.read_node(page_id).unwrap();
        let first_read = first_read.read();
        let second_read = store.read_node(page_id).unwrap();
        let second_read = second_read.read();

        assert_eq!(&*first_read, &*second_read);
        assert_eq!(&*first_read, &node);
    }
    #[test]
    fn write_node_works_through_shared_store_reference() {
        let mut store = MemoryNodeStore::new();

        let page_id = store.allocate_node(&leaf_node()).unwrap();

        let store_ref = &store;

        let replacement = internal_node();

        {
            let guard = store_ref.write_node(page_id, &replacement).unwrap();
            let guard = guard.write();
            assert_eq!(&*guard, &replacement);
        }

        let guard = store.read_node(page_id).unwrap();
        let guard = guard.read();

        assert_eq!(&*guard, &replacement);
    }
}
