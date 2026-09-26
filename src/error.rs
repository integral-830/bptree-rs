use std::num::TryFromIntError;

use thiserror::Error;

use crate::page_id::PageId;

/// Error enum for `BPlusTreeNode` implementing `thiserror`crate
#[derive(Debug, Error)]
pub enum BPlusNodeError {
    /// enum variant invalid Configuration error
    #[error("Invalid Configuration reason: {reason}")]
    InvalidConfiguration { reason: String },
    /// enum variant for node validation while `node_decoding`
    #[error("Node {page_id:?} is corrupted reason: {reason}")]
    CorruptNode { page_id: PageId, reason: String },
    /// enum variant for node validation while `node_encoding`
    #[error("Node is invalid reason: {reason}")]
    InvalidNode { reason: String },
    /// enum variant for node validation while `node_encoding`
    #[error("Node is invalid reason: {reason}")]
    NodeTooLarge { reason: String },
    /// enum variant for node encoding err
    #[error(" Encoding Node {page_id:?} failed reason: {reason}")]
    NodeEncodingErr { page_id: PageId, reason: String },
    /// Index conversion error variant
    #[error("Index bounds overflowed: {0}")]
    IndexOverflow(#[from] TryFromIntError),
    /// Size overflowed error variant
    #[error("Size overflowed")]
    SizeOverflow,
    ///Invalid Magic err variant
    #[error("Invalid file header: {reason}")]
    InvalidHeader { reason: String },
    /// enum variant for node encoding err
    #[error(" Decoding Node failed reason: {reason}")]
    NodeDecodingErr { reason: String },
    /// enum variant for node encoding err
    #[error(" Node slicing failed ")]
    NodeSlicingErr,
    /// enum variant for node encoding err
    #[error(" Invalid Node type accepted:(0 or 1)")]
    InvalidNodeType,
    /// Invalid `next_leaf_value` err variant
    #[error(" Invalid next_leaf_value value expected: (0 or 1) found: {found}")]
    InvalidNextLeafValue { found: usize },
    /// Invalid payload err variant
    #[error(" Invalid payload length expected: 4096 found: {found}")]
    InvalidPayload { found: usize },
}

/// Error enum for `BPlusTree`
#[derive(Debug, Error)]
pub enum BPlusTreeError<E> {
    #[error("node store error: {0}")]
    Storage(#[from] E),
    #[error("node error: {0}")]
    Node(BPlusNodeError),
    #[error("invalid tree state: {0}")]
    InvalidTreeState(&'static str),
    #[error("duplicate key")]
    DuplicateKey,
    #[error("leaf node is full")]
    LeafFull,
}

#[cfg(test)]
mod error_tests {
    use super::*;
    use crate::page_id::PageId;
    use std::error::Error;

    #[test]
    fn invalid_configuration_can_be_constructed() {
        let error = BPlusNodeError::InvalidConfiguration {
            reason: "node capacity must be non-zero".to_string(),
        };

        assert!(matches!(error, BPlusNodeError::InvalidConfiguration { .. }));
    }

    #[test]
    fn invalid_configuration_display_contains_reason() {
        let error = BPlusNodeError::InvalidConfiguration {
            reason: "node capacity must be non-zero".to_string(),
        };

        let message = error.to_string();

        assert!(message.contains("node capacity must be non-zero"));
    }

    #[test]
    fn corrupt_node_can_be_constructed() {
        let error = BPlusNodeError::CorruptNode {
            page_id: PageId::new(7),
            reason: "invalid child count".to_string(),
        };

        assert!(matches!(error, BPlusNodeError::CorruptNode { .. }));
    }

    #[test]
    fn corrupt_node_display_contains_page_id() {
        let error = BPlusNodeError::CorruptNode {
            page_id: PageId::new(7),
            reason: "invalid child count".to_string(),
        };

        let message = error.to_string();

        assert!(
            message.contains("7"),
            "error message should include the page ID: {message}"
        );
    }

    #[test]
    fn corrupt_node_display_contains_reason() {
        let error = BPlusNodeError::CorruptNode {
            page_id: PageId::new(7),
            reason: "invalid child count".to_string(),
        };

        let message = error.to_string();

        assert!(
            message.contains("invalid child count"),
            "error message should include the corruption reason: {message}"
        );
    }

    #[test]
    fn corrupt_node_display_contains_page_id_and_reason() {
        let error = BPlusNodeError::CorruptNode {
            page_id: PageId::new(42),
            reason: "malformed separator keys".to_string(),
        };

        let message = error.to_string();

        assert!(message.contains("42"));
        assert!(message.contains("malformed separator keys"));
    }

    #[test]
    fn error_implements_std_error() {
        fn assert_error<T: Error>() {}

        assert_error::<BPlusNodeError>();
    }

    #[test]
    fn error_implements_debug() {
        let error = BPlusNodeError::InvalidConfiguration {
            reason: "invalid order".to_string(),
        };

        let debug = format!("{error:?}");

        assert!(debug.contains("InvalidConfiguration"));
    }

    #[test]
    fn corrupt_node_debug_contains_variant_name() {
        let error = BPlusNodeError::CorruptNode {
            page_id: PageId::new(3),
            reason: "bad node".to_string(),
        };

        let debug = format!("{error:?}");

        assert!(debug.contains("CorruptNode"));
    }

    #[test]
    fn distinct_corruption_reasons_are_preserved() {
        let first = BPlusNodeError::CorruptNode {
            page_id: PageId::new(1),
            reason: "invalid key ordering".to_string(),
        };

        let second = BPlusNodeError::CorruptNode {
            page_id: PageId::new(1),
            reason: "invalid child pointer".to_string(),
        };

        assert!(first.to_string().contains("invalid key ordering"));
        assert!(second.to_string().contains("invalid child pointer"));
    }
}
