use crate::BPlusNode;
use crate::page_id::PageId;

pub trait NodeStore {
    type Error;

    /// Returns the node stored at `page_id`, or an error if it cannot be read
    fn read_node(&self, page_id: PageId) -> Result<BPlusNode, Self::Error>;

    /// Replaces the node at an existing `page_id`; it does not allocate a new page
    fn write_node(&mut self, page_id: PageId, node: &BPlusNode) -> Result<(), Self::Error>;

    /// Allocates a fresh page, stores the node, and returns its `PageId`
    fn allocate_node(&mut self, node: &BPlusNode) -> Result<PageId, Self::Error>;
}
