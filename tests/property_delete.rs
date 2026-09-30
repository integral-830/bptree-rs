use std::collections::BTreeMap;

use bptree::key::Key;
use bptree::page_id::PageId;
use bptree::record_id::RecordId;
use bptree::storage::memory::MemoryNodeStore;
use bptree::tree::BPlusTree;
use proptest::prelude::*;

fn key_from_u64(value: u64) -> Key {
    Key::new(value.to_be_bytes().to_vec())
}

fn record_id_from_u64(value: u64) -> RecordId {
    RecordId::new(PageId::new(value), 0)
}

#[derive(Debug, Clone)]
enum Operation {
    Insert(u64),
    Delete(u64),
}

fn operation_strategy() -> impl Strategy<Value = Operation> {
    prop_oneof![
        (0u64..128).prop_map(Operation::Insert),
        (0u64..128).prop_map(Operation::Delete),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 256,
        max_shrink_iters: 10_000,
        ..ProptestConfig::default()
    })]

    #[test]
    fn mixed_insert_delete_matches_btreemap(
        operations in prop::collection::vec(operation_strategy(), 1..500)
    ) {
        let store = MemoryNodeStore::new();
        let tree = BPlusTree::new(store)
            .expect("failed to create B+Tree");

        let mut reference = BTreeMap::<Key, RecordId>::new();

        for operation in operations {
            match operation {
                Operation::Insert(value) => {
                    let key = key_from_u64(value);
                    let record_id = record_id_from_u64(value);

                    let tree_result =
                        tree.insert(key.clone(), record_id);

                    let reference_result =
                        reference.insert(key.clone(), record_id);

                    match reference_result {
                        Some(_) => {
                            assert!(
                                tree_result.is_err(),
                                "tree accepted duplicate key"
                            );
                        }
                        None => {
                            assert!(
                                tree_result.is_ok(),
                                "tree rejected valid insert"
                            );
                        }
                    }
                }

                Operation::Delete(value) => {
                    let key = key_from_u64(value);

                    let expected = reference.remove(&key);
                    let actual = tree
                        .delete(&key)
                        .expect("delete failed");

                    assert_eq!(actual, expected);
                }
            }

            for (key, expected) in &reference {
                let actual = tree
                    .search(key)
                    .expect("search failed");

                assert_eq!(
                    actual,
                    Some(*expected),
                    "search mismatch for key {key:?}"
                );
            }

            let expected_scan: Vec<(Key, RecordId)> = reference
                .iter()
                .map(|(key, record_id)| {
                    (key.clone(), *record_id)
                })
                .collect();

            let actual_scan =
                tree.scan().expect("scan failed");

            assert_eq!(actual_scan, expected_scan);
        }
    }
}
