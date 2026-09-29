use parking_lot::{RwLockReadGuard, RwLockWriteGuard};

use crate::BPlusNode;
use crate::page_id::PageId;

pub type ReadGuard<'a> = RwLockReadGuard<'a, BPlusNode>;
pub type WriteGuard<'a> = RwLockWriteGuard<'a, BPlusNode>;

pub trait NodeStore {
    type Error;

    /// Returns the node stored at `page_id`, or an error if it cannot be read
    fn read_node(&self, page_id: PageId) -> Result<RwLockReadGuard<'_, BPlusNode>, Self::Error>;

    /// Replaces the node at an existing `page_id`; it does not allocate a new page
    fn write_node(
        &self,
        page_id: PageId,
        node: &BPlusNode,
    ) -> Result<RwLockWriteGuard<'_, BPlusNode>, Self::Error>;

    /// Allocates a fresh page, stores the node, and returns its `PageId`
    fn allocate_node(&mut self, node: &BPlusNode) -> Result<PageId, Self::Error>;
}
