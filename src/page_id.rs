use std::fmt::Debug;

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone, Copy, Hash)]
pub struct PageId(u64);

impl PageId {
    /// Returns new allocated `PageId` from provided Id.
    pub fn new(id: u64) -> Self {
        Self(id)
    }

    /// Returns the associated `PageId`
    pub fn get(&self) -> u64 {
        self.0
    }
}

#[cfg(test)]
mod page_id_tests {
    use super::*;
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    #[test]
    fn new_stores_page_id() {
        let page_id = PageId::new(42);

        assert_eq!(page_id.get(), 42);
    }

    #[test]
    fn zero_page_id_is_valid() {
        let page_id = PageId::new(0);

        assert_eq!(page_id.get(), 0);
    }

    #[test]
    fn max_page_id_is_preserved() {
        let page_id = PageId::new(u64::MAX);

        assert_eq!(page_id.get(), u64::MAX);
    }

    #[test]
    fn equal_page_ids_are_equal() {
        let a = PageId::new(10);
        let b = PageId::new(10);

        assert_eq!(a, b);
    }

    #[test]
    fn different_page_ids_are_not_equal() {
        let a = PageId::new(10);
        let b = PageId::new(20);

        assert_ne!(a, b);
    }

    #[test]
    fn page_id_ordering_is_correct() {
        let lower = PageId::new(10);
        let higher = PageId::new(20);

        assert!(lower < higher);
        assert!(higher > lower);
    }

    #[test]
    fn page_id_partial_cmp_matches_cmp() {
        let ids = [
            PageId::new(0),
            PageId::new(1),
            PageId::new(42),
            PageId::new(u64::MAX),
        ];

        for a in &ids {
            for b in &ids {
                assert_eq!(a.partial_cmp(b), Some(a.cmp(b)));
            }
        }
    }

    #[test]
    fn page_id_is_copy() {
        let original = PageId::new(42);
        let copied = original;

        assert_eq!(original, copied);
        assert_eq!(original.get(), 42);
        assert_eq!(copied.get(), 42);
    }

    #[test]
    fn equal_page_ids_produce_equal_hashes() {
        let a = PageId::new(42);
        let b = PageId::new(42);

        let mut hasher_a = DefaultHasher::new();
        let mut hasher_b = DefaultHasher::new();

        a.hash(&mut hasher_a);
        b.hash(&mut hasher_b);

        assert_eq!(hasher_a.finish(), hasher_b.finish());
    }

    #[test]
    fn page_ids_can_be_used_as_hash_map_keys() {
        use std::collections::HashMap;

        let mut pages = HashMap::new();

        pages.insert(PageId::new(1), "root");
        pages.insert(PageId::new(2), "leaf");

        assert_eq!(pages.get(&PageId::new(1)), Some(&"root"));
        assert_eq!(pages.get(&PageId::new(2)), Some(&"leaf"));
        assert_eq!(pages.get(&PageId::new(3)), None);
    }
}
