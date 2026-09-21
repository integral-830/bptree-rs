use crate::page_id::PageId;
use crate::slotted_page::SlotId;

#[derive(Debug, PartialEq, Eq, Clone, Copy, Hash)]
pub struct RecordId {
    page_id: PageId,
    slot_id: SlotId,
}

impl RecordId {
    /// Create and returns new allocated `RecordId`for a `PageId`nd `SlotId`
    pub fn new(page_id: PageId, slot_id: SlotId) -> Self {
        Self { page_id, slot_id }
    }

    /// Returns the allocated `PageId`
    pub fn page_id(&self) -> PageId {
        self.page_id
    }

    /// Returns the allocated `SlotId`
    pub fn slot_id(&self) -> SlotId {
        self.slot_id
    }
}

#[cfg(test)]
mod record_id_tests {
    use super::*;
    use std::collections::HashMap;
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    fn make_record_id(page_id: u64, slot_id: u16) -> RecordId {
        RecordId::new(PageId::new(page_id), slot_id)
    }

    #[test]
    fn new_stores_page_and_slot_ids() {
        let record_id = make_record_id(42, 7);

        assert_eq!(record_id.page_id(), PageId::new(42));
        assert_eq!(record_id.slot_id(), 7);
    }

    #[test]
    fn page_id_accessor_returns_correct_page() {
        let record_id = make_record_id(10, 5);

        assert_eq!(record_id.page_id(), PageId::new(10));
    }

    #[test]
    fn slot_id_accessor_returns_correct_slot() {
        let record_id = make_record_id(10, 5);

        assert_eq!(record_id.slot_id(), 5);
    }

    #[test]
    fn zero_page_and_slot_ids_are_preserved() {
        let record_id = make_record_id(0, 0);

        assert_eq!(record_id.page_id(), PageId::new(0));
        assert_eq!(record_id.slot_id(), 0);
    }

    #[test]
    fn maximum_page_and_slot_ids_are_preserved() {
        let record_id = make_record_id(u64::MAX, u16::MAX);

        assert_eq!(record_id.page_id(), PageId::new(u64::MAX));
        assert_eq!(record_id.slot_id(), u16::MAX);
    }

    #[test]
    fn identical_record_ids_are_equal() {
        let a = make_record_id(3, 8);
        let b = make_record_id(3, 8);

        assert_eq!(a, b);
    }

    #[test]
    fn different_page_ids_make_record_ids_unequal() {
        let a = make_record_id(3, 8);
        let b = make_record_id(4, 8);

        assert_ne!(a, b);
    }

    #[test]
    fn different_slot_ids_make_record_ids_unequal() {
        let a = make_record_id(3, 8);
        let b = make_record_id(3, 9);

        assert_ne!(a, b);
    }

    #[test]
    fn record_id_is_copy() {
        let original = make_record_id(12, 6);
        let copied = original;

        assert_eq!(original, copied);
        assert_eq!(copied.page_id(), PageId::new(12));
        assert_eq!(copied.slot_id(), 6);
    }

    #[test]
    fn cloning_record_id_preserves_value() {
        let original = make_record_id(12, 6);
        let cloned = original;

        assert_eq!(original, cloned);
    }

    #[test]
    fn equal_record_ids_produce_equal_hashes() {
        let a = make_record_id(42, 7);
        let b = make_record_id(42, 7);

        let mut hasher_a = DefaultHasher::new();
        let mut hasher_b = DefaultHasher::new();

        a.hash(&mut hasher_a);
        b.hash(&mut hasher_b);

        assert_eq!(hasher_a.finish(), hasher_b.finish());
    }

    #[test]
    fn record_id_can_be_used_as_hash_map_key() {
        let mut records = HashMap::new();

        let first = make_record_id(1, 2);
        let second = make_record_id(1, 3);

        records.insert(first, "alice");
        records.insert(second, "bob");

        assert_eq!(records.get(&first), Some(&"alice"));
        assert_eq!(records.get(&second), Some(&"bob"));
        assert_eq!(records.get(&make_record_id(1, 4)), None);
    }

    #[test]
    fn inserting_same_record_id_replaces_existing_value() {
        let mut records = HashMap::new();
        let record_id = make_record_id(5, 10);

        records.insert(record_id, "old");
        records.insert(record_id, "new");

        assert_eq!(records.len(), 1);
        assert_eq!(records.get(&record_id), Some(&"new"));
    }

    #[test]
    fn debug_format_includes_identifier_values() {
        let record_id = make_record_id(42, 7);
        let formatted = format!("{record_id:?}");

        assert!(formatted.contains("42"));
        assert!(formatted.contains("7"));
    }
}
