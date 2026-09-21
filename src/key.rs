#[derive(PartialEq, Eq, PartialOrd, Ord)]
pub struct Key {
    bytes: Vec<u8>,
}

impl Key {
    /// Create new key from owned bytes
    pub fn new(bytes: Vec<u8>) -> Self {
        Self { bytes }
    }

    /// Returns the key as borrowed raw bytes
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
}

#[cfg(test)]
mod keys_tests {
    use super::*;
    use std::cmp::Ordering;

    #[test]
    fn new_preserves_bytes() {
        let key = Key::new(vec![1, 2, 3]);

        assert_eq!(key.as_bytes(), &[1, 2, 3]);
    }

    #[test]
    fn new_accepts_empty_key() {
        let key = Key::new(vec![]);

        assert!(key.as_bytes().is_empty());
    }

    #[test]
    fn equal_keys_are_equal() {
        let a = Key::new(vec![1, 2, 3]);
        let b = Key::new(vec![1, 2, 3]);

        assert!(a == b);
        assert_eq!(a.cmp(&b), Ordering::Equal);
    }

    #[test]
    fn different_keys_are_not_equal() {
        let a = Key::new(vec![1, 2]);
        let b = Key::new(vec![1, 3]);

        assert!(a != b);
    }

    #[test]
    fn lexicographic_ordering_uses_first_differing_byte() {
        let a = Key::new(vec![1, 2, 3]);
        let b = Key::new(vec![1, 3, 0]);

        assert_eq!(a.cmp(&b), Ordering::Less);
        assert_eq!(b.cmp(&a), Ordering::Greater);
    }

    #[test]
    fn shorter_matching_prefix_is_less() {
        let a = Key::new(vec![1, 2]);
        let b = Key::new(vec![1, 2, 0]);

        assert_eq!(a.cmp(&b), Ordering::Less);
        assert_eq!(b.cmp(&a), Ordering::Greater);
    }

    #[test]
    fn empty_key_is_less_than_non_empty_key() {
        let empty = Key::new(vec![]);
        let non_empty = Key::new(vec![0]);

        assert_eq!(empty.cmp(&non_empty), Ordering::Less);
    }

    #[test]
    fn partial_cmp_matches_cmp() {
        let keys = [
            Key::new(vec![]),
            Key::new(vec![1]),
            Key::new(vec![1, 0]),
            Key::new(vec![2]),
        ];

        for a in &keys {
            for b in &keys {
                assert_eq!(a.partial_cmp(b), Some(a.cmp(b)));
            }
        }
    }

    #[test]
    fn cmp_equal_iff_keys_are_equal() {
        let keys = [
            Key::new(vec![]),
            Key::new(vec![1]),
            Key::new(vec![1, 2]),
            Key::new(vec![2]),
            Key::new(vec![1, 2]),
        ];

        for a in &keys {
            for b in &keys {
                assert_eq!(a.cmp(b) == Ordering::Equal, a == b);
            }
        }
    }

    #[test]
    fn ordering_is_transitive() {
        let a = Key::new(vec![1]);
        let b = Key::new(vec![1, 1]);
        let c = Key::new(vec![2]);

        assert!(a < b);
        assert!(b < c);
        assert!(a < c);
    }
}
