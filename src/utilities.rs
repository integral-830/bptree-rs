use std::collections::HashSet;

pub fn has_duplicates<T: Eq + std::hash::Hash>(slice: &[T]) -> bool {
    let mut seen = HashSet::new();
    for item in slice {
        if !seen.insert(item) {
            return true; // Found a duplicate early
        }
    }
    false
}
