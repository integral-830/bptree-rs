use bptree::key::Key;
use bptree::page_id::PageId;
use bptree::record_id::RecordId;
use bptree::slotted_page::SlotId;
use bptree::storage::memory::MemoryNodeStore;
use bptree::tree::BPlusTree;

#[test]
fn concurrent_search_latch_coupling_stress() {
    use std::sync::Arc;
    use std::thread;

    let store = MemoryNodeStore::new();
    let tree = Arc::new(BPlusTree::new(store).unwrap());

    let key_count = 1_000;

    for i in 0..key_count {
        let key = Key::new(format!("key-{i:04}").into_bytes());
        let record_id = RecordId::new(PageId::new(i as u64), i as u16);

        tree.insert(key, record_id).unwrap();
    }

    let thread_count = 8;
    let iterations = 5_000;

    let mut handles = Vec::with_capacity(thread_count);

    for thread_id in 0..thread_count {
        let tree = Arc::clone(&tree);

        handles.push(thread::spawn(move || {
            for iteration in 0..iterations {
                let key_index = (thread_id * iterations + iteration) % key_count;

                let key = Key::new(format!("key-{key_index:04}").into_bytes());

                let result = tree.search(&key).unwrap();

                assert!(result.is_some(), "existing key was not found");
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
