use std::num::TryFromIntError;

use thiserror::Error;

use crate::page_id::PageId;

/// Error enum for this crate implementing `thiserror`crate
#[derive(Debug, Error)]
pub enum BPlusTreeError {
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
}

#[cfg(test)]
mod error_tests {
    use super::*;
    use crate::page_id::PageId;
    use std::error::Error;

    #[test]
    fn invalid_configuration_can_be_constructed() {
        let error = BPlusTreeError::InvalidConfiguration {
            reason: "node capacity must be non-zero".to_string(),
        };

        assert!(matches!(error, BPlusTreeError::InvalidConfiguration { .. }));
    }

    #[test]
    fn invalid_configuration_display_contains_reason() {
        let error = BPlusTreeError::InvalidConfiguration {
            reason: "node capacity must be non-zero".to_string(),
        };

        let message = error.to_string();

        assert!(message.contains("node capacity must be non-zero"));
    }

    #[test]
    fn corrupt_node_can_be_constructed() {
        let error = BPlusTreeError::CorruptNode {
            page_id: PageId::new(7),
            reason: "invalid child count".to_string(),
        };

        assert!(matches!(error, BPlusTreeError::CorruptNode { .. }));
    }

    #[test]
    fn corrupt_node_display_contains_page_id() {
        let error = BPlusTreeError::CorruptNode {
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
        let error = BPlusTreeError::CorruptNode {
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
        let error = BPlusTreeError::CorruptNode {
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

        assert_error::<BPlusTreeError>();
    }

    #[test]
    fn error_implements_debug() {
        let error = BPlusTreeError::InvalidConfiguration {
            reason: "invalid order".to_string(),
        };

        let debug = format!("{error:?}");

        assert!(debug.contains("InvalidConfiguration"));
    }

    #[test]
    fn corrupt_node_debug_contains_variant_name() {
        let error = BPlusTreeError::CorruptNode {
            page_id: PageId::new(3),
            reason: "bad node".to_string(),
        };

        let debug = format!("{error:?}");

        assert!(debug.contains("CorruptNode"));
    }

    #[test]
    fn distinct_corruption_reasons_are_preserved() {
        let first = BPlusTreeError::CorruptNode {
            page_id: PageId::new(1),
            reason: "invalid key ordering".to_string(),
        };

        let second = BPlusTreeError::CorruptNode {
            page_id: PageId::new(1),
            reason: "invalid child pointer".to_string(),
        };

        assert!(first.to_string().contains("invalid key ordering"));
        assert!(second.to_string().contains("invalid child pointer"));
    }
}
